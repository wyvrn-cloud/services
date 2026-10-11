# wyvrn-stickers

Hosts sticker packs and custom-emoji packs.

A pack is made by talking to this service, the way Telegram's sticker bot is
talked to: name the pack, send pictures one at a time each with its emoji,
publish, choose an icon, choose the short name its address will have. Here that
conversation is a [`workflow/1.0`](https://didcomm.org/workflow/1.0) workflow.
The service is the workflow's Processor; the app someone makes a pack with is
its Coordinator, and draws each step from what the service sends.

A pack is then fetched over HTTPS, by anyone who has its address:

```
https://stickers.example/cats-being-cats
```

What a pack is, and how a sticker travels in a conversation, is
[`https://wyvrn.app/stickers/1.0`](https://github.com/wyvrn-cloud/protocols/blob/stickers/protocols/stickers/1.0/readme.md).
That document is the reference for apps; this one is about running the service.

## Running it

From the root of this repository:

```sh
docker compose up stickers
```

That runs it at <http://localhost:8082/>. A pack's address begins with the
service's public URL, so packs made there are only good on this machine.

Without Docker: `cargo run` in this directory (`services/stickers`; it reads
`config/default.toml` from there).

### For real

It needs an address of its own, over HTTPS:

```sh
STICKERS_PUBLIC_URL=https://stickers.example/ \
STICKERS_DID_METHOD=web \
STICKERS_ADMINS="did:peer:4zQm..." \
docker compose up -d stickers
```

- `STICKERS_PUBLIC_URL` is where makers send their messages and where packs are
  fetched from. It is part of every pack's address: changing it later breaks
  every pack already handed out.
- With `STICKERS_DID_METHOD=web` the service itself is `did:web:stickers.example`.
- The `stickers-data` volume holds the service's own keys and every pack. Keep
  it, and back it up.

## What is served

| Path | What |
|---|---|
| `/<name>`, `/<name>/pack.json` | The pack: its title, kind, and each picture's file, size, hash and emoji. |
| `/<name>/<file>` | One picture. A file is named for its hash, so it never changes and is served to be kept. |
| `/drafts/<instance>/<file>` | A picture of a pack still being made, for its maker's app to show. |
| `POST /` | `workflow/1.0` messages. Only ones their sender authenticated (authcrypt): anything else is answered 403. |
| `/did`, `/.well-known/did.json` | The service's own DID. |
| `/healthz` | `ok`. |

A pack that was deleted or taken down answers 410.

## The workflows

`discover` lists them and `fetch-template` returns each in full.

| Template | What it does |
|---|---|
| `new-sticker-pack` | Makes a sticker pack: title, stickers each with an emoji, icon, short name. |
| `new-emoji-pack` | The same for custom emoji, each with the shortcode it is typed as. |
| `edit-pack` | Changes one of the maker's own packs: add and remove pictures, rename, choose the icon, keep it private or not, delete it. |

A maker has one of each under way at a time. A workflow belongs to the DID that
started it, and to nobody else it isn't there. One left for a day
(`draft_hours`) is forgotten with what it had gathered.

A maker whose devices change writes from a new DID. The first message from it
carries a `from_prior`, as DIDComm provides for, and their packs and whatever
they were in the middle of go with them.

## Limits

Under `[service]` in the configuration:

| Setting | Default | |
|---|---|---|
| `max_sticker_px`, `max_sticker_bytes` | 512, 500 KB | A sticker's longest side, and its file. |
| `max_emoji_px`, `max_emoji_bytes` | 128, 128 KB | The same for a custom emoji. |
| `max_per_pack` | 120 | Pictures in one pack. |
| `max_packs_per_maker` | 50 | Packs one maker may have. |
| `draft_hours` | 24 | How long an unfinished pack is kept. |
| `reserved_names` | the service's own routes, and names like `official` | Names no pack may take. |

Pictures are PNG, GIF or WebP, still or animated. What a file is, and how big,
is read from its bytes: nothing a maker says about it is taken on trust.

## Taking a pack down

Whoever is at the machine can:

```sh
docker compose exec stickers wyvrn-stickers packs
docker compose exec stickers wyvrn-stickers remove cats-being-cats
```

A pack that is taken down is gone for everyone. Its pictures are deleted, and
its name stays taken, so nobody else can pass for it.

## What it does not do yet

- **No reporting.** There is no way for someone who sees a pack to report it:
  taking one down starts with someone telling whoever runs the service.
- **No rate limits.** Put one in front of it.
- **It sees who fetches what.** Fetching a pack tells the service the address
  that asked. It is not told who that is, and sees nothing of the conversation
  a sticker was sent in.
- **"Keep it to myself" is a courtesy.** A pack marked that way asks other
  people's apps not to offer it. Anyone sent one of its stickers has the
  picture.

## Configuration

`config/default.toml`, or the file `--config` or `$STICKERS_CONFIG` names. The
environment overrides `STICKERS_BIND`, `STICKERS_PUBLIC_URL`,
`STICKERS_DID_METHOD` and `STICKERS_ADMINS`. A `[mediator]` section has messages
to the service arrive through a mediator instead of at `public_url`; packs are
fetched over HTTPS regardless.

## Development

```sh
cargo test -p wyvrn-stickers
```

`tests/stickers.rs` runs the server and has makers go through each workflow
over DIDComm as an app would, then fetches what was published.
