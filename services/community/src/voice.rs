//! community-voice/1.0: voice channels. Who is in each room, what they share,
//! the keys they hand each other, invitations, moderation, and the answers to
//! the forwarding service's "may this session publish that?".
//!
//! Rooms live in memory: a restart empties them, and everyone's app joins again.
//! Nothing here touches the network; `server.rs` sends what it says to send,
//! over a member's live connection where there is one.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::community::{problem, Community};
use crate::store::{Member, Role};

pub const PIURI: &str = "https://wyvrn.app/community-voice/1.0";
pub const WATCH_PRESENCE: &str = "https://wyvrn.app/community-voice/1.0/watch-presence";
pub const PRESENCE: &str = "https://wyvrn.app/community-voice/1.0/presence";
pub const JOIN: &str = "https://wyvrn.app/community-voice/1.0/join";
pub const JOINED: &str = "https://wyvrn.app/community-voice/1.0/joined";
pub const LEAVE: &str = "https://wyvrn.app/community-voice/1.0/leave";
pub const STATE: &str = "https://wyvrn.app/community-voice/1.0/state";
pub const KEYS: &str = "https://wyvrn.app/community-voice/1.0/keys";
pub const INVITE: &str = "https://wyvrn.app/community-voice/1.0/invite";
pub const RING: &str = "https://wyvrn.app/community-voice/1.0/ring";
pub const SET_PREFERENCES: &str = "https://wyvrn.app/community-voice/1.0/set-preferences";
pub const PREFERENCES: &str = "https://wyvrn.app/community-voice/1.0/preferences";
pub const MODERATE: &str = "https://wyvrn.app/community-voice/1.0/moderate";

/// How long someone in a room may go without saying anything (`state`, or any
/// message of this protocol) before they are taken to have gone.
pub const SILENT_FOR: i64 = 60;
/// How often the forwarding service asks again what a session may do, and how
/// long each answer lasts if it can't ask.
pub const REVALIDATE_SECONDS: u64 = 15;
pub const GRANT_SECONDS: i64 = 60;
/// As many as one `invite` asks.
const MAX_INVITEES: usize = 10;
/// How long a ring lasts.
const RING_SECONDS: i64 = 120;

/// What someone in a room is doing, as their `state` last said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sharing {
    pub camera: bool,
    pub screen: bool,
    pub avatar: bool,
}

#[derive(Debug, Clone)]
pub struct Participant {
    /// Opaque: what the forwarding service knows them by.
    pub id: String,
    /// What their app gives the forwarding service.
    pub token: String,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub sharing: Sharing,
    pub seen: i64,
}

#[derive(Debug, Default)]
struct Room {
    /// By DID.
    participants: BTreeMap<String, Participant>,
}

/// A member's live connection (community-voice/1.0's Connectivity): where
/// presence goes, and what reaches them fastest.
struct Live {
    did: String,
    watching: bool,
}

#[derive(Default)]
struct Inner {
    rooms: HashMap<String, Room>,
    live: HashMap<u64, Live>,
}

/// Every voice room of the community, and who holds a live connection.
#[derive(Default)]
pub struct Rooms {
    inner: Mutex<Inner>,
}

/// What handling a message came to.
#[derive(Debug, Default)]
pub struct VoiceOutcome {
    pub reply: Option<Value>,
    /// Messages for members, a DID each: over their live connection if they hold
    /// one, through DIDComm otherwise.
    pub notices: Vec<(String, Value)>,
    /// Messages for live connections only (presence).
    pub live: Vec<(u64, Value)>,
    /// Whether something was posted (an invitation for someone who won't be rung).
    pub posted: bool,
}

impl VoiceOutcome {
    fn reply(reply: Value) -> Self {
        Self { reply: Some(reply), ..Self::default() }
    }
}

fn reply_to(message: &Value, message_type: &str, body: Value) -> Value {
    let mut reply = json!({ "type": message_type, "body": body });
    if let Some(thid) = message["thid"].as_str().or(message["id"].as_str()) {
        reply["thid"] = json!(thid);
    }
    reply
}

