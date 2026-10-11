# wyvrn-announce

A one-way announcement channel over DIDComm. People subscribe to it from their app,
admins post, and every subscriber receives each post. Nobody can reply; subscribers
can react to a post with emoji, and everyone is shown how many reacted with what, never who. Every post is
kept, and a web page shows them all.

It speaks [`https://wyvrn.app/announcements/1.0`](https://github.com/wyvrn-cloud/protocols/blob/announcements/protocols/announcements/1.0/readme.md),
which [Wyvrn](https://wyvrn.app) understands: add the channel's DID as a contact and
the app subscribes, shows posts as they arrive, and offers a place to write only to
admins.

## Running it

```sh
docker compose up
```

That serves the channel at <http://localhost:8080/> under a `did:peer:4`, which is
enough to try it against a local Wyvrn. It logs its DID at startup, and `GET /did`
returns it.

Without Docker: `cargo run` (reads `config/default.toml`).

### For real

Put it behind HTTPS and give it a `did:web`:

```sh
ANNOUNCE_PUBLIC_URL=https://updates.example/ \
ANNOUNCE_DID_METHOD=web \
ANNOUNCE_NAME="Wyvrn Updates" \
ANNOUNCE_DESCRIPTION="Release notes and news from the Wyvrn team." \
docker compose up -d
```

The channel is then `did:web:updates.example`, and publishes its DID document at
`/.well-known/did.json`. The `announce-data` volume holds its keys, every post and the
list of subscribers: keep it, and back it up.

A `did:webvh` is not supported for the channel's own DID yet. Subscribers may be any
DID the [didcomm](https://github.com/wyvrn-cloud/didcomm) library resolves, `did:webvh`
included.

## Admins

Only admins can post. The first one has to be named from outside, by their DID (in
Wyvrn: Settings shows your own):

```sh
ANNOUNCE_ADMINS="did:peer:4zQm..."          # in the environment, or channel.admins in the config
docker compose exec announce wyvrn-announce admin add "did:peer:4zQm..."
```

After that an admin makes others from the app, out of the people subscribed.

A Wyvrn account's DID changes whenever a device is added to or removed from it. The
channel follows the change (a signed `from_prior`), so an admin stays an admin, and the
DID they left is never listened to again. The configured list is therefore only needed
once; a DID in it that has since been replaced is ignored.

```sh
wyvrn-announce subscribers            # role, whether subscribed, name, DID
wyvrn-announce admin list
wyvrn-announce admin remove <did>     # refuses to remove the last one
```

A role changed from the command line takes effect at once. The person's app shows it
the next time it starts.

## The web pages

| Path | What |
|---|---|
| `/` | Every post, newest first, 20 to a page (`?before=<seq>` for older ones) |
| `/posts/<seq>` | One post |
| `/feed.xml` | The newest 30 as an Atom feed |
| `/api/posts` | The same as JSON (`?before=<seq>&limit=<n>`, at most 50) |
| `/api/channel` | Name, description, DID, number of posts and subscribers |

Posts are Markdown. HTML written into a post is shown as text, never run. The pages
have nowhere to write anything.

## Configuration

See [`config/default.toml`](config/default.toml). The environment overrides the
settings a deployment most often changes: `ANNOUNCE_BIND`, `ANNOUNCE_PUBLIC_URL`,
`ANNOUNCE_DID_METHOD`, `ANNOUNCE_NAME`, `ANNOUNCE_DESCRIPTION`, `ANNOUNCE_ADMINS`.
`ANNOUNCE_CONFIG` (or `--config`) names another file.

`channel.show_authors` (off by default) adds the name an author's app gave to each of
their posts, for subscribers and on the web pages. An author's DID is never shown.

## How posts get out

A post is logged first, then sent to each subscriber's DID, which for a Wyvrn account
means to their mediator, where it waits for whichever of their devices is next online.
A subscriber whose mediator can't be reached is tried again every minute, in order,
from where they were. A new subscriber is not sent the back catalogue: their app asks
for as much of it as it wants (`history-request`).

## Development

```sh
cargo test       # unit tests, and tests/channel.rs: the server with real agents over HTTP
```
