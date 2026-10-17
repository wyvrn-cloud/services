# wyvrn-files

A files service: the `host` role of
[`files/1.0`](https://github.com/wyvrn-cloud/protocols/blob/master/protocols/files/1.0/readme.md).
Agents it allows put files too big for a message here, over HTTPS in chunks
that resume, and hand them to anyone as a DIDComm attachment by reference
(`links` + `hash`). It keeps bytes and serves bytes; it doesn't look at them.

The first use is a build machine getting binaries to a headset and
recordings back ([`didcomm-rpc`](https://github.com/frostyfrog/didcomm-rpc)).

## How a file gets here

1. The uploader hashes the file and sends `upload` (name, media type, size,
   hash). The service checks it may, that the file isn't too large and that
   it fits the uploader's quota, and answers `upload-granted` with a `PUT`
   address.
2. The uploader `PUT`s the bytes at that address, at most `chunk_bytes` at a
   time, each with `Upload-Offset`. A `HEAD` says how much is there, so a
   dropped transfer carries on from where it stopped. Asking `upload` again
   for the same file gets the same address and the offset.
3. `complete`: the service hashes what it has, and if it is what was
   promised, answers `completed` with the download address. The file is now
   served at `/f/<id>` to anyone with the link, with `Range`, until it
   expires (14 days unless asked otherwise, at most 90).

`extend`, `delete` and `list` do the rest. Every message has to be
sender-authenticated; the protocol's readme has the shapes.

## Running

```sh
cargo run                      # at http://localhost:8080/, as a did:peer:4, nobody allowed yet
FILES_UPLOADERS=did:peer:4zQm… cargo run
docker compose up files        # from the repository root
```

A real one is a `did:web` at its own domain over HTTPS, behind a web server
that passes the domain to it ([`deploy/services`](../../deploy/services)).

## Settings

`config/default.toml`, and the environment for what a deployment changes:

| | |
|---|---|
| `FILES_BIND` | Where it listens. `0.0.0.0:8080`. |
| `FILES_PUBLIC_URL` | Where it is reached: its DIDComm endpoint, and the start of every file's address. Part of every link it hands out, so it can't change afterwards. |
| `FILES_DID_METHOD` | `peer` or `web`. |
| `FILES_UPLOADERS` | Who may put files here, separated by commas: the DID an agent writes from, or a public address at a DID host (`did:web:id.example:frosty:claude`), matched by the key its holder writes with. Nobody, until someone is named. |
| `FILES_QUOTA_BYTES` | How much each uploader may keep in all. 10 GiB. |
| `FILES_MAX_FILE_BYTES` | The largest file. 4 GiB. |
| `FILES_DIR` | Where the bytes are. `data/files`. |
| `FILES_DATABASE_URL` | Its records (and its keys) in MySQL or MariaDB instead of SQLite: `mysql://user:password@host/database`, a database of its own ([the root README](../../README.md)). The bytes are on disk either way. |
| `FILES_PUBLIC_ADDRESS` | An address for it at the DID host (`did:web:id.example:official:files`), to be known by. The name is set aside for it first: `wyvrn-did-host assign official:files did:web:files.example`. |
| `FILES_PROFILE_NAME`, `_DESCRIPTION`, `_PICTURE` | Who it says it is to someone looking its DID up. |

The chunk size (64 MiB), how long a file is kept by default and at most, how
long an unfinished upload is kept (a day) and how many an uploader may have
going (16) are in `config/default.toml`'s `[service]`.

## Commands

On the same store, with the service running or not:

```sh
wyvrn-files files           # every file: id, state, name, bytes held/promised, expiry, owner
wyvrn-files remove <id>     # take one away
wyvrn-files prune           # remove what is past its time now (the service does this every 10 minutes)
```

## What it keeps

The records (whose each file is, what it is, when it goes) in its database;
the bytes as one file per id under `FILES_DIR`. The two go together: a record
without its bytes is an upload that hasn't started, and bytes without a
record are never served. Back up both, or neither: what is here is meant to
expire.

The upload address and the download address are the file's id, which is
unguessable and told only inside encrypted messages. `PUT` is refused once a
file is complete; `delete` and `extend` are DIDComm messages, authenticated
by DID. An uploader that wants the host unable to read a file encrypts it
before upload and sends the key in the message that shares it
(`chat-message/1.0`'s `ciphering`); nothing here changes for that.

## Tests

```sh
cargo test -p wyvrn-files
WYVRN_TEST_DATABASE_URL=mysql://root:test@127.0.0.1:13306/mysql cargo test -p wyvrn-files   # against MySQL
```

`tests/files.rs` runs the service in the process with an uploader over
DIDComm and HTTP: chunks, a wrong offset, a chunk too long, resuming,
completing, fetching whole and by range, a stranger, an anonymous message,
a hash that doesn't match, and the limits.
