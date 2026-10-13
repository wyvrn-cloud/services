# Running the services behind a web server you already have

For a machine that already has Caddy (or nginx, ...) answering for its domains
and a MySQL or MariaDB server to use. Each service runs in Docker, listens on
this machine only, and keeps what it keeps in a database of its own. Your web
server holds the certificates and passes each domain on.

With no web server yet, [`deploy/did-host`](../did-host) brings its own Caddy.

| Service | What an app expects | Needed for a release of the app? |
|---|---|---|
| `did-host` | `did:web:id.wyvrn.app`, unless built with `VITE_DID_HOST_DID` | Yes: public addresses |
| `stickers` | `did:web:stickers.wyvrn.app`, unless built with `VITE_STICKER_SERVICE_DID` | Yes: making and sending sticker packs |
| `community` | Nothing: people are given its DID | No |
| `announce` | Nothing: people are given its DID | No |

## 1. Domains

One domain or subdomain per service, serving nothing else, with DNS pointing at
this machine. A service's domain is part of every address it hands out (a
public address, a pack's address): it can't be changed afterwards.

## 2. Databases

Each service needs **a database of its own**. Their tables share names
(`members`, `posts`), so a service started on another's database refuses to
run and says whose it is. One user for all of them is fine:

```sql
CREATE DATABASE wyvrn_did_host;
CREATE DATABASE wyvrn_stickers;
-- CREATE DATABASE wyvrn_community;
-- CREATE DATABASE wyvrn_announce;
CREATE USER 'wyvrn'@'%' IDENTIFIED BY 'a long password';
GRANT ALL ON wyvrn_did_host.* TO 'wyvrn'@'%';
GRANT ALL ON wyvrn_stickers.* TO 'wyvrn'@'%';
```

A service makes its own tables the first time it starts. MySQL 8 and MariaDB 11
are what the tests are run against.

Where the database server is decides the host in each `DATABASE_URL`:

- **On this machine itself**: `host.docker.internal`. The server has to listen
  on more than `127.0.0.1` for a container to reach it (`bind-address`), and
  the user has to be allowed from the Docker network (`'wyvrn'@'%'`, or the
  network's range).
- **In another Docker Compose stack** (the mediator's, say): put these services
  on that stack's network and use its service name. Add to
  `docker-compose.override.yml`:

  ```yaml
  networks:
    default:
      name: <that stack's network>   # `docker network ls`
      external: true
  ```
- **Elsewhere**: its hostname.

## 3. Configure and start

```sh
cd deploy/services
cp .env.example .env     # the domains, the database URLs, the admins
docker compose up -d                                          # did-host and stickers
docker compose --profile community --profile announce up -d   # the other two as well
```

The first start builds from source, which takes several minutes.

### With a compose file of your own

Build the images where the source is, and carry them over:

```sh
# From the root of this repository.
docker build -f services/did-host/Dockerfile -t wyvrn-did-host .
docker build -f services/stickers/Dockerfile -t wyvrn-stickers .
docker save wyvrn-did-host wyvrn-stickers | gzip | ssh you@server 'gunzip | docker load'
```

(An image is for the processor it was built on. For a server of another kind:
`docker buildx build --platform linux/arm64 …`.)

Then a service in your compose file is its image and its settings.
Everything it needs is in the image, everything it keeps is in its database,
and it listens on 8080.

```yaml
  did-host:
    image: wyvrn-did-host
    restart: unless-stopped
    environment:
      DID_HOST_DOMAIN: "id.example"
      DID_HOST_PUBLIC_URL: "https://id.example/"
      DID_HOST_DID_METHOD: "web"
      DID_HOST_ADMINS: ""
      DID_HOST_DATABASE_URL: "mysql://wyvrn:password@mysql:3306/wyvrn_did_host"

  stickers:
    image: wyvrn-stickers
    restart: unless-stopped
    environment:
      STICKERS_PUBLIC_URL: "https://stickers.example/"
      STICKERS_DID_METHOD: "web"
      STICKERS_ADMINS: ""
      STICKERS_DATABASE_URL: "mysql://wyvrn:password@mysql:3306/wyvrn_stickers"
```

Optional, for any of them: who it says it is when someone looks its DID up
(`<SERVICE>_PROFILE_NAME`, `_PROFILE_DESCRIPTION`, and `_PROFILE_PICTURE` for a
picture mounted into the container), as
[the root README](../../README.md#who-a-service-says-it-is) has it:

```yaml
    environment:
      ANNOUNCE_PROFILE_PICTURE: "/app/profile.png"
    volumes:
      - ./updates.png:/app/profile.png:ro
```

`docker-compose.yml` here has the same for `community` and `announce`. With
Caddy in the same compose file, it reaches them by name (`reverse_proxy
did-host:8080`) and they need no `ports`.

## 4. Your web server

[`Caddyfile.example`](Caddyfile.example) has what to add to Caddy: each domain,
passed to `127.0.0.1` at the port its service listens on (8181 to 8184, or
what `.env` says). Reload Caddy, then:

```sh
curl https://id.example/did                    # did:web:id.example
curl https://id.example/.well-known/did.json   # its DID document
curl https://stickers.example/did              # did:web:stickers.example
```

That DID is what the app is given: `VITE_DID_HOST_DID` and
`VITE_STICKER_SERVICE_DID` at build time, unless the domains are
`id.wyvrn.app` and `stickers.wyvrn.app`, which it expects already.

### A rate limit

The services limit nothing themselves, and stock Caddy has no rate limit: it
comes as a module, [`caddy-ratelimit`](https://github.com/mholt/caddy-ratelimit),
that Caddy is built with.

- Caddy installed on the machine: `xcaddy build --with github.com/mholt/caddy-ratelimit`,
  and put the binary it makes in place of the one there
  (`caddy list-modules | grep rate_limit` says whether it took).
- Caddy in Docker: build its image with the module, and use that image.

  ```dockerfile
  FROM caddy:2-builder AS builder
  RUN xcaddy build --with github.com/mholt/caddy-ratelimit
  FROM caddy:2
  COPY --from=builder /usr/bin/caddy /usr/bin/caddy
  ```

`Caddyfile.example` then limits each address to 60 messages and 600 reads a
minute; past that it answers `429` with a `Retry-After`. Those numbers are a
starting point, not measured: one person making a pack sends a few messages a
minute, and opening a pack reads one picture per sticker. The counts are kept
in that Caddy's memory, so they are per Caddy and start again when it does.

## 5. Keep it

- **The databases** hold everything a service was told: back them up as you do
  the mediator's.
- That is all there is to keep: a service's own keys are in its database too
  (unencrypted, as the file they used to be in was: whoever can read the
  database can be the service). A container can be remade, or the service
  moved to another machine, with nothing carried over but its settings.
- Updating: `git pull`, `docker compose build`, `docker compose up -d`.

## A service's own commands

Run inside its container, on the same database:

```sh
docker compose exec did-host wyvrn-did-host names
docker compose exec stickers wyvrn-stickers packs
docker compose exec community wyvrn-community add <did> admin
```

Each service's README lists its commands.
