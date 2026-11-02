# What to add on the server

quickbuild.sh copies this file to the server on each run of its `services` part. It lists what the server needs before the newest images can run: the additions to the compose file, `.env`, Caddy and Anubis. Anything here already on the server can be skipped.

The images themselves arrive with each run: `wyvrn-did-host`, `wyvrn-stickers`, `wyvrn-community`, `wyvrn-announce`, `wyvrn-cards`, `wyvrn-files`, `wyvrn-relay`, `wyvrn-verify`, and new since 2026-11-02, `wyvrn-community-host` and `wyvrn-claude`.

The full compose blocks are in `docker-compose.yml` beside this file in wyvrn-services. Below, each is given as it goes in a compose file of the server's own (images carried over, `image:` and no `build:`).

## 1. The community web view (in `wyvrn-community`, no new image)

Each community server now answers a browser at its own address with its web view: its page, its public channels, and signing in. The web view of a community that existed before this release stays off until an admin turns it on in the app (Admin, On the web).

**.env**

```sh
# Where the web view is, if not at the community's own address (another origin,
# or a path: https://wyvrn.app/community/). Empty: the community's own address.
COMMUNITY_WEB_URL=
# The proxies whose X-Forwarded-For, -Proto and -Prefix the server believes:
# the Docker networks, where Caddy's (or Anubis's) requests come from.
COMMUNITY_WEB_TRUSTED_PROXIES=172.16.0.0/12
```

**Compose**, in the `community` service's `environment`:

```yaml
      COMMUNITY_WEB_URL: "${COMMUNITY_WEB_URL:-}"
      COMMUNITY_WEB_TRUSTED_PROXIES: "${COMMUNITY_WEB_TRUSTED_PROXIES:-172.16.0.0/12}"
```

**Anubis.** The pages are for browsers and can be behind its challenge. These can't, because what asks for them isn't a browser:

| Path | Who asks |
|---|---|
| `POST /`, and WebSocket upgrades on `/` | Apps (DIDComm). Already let through if the community works today. |
| `/.well-known/openid-configuration`, `/.well-known/oauth-authorization-server` | Other websites' servers, signing in with the community |
| `/oauth/token`, `/oauth/userinfo`, `/oauth/jwks`, `/oauth/revoke`, `/oauth/register` | The same |
| `/voice/auth` | The voice relay (moq-relay) |
| `/u/*`, `/f/*`, `/s/*` | Apps: files members attach, and the community's sticker packs |
| `/robots.txt`, `/sitemap.xml` | Search engines, if the pages are to be found. Check that Anubis's bot policy lets the crawlers you want through. |

`/oauth/authorize` and `/sign-in/*` are opened in the person's own browser and can stay behind the challenge.

With an Anubis new enough for CEL expressions (1.18 and later), in its bot policy, above the catch-all rules:

```yaml
- name: wyvrn-community-machines
  action: ALLOW
  expression:
    any:
      - method == "POST" && path == "/"
      - headers["Upgrade"] == "websocket"
      - path.startsWith("/.well-known/")
      - path.startsWith("/oauth/") && path != "/oauth/authorize"
      - path == "/voice/auth"
      - path.startsWith("/u/") || path.startsWith("/f/") || path.startsWith("/s/")
```

With an older one, which matches paths only (`path_regex`), the paths can still be let through, except `/` itself: let the domain's `POST` requests reach the service without Anubis in Caddy instead (a `@didcomm method POST` matcher before the Anubis `reverse_proxy`).

## 2. The community host (`wyvrn-community-host`)

Many communities in one process, each at `https://<its domain>/c/<name>/` with a did:web of its own there. Its admins make, change and remove communities from the app. Each community gets a MySQL database of its own.

**DNS**: a domain for it, say `communities.wyvrn.app`. It is part of every community's DID, so choose it once.

**MySQL**:

```sql
CREATE DATABASE wyvrn_community_host;
GRANT ALL ON wyvrn_community_host.* TO 'wyvrn'@'%';
-- Each community's own database, made when the community is: wyvrn_c<its id>.
GRANT ALL ON `wyvrn_c%`.* TO 'wyvrn'@'%';
```

**.env**

```sh
COMMUNITY_HOST_DOMAIN=communities.wyvrn.app
# Who may make, change and remove communities (DIDs, or public addresses), by commas.
COMMUNITY_HOST_ADMINS=
COMMUNITY_HOST_DATABASE_URL=mysql://wyvrn:password@host.docker.internal:3306/wyvrn_community_host
# The MySQL server itself, with no database: each community's is made on it.
COMMUNITY_HOST_MYSQL_URL=mysql://wyvrn:password@host.docker.internal:3306
# Voice channels for every community, through one moq-relay (optional).
COMMUNITY_HOST_VOICE_RELAY_URL=
COMMUNITY_HOST_VOICE_RELAY_FINGERPRINT=
```

**Compose**:

