# wyvrn-community-host

Many communities in one process, the way ACA-Py keeps many agents. Each community is `wyvrn-community`'s own server, opened from settings the host makes for it. It has its own DID (the `did:web` at `<public_url>c/<name>/`), its own keys, stores and public address (`community:<name>` at the DID host), and nothing of one reaches another. There is no shared table with a community column.

The single `wyvrn-community` is still the build for one community on its own server, and is where a hosted community goes when its owner moves it to a server of their own (community/1.0, "Hosted communities, and moving").

## Where a community is kept

Chosen per community when it is made:

| Kind | Where |
|---|---|
| `sqlite` | `<storage.dir>/<name>/`: `identity.json` (its keys), `community.sqlite` (with its profile and address log), `stickers.sqlite`, `files.sqlite` |
| `mysql-db` | A database of its own on the MySQL server, `<database_prefix><id>`: its own tables starting `m_`, its packs' `s_`, its files' records' `f_` |
| `mysql-shared` | Tables of its own in `shared_database`, starting `c<id>_m_` (`c<id>_s_`, `c<id>_f_`) |

The files members attach are kept in `<storage.dir>/<name>/files` whatever the kind. All MySQL communities share the server's connections (`WYVRN_MYSQL_CONNECTIONS`, 64 by default).

A host admin can move a community to another kind from its admin page (Move … to). It is closed, every table and its keys are copied, and it opens from the new place at the next request; requests that come meanwhile wait. What it was kept in is left for an admin to remove, and SQLite files it left before are renamed aside if it comes back to them. A MySQL database or tables that already hold something are refused. Its DID, members and posts stay as they were.

## Thousands, few open

A community is opened when it is spoken to. It is closed once nobody has spoken to it for `loading.idle_seconds` and nobody holds a live connection or is in a voice room. Past `loading.max_loaded`, the least recently used are closed first. Something it still has to do unasked is noted as when to open it again, and the host opens it then: trying again a member who couldn't be reached (a minute later), or telling everyone that a poll ended.

## Making and managing communities

Host admins (`host.admins`, or `COMMUNITY_HOST_ADMINS`) see the host's admin page in the app, under Settings, Services you run. It lists every community, makes new ones, sets what each may hold, and suspends, removes or restores them. The same from the command line:

```
wyvrn-community-host create <name> <owner-did> [sqlite|mysql-db|mysql-shared] [<title>]
wyvrn-community-host list
wyvrn-community-host suspend|resume|delete|restore <name>
wyvrn-community-host limits <name> <members> <channels>
```

A community's owner is its first admin. A removed community is kept for 30 days and can be restored until then.

## Bringing in a Discord server's history

From a DiscordChatExporter backup (JSON, with `--media`), a channel at a time:

```
wyvrn-community-host discord-survey <folder> [<channel>...]
wyvrn-community-host import-discord <name> <folder> <channel>... [--map <discord-id>=<channel>]
                     [--text-only] [--no-threads] [--expressions <folder>] [--refresh-embeds] [--dry-run]
```

`discord-survey` counts what a backup holds. `import-discord` brings each channel's posts (and its threads) into the channel here of the same name, oldest first, read among its posts by time; nobody is notified. Run again, it brings only what isn't there yet. Discord's markup is Markdown here, as the app draws it: `<@id>`, `<@&id>` and `<#id>` become `@name` and `#channel`, the server's own emoji its packs' (`--expressions`, the folder `import-expressions` filled), and `<t:…>` is left for the app to show in the reader's zone. The same goes for the words of a post's embeds (a bot's rules, a webhook's cards): title, description, and each field's name and value.

`--refresh-embeds` brings in nothing new: for the posts already imported from those channels it writes their embeds again from the backup, as an import does now, and marks each one that changed as changed (as an edit is), so members' apps take it up. What a post says, its reactions and files are left as they are. It is how posts imported before embeds were read this way are put right, without importing again:

```
wyvrn-community-host import-discord <name> <folder> rules --refresh-embeds --dry-run   # what it would change
wyvrn-community-host import-discord <name> <folder> rules --refresh-embeds
```

## Moving out to a server of its own

A community's owner (its owner in community/1.0's sense, which follows them when their DID changes and when they hand the community on) has a page in the community's own admin workflows, "Move to your own server", listed to them alone:

1. **Begin moving.** The community is read-only from then on (`e.p.req.moving`). Every member is told by a `community` with `moving`, sent from the community's public address with the `from_prior` that brings a member who knew it at its path to the address.
2. **Make the export.** It is offered once, at a link good for 15 minutes (`/export/<token>`). It is a `.tar.gz` of everything, as the single `wyvrn-community` keeps it in SQLite: its keys, its store (members, posts, roles, invites, its address's log), its packs, its files, a `config/default.toml` and a README.
3. The owner runs it at their own domain with `COMMUNITY_PUBLIC_ADDRESS` set to the same address. Holding it (with the same keys) writes the address's next log entry, which names their server, and members' apps follow.
4. The host checks every minute. Once the address no longer names this copy, it is stopped and kept for 30 days (`moved`), and a host admin can restore it. Without a DID host, a host admin marks it handed over.

Called off before then, it is open here again as it was.

## Limits

`[limits]` sets what each community may hold, and an admin can change them for one community. Each limit is refused where it is met, with community/1.0's codes:
- members: `e.p.req.community-full`;
- channels: `e.p.msg.too-many`;
- files: files/1.0's `e.p.msg.quota`;
- kinds of channel: `disabled_channel_kinds = ["wiki", "stage"]` turns those off (any of `announcement`, `voice`, `stage`, `forum`, `gallery`, `calendar`, `form`, `wiki`; a text channel can't be). A new one is refused with `e.p.msg.kind-off`, and one a community has already is read-only: seen and moderated, but nothing new is posted, answered or joined in it. A community's own setting, on the admin page, takes the place of the host's; set back to the host's, it follows the host's again.

## Voice

Every community shares one moq-relay. Its `[auth] url` is `<public_url>voice/auth`. A participant's token starts with the community's name (`<name>.`), so the host passes each question to the community whose token it is.

## Deploying

`docker compose --profile communities up -d`, behind HTTPS with `COMMUNITY_HOST_DID_METHOD=web` (did:web resolves over HTTPS only; `peer` is for a host without it).
