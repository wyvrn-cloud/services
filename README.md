# wyvrn-services

The services that run beside [Wyvrn](https://wyvrn.app), in one Rust workspace so
that they share crates instead of each carrying its own copy of the same plumbing.

| | |
|---|---|
| [`services/announce`](services/announce) | A one-way announcement channel: people subscribe, admins post, everyone subscribed receives it, and a web page shows every post. |
| [`services/community`](services/community) | A community server: channels that the people let in, and nobody else, read and post in. A post can ask a question, answered as a form. |
| [`services/did-host`](services/did-host) | Hosts short, stable `did:webvh` DIDs, and their parallel `did:web`, for agents that write and sign them themselves. |
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

The service's own keys stay in a file either way (`data/identity.json`), so its
volume is still worth keeping.

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
   and an entry in `docker-compose.yml`.
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

## Licence

Apache-2.0.