fn refuse(message: &Value, code: &str, comment: &str) -> anyhow::Result<VoiceOutcome> {
    Ok(VoiceOutcome::reply(problem(message, code, comment)))
}

fn random_id(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes * 2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(bytes * 2);
    out
}

/// A voice channel's setting `field` (`speak`, `video`, `screen`): whether a
/// member with `role` may.
fn may(voice: &Value, field: &str, role: Role) -> bool {
    match voice[field].as_str() {
        Some("admins") => role == Role::Admin,
        _ => true,
    }
}

/// The room's path at the forwarding service: the community's DID hashed, so
/// that it names no DID, and the channel.
pub fn room_path(community_did: &str, channel: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(community_did.as_bytes());
    let short: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("c/{short}/{channel}")
}

impl Rooms {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A live connection opened by `did`; returns its number.
    pub fn open_live(&self, id: u64, did: &str) {
        self.lock().live.insert(id, Live { did: did.to_string(), watching: false });
    }

    /// The live connections `did` holds.
    pub fn live_of(&self, did: &str) -> Vec<u64> {
        self.lock().live.iter().filter(|(_, live)| live.did == did).map(|(id, _)| *id).collect()
    }

    /// A live connection closed: its holder, if it held no other, is out of any
    /// room it was in. Returns the presence to send.
    pub fn close_live(&self, community: &Community, id: u64) -> Vec<(u64, Value)> {
        let left = {
            let mut inner = self.lock();
            let Some(live) = inner.live.remove(&id) else { return Vec::new() };
            if inner.live.values().any(|other| other.did == live.did) {
                return Vec::new();
            }
            let mut left = Vec::new();
            for (channel, room) in inner.rooms.iter_mut() {
                if room.participants.remove(&live.did).is_some() {
                    left.push(channel.clone());
                }
            }
            left
        };
        left.iter().flat_map(|channel| self.presence_for_watchers(community, channel)).collect()
    }

    /// Who is in `channel`, as `presence` says them.
    fn participants_body(&self, community: &Community, inner: &Inner, channel: &str) -> Vec<Value> {
        let Some(room) = inner.rooms.get(channel) else { return Vec::new() };
        room.participants
            .iter()
            .map(|(did, p)| {
                let mut entry = json!({
                    "did": did,
                    "participant": p.id,
                    "muted": p.muted,
                    "deafened": p.deafened,
                    "server_muted": p.server_muted,
                    "sharing": { "camera": p.sharing.camera, "screen": p.sharing.screen, "avatar": p.sharing.avatar },
                });
                if let Some(name) = community.store.member(did).ok().flatten().and_then(|m| m.name) {
                    entry["name"] = json!(name);
                }
                entry
            })
            .collect()
    }

    /// `presence` for `channel`, for every live connection that watches.
    fn presence_for_watchers(&self, community: &Community, channel: &str) -> Vec<(u64, Value)> {
        let inner = self.lock();
        let body = json!({ "channels": [{ "channel": channel, "participants": self.participants_body(community, &inner, channel) }] });
        inner.live.iter().filter(|(_, live)| live.watching).map(|(id, _)| (*id, json!({ "type": PRESENCE, "body": body.clone() }))).collect()
    }

    /// Those who have said nothing for too long, taken out. Returns the presence
    /// to send.
    pub fn sweep(&self, community: &Community, now: i64) -> Vec<(u64, Value)> {
        let gone: Vec<String> = {
            let mut inner = self.lock();
            let live_dids: Vec<String> = inner.live.values().map(|live| live.did.clone()).collect();
            let mut gone = Vec::new();
            for (channel, room) in inner.rooms.iter_mut() {
                let before = room.participants.len();
                // Someone holding a live connection is still there, whatever they last said.
                room.participants.retain(|did, p| now - p.seen <= SILENT_FOR || live_dids.contains(did));
                if room.participants.len() != before {
                    gone.push(channel.clone());
                }
            }
            gone
        };
        gone.iter().flat_map(|channel| self.presence_for_watchers(community, channel)).collect()
    }

