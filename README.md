# wyvrn-services

The services that run beside [Wyvrn](https://wyvrn.app), in one Rust workspace so
that they share crates instead of each carrying its own copy of the same plumbing.

| | |
|---|---|
| [`services/announce`](services/announce) | A one-way announcement channel: people subscribe, admins post, everyone subscribed receives it, and a web page shows every post. |
| [`services/community`](services/community) | A community server: channels that the people let in, and nobody else, read and post in. A post can ask a question, answered as a form. |
| [`services/did-host`](services/did-host) | Hosts short, stable `did:webvh` DIDs, and their parallel `did:web`, for agents that write and sign them themselves. |
| [`services/cards`](services/cards) | Link cards: what a link to a public address or a contact invite looks like where it is posted, as OpenGraph tags and a drawn picture. Not a DIDComm service: it has no identity and keeps nothing. |
| [`services/stickers`](services/stickers) | Hosts sticker and custom-emoji packs. They are made by talking to it, as `workflow/1.0` workflows, and fetched from it over HTTPS. |
| [`crates/wyvrn-service`](crates/wyvrn-service) | What every DIDComm service does, whatever it is for. |
| [`crates/wyvrn-hosted-did`](crates/wyvrn-hosted-did) | A hosted DID from both ends: the holder writing and signing its log, and anyone reading it -- from a `did:web` to the `did:webvh` behind it, verified, and its whois profile. Works on what was fetched; the fetching is the caller's. |
| [`crates/wyvrn-hosted-did-wasm`](crates/wyvrn-hosted-did-wasm) | The same for a web app, as a WebAssembly module. |

The mediator is not here: it is its own repository, with its own workspace.

## Running a service

```sh
docker compose up announce
```

`docker-compose.yml` lists every service, each started by name. Each service's own
README says what it needs set for a real deployment.

Without Docker, from a service's directory: `cargo run` (it reads its `config/`
from there, and keeps its data in `data/`).

For real ones, on a machine that already has a web server and a database:
[`deploy/services`](deploy/services). For only the DID host, with a Caddy of
its own: [`deploy/did-host`](deploy/did-host).

## Who a service says it is

