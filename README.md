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