    /// The forwarding service's question (moq-auth's auth contract): what the
    /// session holding `token` may do. `None`: nothing, refuse it.
    pub fn grant(&self, community: &Community, community_did: &str, token: &str, now: i64) -> Option<Value> {
        let inner = self.lock();
        for (channel, room) in &inner.rooms {
            let Some((did, p)) = room.participants.iter().find(|(_, p)| p.token == token) else { continue };
            let channel_settings = community.store.channel(channel).ok().flatten()?.voice?;
            let role = community.store.member(did).ok().flatten()?.role;
            let path = room_path(community_did, channel);
            let mut publish = vec![format!("{path}/{}/position", p.id)];
            if may(&channel_settings, "speak", role) && !p.server_muted {
                publish.push(format!("{path}/{}/mic", p.id));
            }
            if may(&channel_settings, "video", role) {
                publish.push(format!("{path}/{}/camera", p.id));
                publish.push(format!("{path}/{}/avatar", p.id));
            }
            if may(&channel_settings, "screen", role) {
                publish.push(format!("{path}/{}/screen", p.id));
            }
            // A grant that is revalidated has to say when it runs out (moq-auth):
            // well after the next asking, so a slow answer doesn't cut anyone off.
            return Some(json!({ "publish": publish, "subscribe": [format!("{path}/**")], "revalidate": REVALIDATE_SECONDS, "expires": now + GRANT_SECONDS }));
        }
        None
    }

