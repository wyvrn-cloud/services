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
use crate::store::Member;

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
/// How long someone moved to another voice channel has to get there.
pub const MOVE_GRACE: i64 = 15;
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
    /// The member's.
    pub did: String,
    /// The device's own id (`join`'s `device`; empty for one that sent none). A
    /// member's devices share its DID, and each is a participant of its own.
    pub device: String,
    /// The `did:key` this device made for its time in the room, which the others
    /// seal their keys to: members aren't told each other's DIDs (community-voice/1.0's
    /// room keys). Empty from an app from before them.
    pub room_key: String,
    /// The live connection it joined over, if it did: closing that takes it out.
    pub live: Option<u64>,
    /// What their app gives the forwarding service.
    pub token: String,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub sharing: Sharing,
    pub seen: i64,
    /// Moved by a moderator: the voice channel their app is to join instead, and
    /// since when. Taken out of this one if it hasn't within [`MOVE_GRACE`].
    pub moving_to: Option<(String, i64)>,
    /// On a stage: among those who speak, not the audience (a moderator's say).
    pub speaker: bool,
    /// On a stage, in the audience: when they raised their hand to speak.
    pub hand: Option<i64>,
}

/// Whether a voice channel is a stage (community-voice/1.0's "Stages"): its
/// speakers are heard, everyone else listens, and moderators choose who speaks.
pub fn is_stage(channel: &crate::store::Channel) -> bool {
    channel.voice.as_ref().is_some_and(|voice| voice["stage"] == true)
}

/// Whether `p` may be heard in `channel`: as anyone may in a voice channel, and on
/// a stage, only as a speaker.
fn heard(community: &Community, p: &Participant, channel: &crate::store::Channel) -> bool {
    may(community, &p.did, channel, "speak") && !p.server_muted && (!is_stage(channel) || p.speaker)
}

/// Whether the forwarding service lets `p` send a voice at all. On a stage that
/// is the audience too, so that one asked up to speak is heard at once: the relay
/// holds what it was told for up to `REVALIDATE_SECONDS`, and a session of its own
/// for the new speaker took joining again. Who is *heard* there is the listeners'
/// to decide, from who the server says speaks (community-voice/1.0, "Stages"):
/// they take no voice from the audience.
fn may_send_voice(community: &Community, p: &Participant, channel: &crate::store::Channel) -> bool {
    may(community, &p.did, channel, "speak") && !p.server_muted
}

/// Whether `p` may show `field` (`video`, `screen`): on a stage, only a speaker.
fn shows(community: &Community, p: &Participant, channel: &crate::store::Channel, field: &str) -> bool {
    may(community, &p.did, channel, field) && (!is_stage(channel) || p.speaker)
}

#[derive(Debug, Default)]
struct Room {
    /// By participant id.
    participants: BTreeMap<String, Participant>,
}

impl Room {
    /// The participant of `did` that a request means: the one it names, if it is
    /// theirs, else their only one (the first, for a member that sent none from
    /// several devices).
    fn of(&self, did: &str, named: Option<&str>) -> Option<String> {
        match named {
            Some(id) => self.participants.get(id).filter(|p| p.did == did).map(|p| p.id.clone()),
            None => self.participants.values().find(|p| p.did == did).map(|p| p.id.clone()),
        }
    }

    fn holds(&self, did: &str) -> bool {
        self.participants.values().any(|p| p.did == did)
    }
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
    /// When time in voice was last counted (insights).
    counted: i64,
}

/// Every voice room of the community, and who holds a live connection.
#[derive(Default)]
pub struct Rooms {
    inner: Mutex<Inner>,
    /// What every participant's token starts with: where one host keeps many
    /// communities, which of them the forwarding service's question is for.
    token_prefix: String,
}

impl Rooms {
    /// Whether anyone is in a room, or holds a live connection.
    pub fn occupied(&self) -> bool {
        let inner = self.lock();
        !inner.live.is_empty() || inner.rooms.values().any(|room| !room.participants.is_empty())
    }

    /// The seconds since time in voice was last counted, at most a minute (a host
    /// asleep in between doesn't count); none the first time.
    pub fn uncounted(&self, now: i64) -> i64 {
        let mut inner = self.lock();
        let last = std::mem::replace(&mut inner.counted, now);
        if last == 0 { 0 } else { (now - last).clamp(0, 60) }
    }