A service that is a `did:web` publishes a profile: a name, a description, a
picture, and if it likes a banner and a background, signed with its own key (`/whois.vp`, named in its DID document as
did:webvh's `#whois`). An app that looks the DID up before adding it shows that
profile, and nothing has to be registered anywhere.

```sh
ANNOUNCE_PROFILE_NAME="Wyvrn updates"
ANNOUNCE_PROFILE_DESCRIPTION="News about Wyvrn."
ANNOUNCE_PROFILE_PICTURE=/app/profile.png        # PNG, JPEG, WebP or GIF, at most 200 KB
ANNOUNCE_PROFILE_BANNER=/app/banner.jpg          # across the top: wide, about 3 to 1
ANNOUNCE_PROFILE_BACKGROUND=/app/background.jpg  # behind the rest
```

`<SERVICE>_PROFILE_NAME`, `_PROFILE_DESCRIPTION`, `_PROFILE_PICTURE`,
`_PROFILE_BANNER` and `_PROFILE_BACKGROUND`, or a `[profile]` section in the
configuration file (`name`, `description`, `picture`, `banner`, `background`).
Each picture is a file of the same kinds and limit, and all of them travel inside
the profile: keep the three together well under the 512 KB an app will read. Left out, the name and description are what the service is called
anyway (`ANNOUNCE_NAME`, `COMMUNITY_NAME`; "Stickers"; "Public addresses"), and
the announcement channel's picture is its `channel.picture`. In Docker each
picture is a file mounted into the container. They are read when the service
starts: changing one is replacing the file, or the setting, and restarting it.

An admin of an announcement channel or a community can change its name,
description and picture from the app (their `set-profile`). What was changed is
kept in the service's database and laid over these settings; emptying a field
puts the setting back. The banner and background are only ever the settings'.

Whether an app marks the service **Official** is the app's to say, not the
service's. Wyvrn marks two things: the `did:web` of each domain it was built to
take as the project's own (`VITE_OFFICIAL_DOMAINS`), and any name under
`official:` at its DID host, which is the better of the two:

### A public address for a service

A service is the `did:web` of its own domain: whatever that domain serves today.
Set to, it also holds an address at a DID host, as a person's app does, and is
known by that: a `did:webvh` with a history the host can't rewrite.

```sh
ANNOUNCE_PUBLIC_ADDRESS=did:web:id.example:official:update-news
```

(`COMMUNITY_PUBLIC_ADDRESS`, `STICKERS_PUBLIC_ADDRESS`. A name is 3 to 32 of
a-z, 0-9 and hyphens to a label.) A name under `official:` is reserved, so one of
the host's admins sets it aside for the service first, by the service's own DID:

```sh
docker compose exec did-host wyvrn-did-host assign official:update-news did:web:updates.example
```

The service then claims the name by itself once it is serving (it tries again
until it can), publishes its profile there, and names the address in its own DID
document. Looked up by either DID it is one party: **Official**, by the name its
host reserved for it, and **History verified** where a bare `did:web` is "on its
host's word". Nothing needs listing in any app's build.

From then on the address is the DID the service is known by
(`crates/wyvrn-service`'s `voice`): what is sent to it is read (the service's key
is filed under the address's key ids too), what the service sends unasked comes
from it, with a `from_prior` signed by its `did:web` so that whoever knew it by
that follows it there, and an answer comes from whichever DID it was written to.
What it asks of its DID host it asks as its `did:web`, the DID the name was set
aside for. Writing to its own domain goes on working.

The address's log is kept in the service's database with its keys. Only the
service can continue it: lose both and the name can only be freed by the host's
admin and claimed afresh, as a new DID.

## Who the settings name

A setting that names people (`ANNOUNCE_ADMINS`, `COMMUNITY_ADMINS`,
`COMMUNITY_MEMBERS`) takes the DID their app writes from, or their **public
address** (`did:webvh:…:id.example:alice`, or its `did:web`). An app writes
from its account's DID, never from the address, so an address is matched by
key: whoever writes with a key the address's document lists is who it names,
and is given the role under the DID they write from, the first time they
write. (`crates/wyvrn-service`'s `named`; the DID host has always taken its
admins this way.)

## Where a service keeps its data

In a SQLite file under `data/`, with nothing to set up. Or in MySQL or MariaDB,
when the environment gives it a database:

```sh
STICKERS_DATABASE_URL=mysql://user:password@host:3306/wyvrn_stickers
```

`<SERVICE>_DATABASE_URL` (`ANNOUNCE_`, `COMMUNITY_`, `DID_HOST_`, `STICKERS_`),
or plain `DATABASE_URL` for a service run alone. `mariadb://` is the same thing.
Each service needs **a database of its own**: their tables share names, and one
started on another's refuses to run. It makes its tables itself. A password
with anything but letters and digits in it is percent-encoded.

Its own keys go with the rest: `data/identity.json` beside a SQLite file, and in
the database for a service that has one (so its volume holds nothing, and the
container can be remade freely). They are stored unencrypted in both. A
service given a database after it already had that file takes its keys from
the file, once, and is the service it was.

Nothing moves a SQLite file's contents into MySQL: choose before there is
anything to lose.

`crates/wyvrn-service`'s `db` module is what makes one set of statements serve
both, and says what a statement may not use. Every test that uses a store runs
against MySQL too, each in a database made for it:

```sh
docker run -d --rm -e MYSQL_ROOT_PASSWORD=test -p 127.0.0.1:13306:3306 mysql:8
WYVRN_TEST_DATABASE_URL=mysql://root:test@127.0.0.1:13306/mysql cargo test --workspace
```

## What is shared

`wyvrn-service` is the part of a service that is the same in all of them:

- **An address.** The service's DID: a `did:web` at its public URL, or a
  `did:peer:4` made from its keys. And the routes that publish it (`GET /did`, the
  DID document).
- **An endpoint.** Packed DIDComm messages are posted to it; each is unpacked,
  handed to the service, and the answer sent back.
- **A mediator, optionally.** A service with no address the people it serves can
  reach (behind a home router; serving a phone app that refuses plain HTTP)
  registers with a mediator and collects from it instead. Messages that arrive
  that way are handled exactly as posted ones are.
- **A profile.** Who the service says it is, signed, for an app to show.
- **A store.** A SQLite file or a MySQL database, behind the same statements.
- **Start-up.** Logging, and serving until the container is stopped.

A service implements one trait, `Service`: its agent, and what it answers a message
with. Everything else it has is its own: its configuration file, its store, its
protocol, its pages.

## Adding a service

1. `services/<name>/`, with a `Cargo.toml` that takes its package details and its
   dependencies from the workspace (`version.workspace = true`,
   `tokio = { workspace = true }`); `services/announce` is the model. A dependency
   more than one member uses is declared once, in the root `Cargo.toml`.
2. Implement `wyvrn_service::Service`, and build its router from
   `wyvrn_service::address::routes` and `wyvrn_service::http::receive`.
3. A `Dockerfile` in its directory, built from the repository root (the workspace),
   and an entry in `docker-compose.yml`. Copy another service's: it begins with
   the shared base (below), which has to stay word for word the same.
4. A row in the table above.

Something a second service turns out to need that the first already has belongs in
`crates/`, moved there when the second service needs it, not before.

## Development

```sh
cargo test                    # everything
cargo test -p wyvrn-announce  # one member
```

The DIDComm crates come from [wyvrn-cloud/didcomm](https://github.com/wyvrn-cloud/didcomm)
by git; `Cargo.lock` pins the commit.

### Building the images

Every service's `Dockerfile` begins with the same **shared base**: a stage that
compiles what all the services have in common (the dependencies of
`crates/wyvrn-service`, wyvrn-didcomm among them) as an image layer, with
[cargo-chef](https://github.com/LukeMathWalker/cargo-chef). Because it is the same
in each, Docker builds it once and every other service's build reuses it, however
the images are built (`docker build -f services/<name>/Dockerfile .` one after
another, or compose). It is made again only when a `Cargo.toml` or `Cargo.lock`
changes. Each service then compiles what it needs beyond it as a layer of its own
(also made again only when a manifest or the lock file changes), and its code on
top: a change to the code alone compiles the workspace's own crates, and nothing
else.

`scripts/check-dockerfiles.sh` says whether the shared bases still match; the
build secrets they take are handled by `deploy/docker/cargo-secrets.sh`.

## Licence

Apache-2.0.