    /// Acts on one message of this protocol from `member`, which came over the
    /// live connection `live` if it did.
    pub fn handle(&self, community: &Community, community_did: &str, member: &Member, message: &Value, live: Option<u64>, now: i64) -> anyhow::Result<VoiceOutcome> {
        let Some(voice) = &community.voice else {
            return refuse(message, "e.p.msg.unsupported", "This server has no voice channels.");
        };
        let body = &message["body"];
        let message_type = message["type"].as_str().unwrap_or_default();
        let sender = member.did.as_str();
        // Anything said from inside a room says they are still there.
        if let Some(channel) = body["channel"].as_str() {
            if let Some(p) = self.lock().rooms.get_mut(channel).and_then(|room| room.participants.get_mut(sender)) {
                p.seen = now;
            }
        }
        let voice_channel = |id: &str| -> anyhow::Result<Option<(crate::store::Channel, Value)>> {
            Ok(community.store.channel(id)?.and_then(|channel| channel.voice.clone().map(|settings| (channel, settings))))
        };

        match message_type {
            WATCH_PRESENCE => {
                let Some(live) = live else {
                    return refuse(message, "e.p.req.live-only", "Presence is sent only over a live connection: open a WebSocket to the server.");
                };
                let stop = body["stop"] == true;
                let mut inner = self.lock();
                if let Some(entry) = inner.live.get_mut(&live) {
                    entry.watching = !stop;
                }
                if stop {
                    return Ok(VoiceOutcome::default());
                }
                let channels: Vec<Value> = community
                    .store
                    .channels()?
                    .iter()
                    .filter(|channel| channel.voice.is_some() && !channel.closed)
                    .map(|channel| json!({ "channel": channel.id, "participants": self.participants_body(community, &inner, &channel.id) }))
                    .collect();
                Ok(VoiceOutcome::reply(reply_to(message, PRESENCE, json!({ "channels": channels }))))
            }

            JOIN => {
                let channel_id = body["channel"].as_str().unwrap_or_default();
                let Some((channel, settings)) = voice_channel(channel_id)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel.");
                };
                if channel.closed {
                    return refuse(message, "e.p.msg.closed", "That voice channel is closed.");
                }
                if community.rules_owed_by(sender)? {
                    return refuse(message, "e.p.msg.rules-not-accepted", "Accept this community's rules first.");
                }
                let transport = settings["transport"].as_str().map(str::to_string).or_else(|| community.voice_defaults().ok().flatten().and_then(|v| v["transport"].as_str().map(str::to_string))).unwrap_or_else(|| "moq".into());
                if transport != "moq" {
                    return refuse(message, "e.p.msg.unsupported", "This server doesn't forward that transport yet.");
                }
                let mut left = Vec::new();
                let participant = {
                    let mut inner = self.lock();
                    let room = inner.rooms.entry(channel.id.clone()).or_default();
                    let full = settings["limit"].as_i64().is_some_and(|limit| room.participants.len() as i64 >= limit && !room.participants.contains_key(sender));
                    if full {
                        drop(inner);
                        return refuse(message, "e.p.req.room-full", "That voice channel is full.");
                    }
                    let participant = room
                        .participants
                        .entry(sender.to_string())
                        .and_modify(|p| p.seen = now)
                        .or_insert_with(|| Participant { id: format!("p-{}", random_id(6)), token: random_id(24), muted: false, deafened: false, server_muted: false, sharing: Sharing::default(), seen: now })
                        .clone();
                    // One room per community: joining this one leaves any other.
                    for (other_id, other) in inner.rooms.iter_mut() {
                        if other_id != &channel.id && other.participants.remove(sender).is_some() {
                            left.push(other_id.clone());
                        }
                    }
                    participant
                };
                let inner = self.lock();
                let participants: Vec<Value> = self.participants_body(community, &inner, &channel.id).into_iter().filter(|p| p["did"] != sender).collect();
                drop(inner);
                let role = member.role;
                let mut transport_body = json!({ "kind": "moq", "url": voice.relay_url, "path": room_path(community_did, &channel.id), "token": participant.token });
                if let Some(fingerprint) = &voice.relay_fingerprint {
                    transport_body["fingerprint"] = json!(fingerprint);
                }
                let joined = json!({
                    "channel": channel.id,
                    "participant": participant.id,
                    "transport": transport_body,
                    "may": { "speak": may(&settings, "speak", role) && !participant.server_muted, "video": may(&settings, "video", role), "screen": may(&settings, "screen", role) },
                    "participants": participants,
                });
                let mut outcome = VoiceOutcome::reply(reply_to(message, JOINED, joined));
                outcome.live.extend(self.presence_for_watchers(community, &channel.id));
                for channel in left {
                    outcome.live.extend(self.presence_for_watchers(community, &channel));
                }
                Ok(outcome)
            }

            LEAVE => {
                let channel = body["channel"].as_str().unwrap_or_default();
                let removed = self.lock().rooms.get_mut(channel).and_then(|room| room.participants.remove(sender)).is_some();
                let mut outcome = VoiceOutcome::default();
                if removed {
                    outcome.live = self.presence_for_watchers(community, channel);
                }
                Ok(outcome)
            }

            STATE => {
                let channel = body["channel"].as_str().unwrap_or_default();
                let Some((_, settings)) = voice_channel(channel)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel.");
                };
                let sharing = Sharing {
                    camera: body["sharing"]["camera"] == true,
                    screen: body["sharing"]["screen"] == true,
                    avatar: body["sharing"]["avatar"] == true,
                };
                if ((sharing.camera || sharing.avatar) && !may(&settings, "video", member.role)) || (sharing.screen && !may(&settings, "screen", member.role)) {
                    return refuse(message, "e.p.trust.not-authorized", "You may not share that here.");
                }
                let changed = {
                    let mut inner = self.lock();
                    let Some(p) = inner.rooms.get_mut(channel).and_then(|room| room.participants.get_mut(sender)) else {
                        drop(inner);
                        return refuse(message, "e.p.msg.not-in-room", "You aren't in that voice channel.");
                    };
                    let muted = body["muted"] == true;
                    let deafened = body["deafened"] == true;
                    let changed = p.muted != muted || p.deafened != deafened || p.sharing != sharing;
                    p.muted = muted;
                    p.deafened = deafened;
                    p.sharing = sharing;
                    changed
                };
                let mut outcome = VoiceOutcome::default();
                if changed {
                    outcome.live = self.presence_for_watchers(community, channel);
                }
                Ok(outcome)
            }