    /// Who is in each room now: the members' DIDs, each once, by channel.
    pub fn in_rooms(&self) -> HashMap<String, Vec<String>> {
        let inner = self.lock();
        inner
            .rooms
            .iter()
            .map(|(channel, room)| {
                let mut dids: Vec<String> = room.participants.values().map(|p| p.did.clone()).collect();
                dids.dedup();
                dids.sort();
                dids.dedup();
                (channel.clone(), dids)
            })
            .filter(|(_, dids)| !dids.is_empty())
            .collect()
    }

    /// Rooms whose participants' tokens start with `prefix`.
    pub fn with_token_prefix(prefix: &str) -> Self {
        Self { token_prefix: prefix.to_string(), ..Self::default() }
    }
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

/// Whether `did` may do `field` (`speak`, `video`, `screen`) in the voice
/// channel: the channel's permission (`speak`, or `video` for either kind of
/// picture), held to the channel's own setting, `admins` meaning those who are
/// admins in older terms.
fn may(community: &Community, did: &str, channel: &crate::store::Channel, field: &str) -> bool {
    let check = || -> anyhow::Result<bool> {
        let powers = community.powers(did)?;
        let permission = if field == "speak" { "speak" } else { "video" };
        let setting = channel.voice.as_ref().and_then(|voice| voice[field].as_str().map(str::to_string));
        let timed_out = field == "speak" && community.timed_out(did, crate::gates::now())?;
        Ok(!timed_out && community.channel_permissions(did, &powers, channel)?.contains(permission) && (setting.as_deref() != Some("admins") || powers.all()))
    };
    check().unwrap_or(false)
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

    /// A live connection closed: whoever joined over it is out of their room, and
    /// so is a participant of its holder that joined otherwise, if the holder holds
    /// no other. Returns the presence to send.
    pub fn close_live(&self, community: &Community, id: u64) -> Vec<(u64, Value)> {
        let left = {
            let mut inner = self.lock();
            let Some(live) = inner.live.remove(&id) else { return Vec::new() };
            let holds_another = inner.live.values().any(|other| other.did == live.did);
            let mut left = Vec::new();
            for (channel, room) in inner.rooms.iter_mut() {
                let before = room.participants.len();
                room.participants.retain(|_, p| p.live != Some(id) && (holds_another || p.live.is_some() || p.did != live.did));
                if room.participants.len() != before {
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
        let stage = community.store.channel(channel).ok().flatten().is_some_and(|held| is_stage(&held));
        room.participants
            .values()
            .map(|p| {
                let did = &p.did;
                let mut entry = json!({
                    "did": did,
                    "participant": p.id,
                    "muted": p.muted,
                    "deafened": p.deafened,
                    "server_muted": p.server_muted,
                    "sharing": { "camera": p.sharing.camera, "screen": p.sharing.screen, "avatar": p.sharing.avatar },
                });
                // What they are shown as here, as members lists them: their nickname here first.
                let nickname = community.store.member_here(did).ok().flatten().and_then(|here| here.nickname);
                if let Some(name) = nickname.or_else(|| community.store.member(did).ok().flatten().and_then(|m| m.name)) {
                    entry["name"] = json!(name);
                }
                if !p.room_key.is_empty() {
                    entry["room_key"] = json!(p.room_key);
                }
                if let Some((to, _)) = &p.moving_to {
                    entry["moving_to"] = json!(to);
                }
                if stage {
                    entry["speaker"] = json!(p.speaker);
                    if let Some(at) = p.hand {
                        entry["hand"] = json!(at);
                    }
                }
                entry
            })
            .collect()
    }

    /// `presence` for `channel`, for every live connection that watches.
    fn presence_for_watchers(&self, community: &Community, channel: &str) -> Vec<(u64, Value)> {
        let inner = self.lock();
        let body = json!({ "channels": [{ "channel": channel, "participants": self.participants_body(community, &inner, channel) }] });
        // Not to anyone who may not see the channel.
        inner.live.iter().filter(|(_, live)| live.watching && community.sees_id(&live.did, channel).unwrap_or(false)).map(|(id, _)| (*id, json!({ "type": PRESENCE, "body": body.clone() }))).collect()
    }

    /// Those who have said nothing for too long, taken out. Returns the presence
    /// to send.
    pub fn sweep(&self, community: &Community, now: i64) -> Vec<(u64, Value)> {
        let gone: Vec<String> = {
            let mut inner = self.lock();
            let live_dids: Vec<String> = inner.live.values().map(|live| live.did.clone()).collect();
            let live_ids: Vec<u64> = inner.live.keys().copied().collect();
            let mut gone = Vec::new();
            for (channel, room) in inner.rooms.iter_mut() {
                let before = room.participants.len();
                // Someone holding a live connection is still there, whatever they last
                // said: the one they joined over, or any of theirs if they joined otherwise.
                room.participants.retain(|_, p| {
                    // Moved, and not gone where they were moved: out.
                    if p.moving_to.as_ref().is_some_and(|(_, since)| now - since > MOVE_GRACE) {
                        return false;
                    }
                    now - p.seen <= SILENT_FOR
                        || match p.live {
                            Some(live) => live_ids.contains(&live),
                            None => live_dids.contains(&p.did),
                        }
                });
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
            let Some(p) = room.participants.values().find(|p| p.token == token) else { continue };
            let did = &p.did;
            let held = community.store.channel(channel).ok().flatten().filter(|held| held.voice.is_some())?;
            community.store.member(did).ok().flatten()?;
            let path = room_path(community_did, channel);
            let mut publish = vec![format!("{path}/{}/position", p.id)];
            if may_send_voice(community, p, &held) {
                publish.push(format!("{path}/{}/mic", p.id));
            }
            if shows(community, p, &held, "video") {
                publish.push(format!("{path}/{}/camera", p.id));
                publish.push(format!("{path}/{}/avatar", p.id));
            }
            if shows(community, p, &held, "screen") {
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
        // Which of the sender's participants a request means (several devices of
        // one member; community-voice/1.0).
        let named = body["participant"].as_str();
        // Anything said from inside a room says they are still there.
        if let Some(channel) = body["channel"].as_str() {
            let mut inner = self.lock();
            if let Some(room) = inner.rooms.get_mut(channel) {
                if let Some(id) = room.of(sender, named) {
                    if let Some(p) = room.participants.get_mut(&id) {
                        p.seen = now;
                    }
                }
            }
        }
        let voice_channel = |id: &str| -> anyhow::Result<Option<(crate::store::Channel, Value)>> {
            // (One they may not see is no voice channel to them.)
            Ok(community.channel_for(sender, id)?.and_then(|channel| channel.voice.clone().map(|settings| (channel, settings))))
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
                    .filter(|channel| community.sees_id(sender, &channel.id).unwrap_or(false))
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
                // Moved here by a moderator (`moderate`'s `move_to`): let in, as on Discord,
                // whatever its limit and Connect.
                let inner_moved_here = {
                    let device = body["device"].as_str().unwrap_or_default();
                    self.lock().rooms.values().flat_map(|room| room.participants.values()).any(|p| p.did == sender && p.device == device && p.moving_to.as_ref().is_some_and(|(to, _)| *to == channel.id))
                };
                if !inner_moved_here && !community.may_in(sender, &channel, "connect")? {
                    return refuse(message, "e.p.trust.not-authorized", "You can't join this voice channel.");
                }
                let transport = settings["transport"].as_str().map(str::to_string).or_else(|| community.voice_defaults().ok().flatten().and_then(|v| v["transport"].as_str().map(str::to_string))).unwrap_or_else(|| "moq".into());
                if transport != "moq" {
                    return refuse(message, "e.p.msg.unsupported", "This server doesn't forward that transport yet.");
                }
                let device = body["device"].as_str().unwrap_or_default().to_string();
                let room_key = body["room_key"].as_str().filter(|key| key.starts_with("did:key:") && key.len() <= 256).unwrap_or_default().to_string();
                let mut left = Vec::new();
                let participant = {
                    let mut inner = self.lock();
                    let room = inner.rooms.entry(channel.id.clone()).or_default();
                    // "Move here": the member's other devices are taken out first.
                    if body["replace"] == true {
                        room.participants.retain(|_, p| p.did != sender || p.device == device);
                    }
                    let held = room.participants.values().find(|p| p.did == sender && p.device == device).map(|p| p.id.clone());
                    // (Someone a moderator moved here comes in whatever the limit, as on Discord.)
                    let moved_here = inner_moved_here;
                    let full = !moved_here && settings["limit"].as_i64().is_some_and(|limit| held.is_none() && room.participants.len() as i64 >= limit);
                    if full {
                        drop(inner);
                        return refuse(message, "e.p.req.room-full", "That voice channel is full.");
                    }
                    let participant = match held.and_then(|id| room.participants.get_mut(&id)) {
                        Some(p) => {
                            p.seen = now;
                            if live.is_some() {
                                p.live = live;
                            }
                            if !room_key.is_empty() {
                                p.room_key = room_key.clone();
                            }
                            p.clone()
                        }
                        None => {
                            let p = Participant {
                                id: format!("p-{}", random_id(6)),
                                did: sender.to_string(),
                                device: device.clone(),
                                room_key: room_key.clone(),
                                live,
                                token: format!("{}{}", self.token_prefix, random_id(24)),
                                muted: false,
                                deafened: false,
                                server_muted: false,
                                sharing: Sharing::default(),
                                seen: now,
                                moving_to: None,
                                // On a stage everyone comes in listening.
                                speaker: false,
                                hand: None,
                            };
                            room.participants.insert(p.id.clone(), p.clone());
                            p
                        }
                    };
                    // One room per community, for each device: joining this one leaves any other.
                    for (other_id, other) in inner.rooms.iter_mut() {
                        let before = other.participants.len();
                        if other_id != &channel.id {
                            other.participants.retain(|_, p| p.did != sender || p.device != device);
                        }
                        if other.participants.len() != before {
                            left.push(other_id.clone());
                        }
                    }
                    participant
                };
                let inner = self.lock();
                // Everyone else, the member's other devices included: each has a key of its own.
                let participants: Vec<Value> = self.participants_body(community, &inner, &channel.id).into_iter().filter(|p| p["participant"] != participant.id.as_str()).collect();
                drop(inner);
                let mut transport_body = json!({ "kind": "moq", "url": voice.relay_url, "path": room_path(community_did, &channel.id), "token": participant.token });
                if let Some(fingerprint) = &voice.relay_fingerprint {
                    transport_body["fingerprint"] = json!(fingerprint);
                }
                let mut joined = json!({
                    "channel": channel.id,
                    "participant": participant.id,
                    "transport": transport_body,
                    "may": { "speak": heard(community, &participant, &channel), "video": shows(community, &participant, &channel, "video"), "screen": shows(community, &participant, &channel, "screen") },
                    "participants": participants,
                });
                // On a stage: the voice is the relay's to forward from the start, so
                // one asked up to speak turns their microphone on where they are.
                if is_stage(&channel) {
                    joined["stage_mic"] = json!("held");
                }
                let mut outcome = VoiceOutcome::reply(reply_to(message, JOINED, joined));
                outcome.live.extend(self.presence_for_watchers(community, &channel.id));
                for channel in left {
                    outcome.live.extend(self.presence_for_watchers(community, &channel));
                }
                Ok(outcome)
            }

            LEAVE => {
                let channel = body["channel"].as_str().unwrap_or_default();
                let removed = self.lock().rooms.get_mut(channel).is_some_and(|room| {
                    let before = room.participants.len();
                    room.participants.retain(|id, p| p.did != sender || named.is_some_and(|named| named != id));
                    room.participants.len() != before
                });
                let mut outcome = VoiceOutcome::default();
                if removed {
                    outcome.live = self.presence_for_watchers(community, channel);
                }
                Ok(outcome)
            }

            STATE => {
                let channel = body["channel"].as_str().unwrap_or_default();
                let Some((held, _)) = voice_channel(channel)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel.");
                };
                let sharing = Sharing {
                    camera: body["sharing"]["camera"] == true,
                    screen: body["sharing"]["screen"] == true,
                    avatar: body["sharing"]["avatar"] == true,
                };
                let stage = is_stage(&held);
                let changed = {
                    let mut inner = self.lock();
                    let Some(p) = inner.rooms.get_mut(channel).and_then(|room| room.of(sender, named).and_then(|id| room.participants.get_mut(&id))) else {
                        drop(inner);
                        return refuse(message, "e.p.msg.not-in-room", "You aren't in that voice channel.");
                    };
                    if ((sharing.camera || sharing.avatar) && !shows(community, p, &held, "video")) || (sharing.screen && !shows(community, p, &held, "screen")) {
                        drop(inner);
                        return refuse(message, "e.p.trust.not-authorized", "You may not share that here.");
                    }
                    let muted = body["muted"] == true;
                    let deafened = body["deafened"] == true;
                    // A hand raised to speak, on a stage, by someone listening.
                    let hand = match body.get("hand").and_then(Value::as_bool) {
                        Some(true) if stage && !p.speaker => p.hand.or(Some(now)),
                        Some(_) => None,
                        None => p.hand,
                    };
                    let changed = p.muted != muted || p.deafened != deafened || p.sharing != sharing || p.hand != hand;
                    p.muted = muted;
                    p.deafened = deafened;
                    p.sharing = sharing;
                    p.hand = hand;
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
                let Some(from) = room.of(sender, named) else {
                    drop(inner);
                    return refuse(message, "e.p.msg.not-in-room", "You aren't in that voice channel.");
                };
                let mut outcome = VoiceOutcome::default();
                for entry in body["keys"].as_array().into_iter().flatten() {
                    let Some(to) = entry["to"].as_str() else { continue };
                    // Only to someone else in the room (another device of the sender's
                    // is), and passed on as it is: the server can't open it. `to` goes
                    // along, for a member in the room from several devices.
                    if let Some(p) = room.participants.get(to).filter(|p| p.id != from) {
                        outcome.notices.push((
                            p.did.clone(),
                            json!({ "type": KEYS, "id": uuid::Uuid::new_v4().to_string(), "body": { "channel": channel, "from": from, "to": to, "kid": entry["kid"], "jwe": entry["jwe"] } }),
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
                let in_room = self.lock().rooms.get(channel_id).map(|room| (room.holds(sender), room.participants.len()));
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
                    if did == sender || community.store.member(did)?.is_none() || !community.sees_id(did, &channel.id)? {
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
                let channel = body["channel"].as_str().unwrap_or_default();
                let participant = body["participant"].as_str().unwrap_or_default();
                let Some((held, _)) = voice_channel(channel)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel.");
                };
                let target = self.lock().rooms.get(channel).and_then(|room| room.participants.get(participant)).map(|p| p.did.clone());
                // On a stage, a speaker may go back to the audience by themselves.
                let stepping_down = body["speak"] == false && target.as_deref() == Some(sender) && body.get("mute").is_none() && body.get("move_to").is_none() && body["remove"] != true;
                if !stepping_down && !community.may_in(sender, &held, "moderate_voice")? {
                    return refuse(message, "e.p.trust.not-authorized", "Moderating a voice channel takes Mute and remove people in voice channels.");
                }
                if body.get("speak").is_some() && !is_stage(&held) {
                    return refuse(message, "e.p.msg.not-a-stage", "Only a stage has speakers.");
                }
                if let Some(target) = target.clone().filter(|target| target != sender) {
                    if !community.outranks(&community.powers(sender)?, &target)? {
                        return refuse(message, "e.p.trust.not-authorized", "You can only moderate someone ranked below you.");
                    }
                }
                // Moved to another voice channel: one the mover moderates too, and the
                // one moved sees.
                let move_to = match body["move_to"].as_str() {
                    None => None,
                    Some(to) => {
                        let Some((there, _)) = voice_channel(to)? else {
                            return refuse(message, "e.p.msg.unknown-channel", "There is no such voice channel to move them to.");
                        };
                        if there.closed || there.id == channel {
                            return refuse(message, "e.p.msg.unknown-channel", "That isn't another open voice channel.");
                        }
                        if !community.may_in(sender, &there, "moderate_voice")? {
                            return refuse(message, "e.p.trust.not-authorized", "Moving someone takes Mute and remove people in voice channels in both channels.");
                        }
                        if let Some(target) = &target {
                            if !community.sees_id(target, &there.id)? {
                                return refuse(message, "e.p.trust.not-authorized", "They can't see that voice channel.");
                            }
                        }
                        Some(there.id)
                    }
                };
                let found = {
                    let mut inner = self.lock();
                    let Some(room) = inner.rooms.get_mut(channel) else {
                        drop(inner);
                        return refuse(message, "e.p.msg.not-in-room", "Nobody is in that voice channel.");
                    };
                    if let Some(to) = &move_to {
                        match room.participants.get_mut(participant) {
                            Some(p) => {
                                p.moving_to = Some((to.clone(), now));
                                true
                            }
                            None => false,
                        }
                    } else if body["remove"] == true {
                        room.participants.remove(participant).is_some()
                    } else if let Some(p) = room.participants.get_mut(participant) {
                        if let Some(mute) = body["mute"].as_bool() {
                            p.server_muted = mute;
                        }
                        // Asked up to speak, or back to the audience: either way the hand is down.
                        if let Some(speak) = body["speak"].as_bool() {
                            p.speaker = speak;
                            p.hand = None;
                        }
                        true
                    } else {
                        false
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
