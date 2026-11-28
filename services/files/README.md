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
| `FILES_ADMINS` | Who runs it, separated by commas: DIDs, or public addresses as for uploaders. They may always upload (under the admins' limits), and they alone see its admin pages. |
| `FILES_OPEN_SIGNUP` | `true`: anyone who authenticates becomes an uploader at their first upload. Off unless set. |
| `FILES_TOTAL_BYTES` | The most it holds of everyone's. 0 (the default): no limit but the disk. |
| `FILES_QUOTA_BYTES` | How much each uploader may keep in all. 10 GiB. |
| `FILES_MAX_FILE_BYTES` | The largest file. 4 GiB. |
| `FILES_DIR` | Where the bytes are. `data/files`. |
| `FILES_DATABASE_URL` | Its records (and its keys) in MySQL or MariaDB instead of SQLite: `mysql://user:password@host/database`, a database of its own ([the root README](../../README.md)). The bytes are on disk, or in a bucket (below), either way. |
| `FILES_BUCKET_ENDPOINT`, `_NAME`, `_REGION`, `_PREFIX`, `_DOWNLOADS` | An S3-compatible bucket for stored files instead of `FILES_DIR` (or `[bucket]` in the config file). See "In a bucket". |
| `FILES_BUCKET_KEY`, `FILES_BUCKET_SECRET` | Its access key and secret. Only ever from the environment (or the variables `[bucket]`'s `key_env` and `secret_env` name). |
| `FILES_PUBLIC_ADDRESS` | An address for it at the DID host (`did:web:id.example:official:files`), to be known by. The name is set aside for it first: `wyvrn-did-host assign official:files did:web:files.example`. |
| `FILES_PROFILE_NAME`, `_DESCRIPTION`, `_PICTURE` | Who it says it is to someone looking its DID up. |

Whether sign-up is open, the total, and the limits (quota, largest file, longest
keep, uploads at once) are where the service starts. An admin changes them while
it runs, from its admin pages in the app; what an admin sets is kept in the
database and wins from then on. There they can also lock sign-up down (nobody
new, everyone in carries on), refuse uploaders it has no profile for (they are
asked for one, through user-profile/1.0), give admins limits of their own, set an
uploader's limits in place of their tier's, block or remove an uploader, and
look through every file, the largest first.

The chunk size (64 MiB), how long a file is kept by default and at most, how
long an unfinished upload is kept (a day) and how many an uploader may have
going (16) are in `config/default.toml`'s `[service]`.

## Commands

On the same store, with the service running or not:

```sh
wyvrn-files files           # every file: id, state, where it is, name, bytes held/promised, expiry, owner
wyvrn-files remove <id>     # take one away
wyvrn-files prune           # remove what is past its time now (the service does this every 10 minutes)
wyvrn-files places          # how many files, and bytes, are on disk and in each bucket
wyvrn-files bucket-check [<origin>]   # write, read back and remove a test file; and whether apps may fetch from it
wyvrn-files move-to-bucket  # move every file not in the bucket there, hashes checked before and after
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

## In a bucket

With a bucket configured (Linode Object Storage, MinIO, AWS S3, anything that
speaks S3), stored files are kept there instead of on disk:

```toml
[bucket]
endpoint = "https://us-east-1.linodeobjects.com"
name = "wyvrn-files"
region = "us-east-1"
prefix = "files"          # a folder of the bucket for this service
downloads = "redirect"    # or "proxy"
link_seconds = 300
```

- **Uploads still land on disk**, a chunk at a time as ever, and are checked
  there. Once stored, a file is moved to the bucket in the background (every 15
  seconds) and removed from disk. Apps don't change, old ones included.
- **Downloads** (`/f/<id>`) are a `302` to a link signed for `link_seconds`, so
  the bytes come straight from the bucket and never pass through this machine.
  That matters for the bill: object storage charges for what leaves it to a
  server too (on Linode, within the same data centre as well), so passing files
  on would pay for every byte twice. `downloads = "proxy"` passes them on
  instead, Range and all, if the bucket must not be seen.
- **The bucket must allow CORS** for redirects: the apps follow a download link
  with the page's own `fetch`, from another origin. Allow `GET` and `HEAD` from
  any origin with the `Range` header, exposing `Content-Range`, `Content-Length`
  and `ETag` (Linode's Cloud Manager, or `s3cmd setcors`). `wyvrn-files
  bucket-check` says whether it does. MinIO allows every origin by itself.
- **Files from before** stay where they were and are still served from there.
  `wyvrn-files move-to-bucket` moves them, each checked against its hash before it
  goes and read back from the bucket after, and only then removed from disk; one
  that doesn't match is left as it is and named. Moving to another bucket is the
  same: name the old one under `[[former_buckets]]` (with `key_env` and
  `secret_env`), set the new one as `[bucket]`, and move.
- A file deleted (or expired) goes at once here and is removed from the bucket
  in the background.

## Tests

```sh
cargo test -p wyvrn-files
WYVRN_TEST_DATABASE_URL=mysql://root:test@127.0.0.1:13306/mysql cargo test -p wyvrn-files   # against MySQL
# Against a bucket (tests/bucket.rs; skipped without one):
docker run -d --rm --name wyvrn-minio-test -p 127.0.0.1:19000:9000 -e MINIO_ROOT_USER=wyvrntest -e MINIO_ROOT_PASSWORD=wyvrntest-secret minio/minio server /data
WYVRN_TEST_BUCKET=http://127.0.0.1:19000 WYVRN_TEST_BUCKET_KEY=wyvrntest WYVRN_TEST_BUCKET_SECRET=wyvrntest-secret cargo test -p wyvrn-files --test bucket
```

`tests/files.rs` runs the service in the process with an uploader over
DIDComm and HTTP: chunks, a wrong offset, a chunk too long, resuming,
completing, fetching whole and by range, a stranger, an anonymous message,
a hash that doesn't match, and the limits.