            KEYS => {
                let channel = body["channel"].as_str().unwrap_or_default();
                let inner = self.lock();
                let Some(room) = inner.rooms.get(channel) else {
                    drop(inner);
                    return refuse(message, "e.p.msg.not-in-room", "You aren't in that voice channel.");
                };
                let Some(from) = room.participants.get(sender).map(|p| p.id.clone()) else {
                    drop(inner);
                    return refuse(message, "e.p.msg.not-in-room", "You aren't in that voice channel.");
                };
                let mut outcome = VoiceOutcome::default();
                for entry in body["keys"].as_array().into_iter().flatten() {
                    let Some(to) = entry["to"].as_str() else { continue };
                    // Only to someone in the room, and passed on as it is: the server can't open it.
                    if let Some((did, _)) = room.participants.iter().find(|(did, p)| p.id == to && did.as_str() != sender) {
                        outcome.notices.push((
                            did.clone(),
                            json!({ "type": KEYS, "id": uuid::Uuid::new_v4().to_string(), "body": { "channel": channel, "from": from, "kid": entry["kid"], "jwe": entry["jwe"] } }),
                        ));
                    }
                }
                Ok(outcome)
            }

            INVITE => {
                let channel_id = body["channel"].as_str().unwrap_or_default();
                let Some((channel, _)) = voice_channel(channel_id)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel.");
                };
                let in_room = self.lock().rooms.get(channel_id).map(|room| (room.participants.contains_key(sender), room.participants.len()));
                let Some((true, count)) = in_room else {
                    return refuse(message, "e.p.trust.not-authorized", "Invite people from inside the voice channel.");
                };
                let invitees: Vec<&str> = body["to"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if invitees.len() > MAX_INVITEES {
                    return refuse(message, "e.p.msg.too-many", "Ten at a time, at most.");
                }
                let mut outcome = VoiceOutcome::default();
                let mut not_rung = Vec::new();
                for did in invitees {
                    if did == sender || community.store.member(did)?.is_none() {
                        continue;
                    }
                    if community.store.voice_rings(did)? {
                        let mut from = json!({ "did": sender });
                        if let Some(name) = &member.name {
                            from["name"] = json!(name);
                        }
                        outcome.notices.push((
                            did.to_string(),
                            json!({ "type": RING, "id": uuid::Uuid::new_v4().to_string(), "body": { "channel": channel.id, "from": from, "participants": count, "expires_at": now + RING_SECONDS } }),
                        ));
                    } else {
                        not_rung.push(did.to_string());
                    }
                }
                if !not_rung.is_empty() {
                    if let Some(text) = community.invitations_channel()? {
                        community.post_voice_invite(&text, member, &channel, &not_rung, now)?;
                        outcome.posted = true;
                    }
                }
                Ok(outcome)
            }

            SET_PREFERENCES => {
                if let Some(ring) = body["ring"].as_bool() {
                    community.store.set_voice_rings(sender, ring)?;
                }
                Ok(VoiceOutcome::reply(reply_to(message, PREFERENCES, json!({ "ring": community.store.voice_rings(sender)? }))))
            }

            MODERATE => {
                if member.role != Role::Admin {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin moderates a voice channel.");
                }
                let channel = body["channel"].as_str().unwrap_or_default();
                let participant = body["participant"].as_str().unwrap_or_default();
                let found = {
                    let mut inner = self.lock();
                    let Some(room) = inner.rooms.get_mut(channel) else {
                        drop(inner);
                        return refuse(message, "e.p.msg.not-in-room", "Nobody is in that voice channel.");
                    };
                    let did = room.participants.iter().find(|(_, p)| p.id == participant).map(|(did, _)| did.clone());
                    match did {
                        None => false,
                        Some(did) => {
                            if body["remove"] == true {
                                room.participants.remove(&did);
                            } else if let Some(mute) = body["mute"].as_bool() {
                                if let Some(p) = room.participants.get_mut(&did) {
                                    p.server_muted = mute;
                                }
                            }
                            true
                        }
                    }
                };
                if !found {
                    return refuse(message, "e.p.msg.not-in-room", "They aren't in that voice channel.");
                }
                Ok(VoiceOutcome { live: self.presence_for_watchers(community, channel), ..VoiceOutcome::default() })
            }

            _ => refuse(message, "e.p.msg.unsupported", "This server doesn't know that message."),
        }
    }
}
