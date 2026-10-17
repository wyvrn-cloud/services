# wyvrn-did-host

Hosts short, stable DIDs for agents that have nowhere to put one.

An agent whose DID is a `did:peer:4` has an address hundreds of characters long
that changes whenever its keys do. This gives it a second one to hand out:

```
did:webvh:QmfGEUAcMpzo25kF2Rhn8L5FAXysfGnkzjwdKoNPi615XQ:id.example:alice
did:web:id.example:alice
```

The agent (the **holder**) writes its own `did:webvh` log and signs every entry.
This service checks each entry, stores it, and serves it, with the parallel
`did:web` that `did:webvh` derives from it and the holder's `whois`. It holds none
of the holder's keys: it cannot change a DID or read what is sent to one.

It speaks [`https://wyvrn.app/did-hosting/1.0`](https://github.com/wyvrn-cloud/protocols/blob/did-hosting/protocols/did-hosting/1.0/readme.md).
That document is the reference for what a holder sends and what is checked; this
one is about running the host.

## Running it

From the root of this repository:

```sh
docker compose up did-host
```

That runs it at <http://localhost:8081/>, where it can be written to and its
checks tried. Nothing it hosts will *resolve* from there: a `did:webvh` names a
domain, and is fetched from it over HTTPS.

Without Docker: `cargo run` in this directory (`services/did-host`; it reads
`config/default.toml` from there).

### For real

**[`deploy/did-host`](../../deploy/did-host) is the guide**: the host and Caddy
in Docker at your own domain, and linking a website to an address it hosts.

In short, it needs a domain of its own, served over HTTPS, with every path
passed to it:

```sh
DID_HOST_DOMAIN=id.example \
DID_HOST_PUBLIC_URL=https://id.example/ \
DID_HOST_DID_METHOD=web \
DID_HOST_ADMINS="did:peer:4zQm..." \
docker compose up -d did-host
```

- `DID_HOST_DOMAIN` is the host as it appears in the DIDs: `alice` is then
  `did:webvh:<SCID>:id.example:alice`, fetched from
  `https://id.example/alice/did.jsonl`.
- `DID_HOST_PUBLIC_URL` is where holders send their messages. It can be the same
  place. With `DID_HOST_DID_METHOD=web` the host itself is `did:web:id.example`.
- The `did-host-data` volume holds the host's own keys and every log it hosts:
  keep it, and back it up. A hosted DID whose log is lost cannot be put back by
  anyone but its holder. With `DID_HOST_DATABASE_URL` set, all of it is
  in that MySQL database instead
  ([where a service keeps its data](../../README.md#where-a-service-keeps-its-data)).

## What is served

| Path | What |
|---|---|
| `/<name>/did.jsonl` | The `did:webvh` log, as its holder wrote it. |
| `/<name>/did.json` | The parallel `did:web` document, derived from the log as `did:webvh` says to: with its `#files` and `#whois` services, whether or not a whois is published. |
| `/<name>/whois.vp` | The holder's whois, if it has published one. |
| `POST /` | `did-hosting/1.0` messages. Only ones their sender authenticated (authcrypt): anything else is answered 403. |
| `/did`, `/.well-known/did.json` | The host's own DID. |
| `/.well-known/did-configuration.json` | With `DID_HOST_DID_METHOD=web`: the host's own DID saying, signed, that this domain is its own. Its DID document names the domain back, so the two are a linked domain an app can check. Made by the host itself, each day; there is nothing to set up. |
| `/healthz` | `ok`. |

A name with nothing published under it answers 404 to all three, whether it is
free, reserved, or being claimed. A name with labels is a path: `official:news`
is at `/official/news/did.jsonl`.

## Names

First come, first served, one to a holder, except for names that are **reserved**.

- `reserved_names` in the configuration: by default the host's own routes and the
  names that would pass for the host itself (`admin`, `support`, `official`, ...).
- `reserved_namespaces`: leading labels under which every name is reserved. By
  default `official`.
- Any name an admin sets aside while the host runs.

A reserved name can be claimed only by the DID an admin has assigned it to. That
is all an admin decides: the holder then reserves and publishes like anyone else,
and from there the DID is the holder's. An admin cannot change it, only free the
name, which ends its hosting.

Admins are the DIDs in `DID_HOST_ADMINS` (`host.admins`), and send `assign` and
`free` as DIDComm messages. In the app they have an admin page for it (a
workflow only admins are shown): every name and how it stands, with setting one
aside and freeing it. Whoever is at the machine can do the same:

```sh
docker compose exec did-host wyvrn-did-host names
docker compose exec did-host wyvrn-did-host assign official:news "did:peer:4zQm..."
docker compose exec did-host wyvrn-did-host assign famous none
docker compose exec did-host wyvrn-did-host free alice
```

There is no list of unfit names beyond `reserved_names`. Add to it what you
don't want claimed.

## A holder that has gone quiet

A holder that never changes its devices never writes again, so silence means
nothing by itself. After `ask_after_days` (365) without hearing from a holder the
host sends its DID a trust-ping; any message back counts. After
`answer_within_days` (7) more with nothing, the name is `stale`: still served,
and one an admin may free.

## Configuration

`config/default.toml`, or the file `--config` or `$DID_HOST_CONFIG` names. The
environment overrides `DID_HOST_BIND`, `DID_HOST_PUBLIC_URL`,
`DID_HOST_DID_METHOD`, `DID_HOST_DOMAIN` and `DID_HOST_ADMINS`. Under `[host]`
there are also `hold_seconds` (600: how long a name is held for whoever asked),
`max_labels` (2), `require_portable` (true), `max_log_bytes` and
`max_whois_bytes`. A `[mediator]` section has messages to the host arrive through a
mediator instead of at `public_url`. They are the same messages either way: a
mediator carries what a sender sealed, and the host opens it and knows the sender
by the key it used. What the host serves is fetched over HTTPS regardless.

## What it does not do yet

- **No witnesses or watchers.** A host that withheld a holder's newer entries
  would not be caught by anything here.
- **No rate limits.** Put one in front of it.

## Reading what it hosts

The other side of this -- being given `did:web:id.example:alice` and ending up
with a verified DID and a profile -- is [`crates/wyvrn-hosted-did`](../../crates/wyvrn-hosted-did).
It decides everything from bytes it is handed, and is given a `Fetch` to get
them with. That is what lets the tests here resolve `https://id.example/...`
without a network: the fetch is answered by the host under test, or by a
prepared answer where the test is of a host that lies.

## Development

```sh
cargo test -p wyvrn-did-host
```

`tests/host.rs` runs the server and has holders write real `did:webvh` logs,
publish them over DIDComm, and fetch them back; then resolves them as someone
given the short name would, including from a host that serves something else.
