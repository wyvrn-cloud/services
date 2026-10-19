# wyvrn-relay

An outbound relay. An agent registered with it holds one WebSocket open to it
and sends through it what it would otherwise post to others' endpoints, so that
where its messages go (a contact's mediator, a service) sees the relay's address
and not the agent's.

It speaks [`https://wyvrn.app/outbound-relay/1.0`](https://github.com/wyvrn-cloud/protocols/blob/outbound-relay/protocols/outbound-relay/1.0/readme.md).
That document says what a client sends and what comes back; this one is about
running the relay.

## What it carries

`RELAY_CARRIES` (`relay.carries`):

- `sends`: a client's messages to others. The relay posts each one and hands
  back the answer. Clients speak to their own mediator directly.
- `all` (the default): that, and what a client says to its own mediator:
  collecting, and a WebSocket opened through the relay for live delivery.

Clients find out by discover-features, which discloses
`app.wyvrn.relay.sends` or `app.wyvrn.relay.all` as a goal code, and the grant
says it again.

## Who it takes

With `RELAY_OPEN=true` (the default) anyone who asks is granted. Otherwise only
the DIDs in `RELAY_CLIENTS`. A grant is kept until the client releases it, or:

```sh
docker compose exec relay wyvrn-relay grants
docker compose exec relay wyvrn-relay release "did:peer:4zQm..."
```

## Where it goes

Only to `http(s)://` addresses for posts and `ws(s)://` for sockets, and only to
the public internet. An address on this machine or a private network is refused,
checked once before anything is sent and again for every connection made, so a
name that changes what it points at in between is refused too. Anyone it grants
could otherwise reach what sits behind its firewall.

`RELAY_ALLOW` names hosts or addresses it may reach anyway: a mediator running
beside it, reached by its name on the Docker network, say.

It keeps nothing of what passes through it, and can't read it: each message is
packed for its recipient. It learns who its clients are, where each message goes,
when, and how big it is.

## Running it

From the root of this repository:

```sh
docker compose up relay
```

That runs it at <http://localhost:8087/> as a `did:peer`. For real, behind HTTPS
at a domain of its own:

```sh
RELAY_PUBLIC_URL=https://relay.example/ \
RELAY_DID_METHOD=web \
docker compose up -d relay
```

As a `did:web` its document lists its WebSocket address (`wss://relay.example/`)
before the HTTPS one. A `did:peer` relay names only the HTTP address, and
clients connect to the same address with `ws://` in place of `http://`. Its
proxy must pass WebSocket upgrades on `/`.

Without Docker: `cargo run` in this directory (it reads `config/default.toml`).

## Configuration

`config/default.toml`, or the file `--config` or `$RELAY_CONFIG` names. The
environment overrides: `RELAY_BIND`, `RELAY_PUBLIC_URL`, `RELAY_DID_METHOD`,
`RELAY_CARRIES`, `RELAY_OPEN`, `RELAY_CLIENTS`, `RELAY_ALLOW`, `RELAY_MAX_BYTES`
(the largest message, and the largest frame; 16 MiB), and `RELAY_DATABASE_URL`
for its grants and keys in MySQL or MariaDB instead of the volume.