```yaml
  community-host:
    image: wyvrn-community-host
    restart: unless-stopped
    extra_hosts:
      - "host.docker.internal:host-gateway"
    ports:
      - "127.0.0.1:8189:8095"
    environment:
      COMMUNITY_HOST_PUBLIC_URL: "https://${COMMUNITY_HOST_DOMAIN}/"
      COMMUNITY_HOST_DID_METHOD: "web"
      COMMUNITY_HOST_ADMINS: "${COMMUNITY_HOST_ADMINS:-}"
      # The DID host each community holds its public address at (community:<name>).
      COMMUNITY_HOST_DID_HOST: "id.wyvrn.app"
      COMMUNITY_HOST_DATABASE_URL: "${COMMUNITY_HOST_DATABASE_URL}"
      COMMUNITY_HOST_STORAGE: "mysql-db"
      COMMUNITY_HOST_MYSQL_URL: "${COMMUNITY_HOST_MYSQL_URL}"
      COMMUNITY_HOST_VOICE_RELAY_URL: "${COMMUNITY_HOST_VOICE_RELAY_URL:-}"
      COMMUNITY_HOST_VOICE_RELAY_FINGERPRINT: "${COMMUNITY_HOST_VOICE_RELAY_FINGERPRINT:-}"
    # Each open community holds a few files open, and each member's live connection one.
    ulimits:
      nofile:
        soft: 65536
        hard: 65536
    volumes:
      # Members' attachments, for every community.
      - community-host-data:/app/data
```

and `community-host-data:` under the file's top-level `volumes:`.

**Caddy**:

```
communities.wyvrn.app {
	encode gzip
	reverse_proxy 127.0.0.1:8189
}
```

(or through Anubis, as the other sites are).

**Anubis**: as for a community (section 1), with every path also under `/c/<name>/`: `POST /c/<name>/` and WebSocket upgrades, `/c/<name>/.well-known/*`, `/c/<name>/oauth/*` but `authorize`, `/c/<name>/u/*`, `/f/*` and `/s/*`. Also `POST /` (the host's own DIDComm, for its admins), `/voice/auth`, and `/export/*` (a community moving out to a server of its own fetches its export). In CEL:

```yaml
- name: wyvrn-community-host-machines
  action: ALLOW
  expression:
    any:
      - method == "POST" && (path == "/" || path.matches("^/c/[^/]+/?$"))
      - headers["Upgrade"] == "websocket"
      - path.matches("^(/c/[^/]+)?/\\.well-known/")
      - path.matches("^(/c/[^/]+)?/oauth/") && !path.endsWith("/oauth/authorize")
      - path == "/voice/auth" || path.startsWith("/export/")
      - path.matches("^/c/[^/]+/(u|f|s)/")
```

**Check**: `curl https://communities.wyvrn.app/healthz` says `ok`.

## 3. Claude rooms (`wyvrn-claude`)

A community server whose rooms Claude answers in, on the operator's Anthropic API key. Everything posted in a room is sent to Anthropic, and the app tells every member so. Who may use it is who the community lets in, as in any community.

**DNS**: a domain, say `claude.wyvrn.app`.

**MySQL**:

```sql
CREATE DATABASE wyvrn_claude;
GRANT ALL ON wyvrn_claude.* TO 'wyvrn'@'%';
```

**.env**

```sh
CLAUDE_DOMAIN=claude.wyvrn.app
CLAUDE_NAME=Claude rooms
# Nobody is let in until someone is named here (DIDs, or public addresses).
CLAUDE_ADMINS=
CLAUDE_MEMBERS=
CLAUDE_DATABASE_URL=mysql://wyvrn:password@host.docker.internal:3306/wyvrn_claude
# The key that pays for every room. Without one, rooms say Claude couldn't answer.
ANTHROPIC_API_KEY=
# Optional: the model, answers a member may ask for a day (50), and tokens the
# whole service may use in 30 days (0: no limit).
CLAUDE_MODEL=
CLAUDE_DAILY_REQUESTS=
CLAUDE_PERIOD_TOKENS=
```

**Compose**:

```yaml
  claude:
    image: wyvrn-claude
    restart: unless-stopped
    extra_hosts:
      - "host.docker.internal:host-gateway"
    ports:
      - "127.0.0.1:8190:8080"
    environment:
      COMMUNITY_PUBLIC_URL: "https://${CLAUDE_DOMAIN}/"
      COMMUNITY_DID_METHOD: "web"
      COMMUNITY_NAME: "${CLAUDE_NAME:-Claude rooms}"
      COMMUNITY_ADMINS: "${CLAUDE_ADMINS:-}"
      COMMUNITY_MEMBERS: "${CLAUDE_MEMBERS:-}"
      COMMUNITY_DATABASE_URL: "${CLAUDE_DATABASE_URL}"
      ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY:-}"
      CLAUDE_MODEL: "${CLAUDE_MODEL:-}"
      CLAUDE_DAILY_REQUESTS: "${CLAUDE_DAILY_REQUESTS:-}"
      CLAUDE_PERIOD_TOKENS: "${CLAUDE_PERIOD_TOKENS:-}"
```

**Caddy**: as for the community host, to `127.0.0.1:8190`.

**Anubis**: as for a community (section 1). It is one.

**Check**: `curl https://claude.wyvrn.app/healthz` says `ok`, and `docker compose logs claude` doesn't say "No ANTHROPIC_API_KEY".
