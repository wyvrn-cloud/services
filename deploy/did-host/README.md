# Running a DID host at your own domain

This sets up [`wyvrn-did-host`](../../services/did-host) in Docker, at a domain
with HTTPS, so that people can get public addresses like
`did:web:id.example:alice` from it. It then covers linking a website to one of
those addresses, which is what the app shows as "Website verified".

You need:

- A machine with Docker, reachable from the internet on ports 80 and 443.
- A domain name for the host (`id.example` below), with DNS you control.

## 1. Choose the domain, and mean it

The domain is part of every address the host gives out. `alice` at `id.example`
is `did:web:id.example:alice`, and is fetched from
`https://id.example/alice/did.json`. Changing the domain later breaks every
address already handed out, so pick one you will keep.

Use a domain or subdomain that serves nothing else. The host answers every
path under it.

Point the domain's DNS (an `A` record, and `AAAA` if the machine has IPv6) at
the machine before going on. Caddy gets the certificate on first start, and
that fails if the domain doesn't reach it yet.

## 2. Configure

```sh
cd deploy/did-host
cp .env.example .env
```

Edit `.env`:

| Setting | What it is |
|---|---|
| `DID_HOST_DOMAIN` | The domain, as above. Nothing else: no `https://`, no path. |
| `DID_HOST_ADMINS` | The DIDs that decide who may claim a reserved name (`official:*`, `admin`, `support`, ...), separated by commas. An admin's DID is the ID their app shows for them. Leave empty if you will only do that from this machine (step 5). |

## 3. Start it

```sh
docker compose up -d
```

The first start builds the host from source, which takes several minutes.
Two containers run: `did-host`, and `caddy` in front of it holding the
certificate.

## 4. Check it

```sh
curl https://id.example/healthz                # ok
curl https://id.example/did                    # did:web:id.example
curl https://id.example/.well-known/did.json   # the host's own DID document
curl https://id.example/.well-known/did-configuration.json   # the host saying the domain is its own
```

If the first fails with a certificate error, look at `docker compose logs
caddy`: the usual cause is DNS that doesn't point here yet, or port 80 closed.

Then try it from the app. In a build of Wyvrn, Settings, Profile, **Public
address**: put `did:web:id.example` in **From** (the domain alone will do), pick
a name, and choose **Get this address**. Afterwards these exist:

```sh
curl https://id.example/<name>/did.jsonl   # the address's signed history
curl https://id.example/<name>/did.json    # the same as a did:web
curl https://id.example/<name>/whois.vp    # the profile, if one was published
```

The app is never given the host's address. It is given the host's DID, looks up
that DID's document (the one at `/.well-known/did.json`), and writes to wherever
the document says. So the host has to be a `did:web`, which this setup makes it
(`DID_HOST_DID_METHOD=web`).

To make a build of the app use this host without typing it, build with
`VITE_DID_HOST_DID=did:web:id.example`. That also makes names under `official:`
at this host the ones it marks as official.

### The host's own DID, and "Official"

The host is itself `did:web:id.example`. It links that DID to its own domain
without being asked: its DID document names `https://id.example`, and it
serves a `did-configuration.json` it signs itself, afresh each day. Someone who
looks the host up in the app is shown the domain, and can check it.

Whether the app also marks it **Official** is the app's decision, not the
host's: a build of the app lists the domains that are the project's own in
`VITE_OFFICIAL_DOMAINS` (by default the DID host and the sticker service it
was built for), and marks the `did:web` at each. Nothing a host says about
itself puts it on that list.

## 5. Reserved names

Names anyone would mistake for the host itself are reserved, and so is
everything under `official:`. A reserved name can be claimed only by the DID it
has been assigned to:

```sh
docker compose exec did-host wyvrn-did-host names
docker compose exec did-host wyvrn-did-host assign official:news "did:peer:4zQm..."
docker compose exec did-host wyvrn-did-host free alice
```

`assign` only says who may claim the name. That person then gets it from their
app like any other, and from there the address is theirs: nobody at the host
can change it, only free the name, which ends its hosting.

## 6. Keep it

- **Back up the `did-host-data` volume.** It holds the host's own keys and the
  history of every address. An address whose history is lost can be put back
  only by its holder.
- **Updating:** `git pull && docker compose up -d --build`. The build keeps its
  work between runs (a Docker cache mount), so after the first, which takes
  several minutes, a change is rebuilt in well under one.
- **There are no rate limits.** Nothing here stops someone reserving names in
  bulk. If the host is open to the public, put a rate limit in front of it.

## Linking a website

Someone with a public address and a website of their own can link the two. The
app then shows people who look the address up that both are run by the same
person: "Website verified alice.example". This is
[Well Known DID Configuration](https://identity.foundation/well-known-did-configuration/),
and it works for any website, wherever it is hosted. It has nothing to do with
the DID host's own domain.

It takes two halves, and neither counts alone:

1. **The address names the website.** In the app: Settings, Profile, Public
   address, **Your website**. Enter the domain and choose **Link this
   website**.
2. **The website names the address.** The app then offers a file,
   `did-configuration.json`. Save it and serve it at exactly

   ```
   https://alice.example/.well-known/did-configuration.json
   ```

Back in the app, **Check** says whether the website is answering with it.

### What the website has to do

- Serve the file over **HTTPS**, at that path, at the **same domain** that was
  entered. `www.alice.example` and `alice.example` are different websites: link
  the one people know.
- Send the header **`Access-Control-Allow-Origin: *`** with it. Without that
  the web app is not allowed to read the file and reports that the website
  couldn't be asked. The installed apps don't need it.
- Not cache it for long, or a replaced file goes unseen.

Served from this machine, with the Caddy that is already running:

```sh
cp sites/website.caddy.example sites/alice.caddy     # then change example.com in it
mkdir -p linked/alice.example/.well-known
cp ~/did-configuration.json linked/alice.example/.well-known/
docker compose restart caddy
```

Served by nginx somewhere else:

```nginx
location = /.well-known/did-configuration.json {
    add_header Access-Control-Allow-Origin "*";
    add_header Cache-Control "no-cache";
}
```

On a static host, put the file at `.well-known/did-configuration.json` in the
site and check two things: that the host publishes directories whose names
start with a dot (some skip them unless told otherwise), and that it sends the
`Access-Control-Allow-Origin` header.

Check it from anywhere:

```sh
curl -i https://alice.example/.well-known/did-configuration.json
```

### When the file has to be made again

The file is signed by the device that made it, and lasts five years. It stops
counting, and has to be made again from the app and replaced, when:

- that device is removed from the account (its key leaves the address);
- the address is given up and a new one got;
- five years pass.

Adding a device, or changing the profile, does not affect it.

Unlinking in the app is enough to end the link. The file left on the website
then means nothing.

### What is and isn't checked

The app checks, each time someone chooses to: that the address's own signed
history names the website; that the website serves a file naming the address
and that website; that the file is in date; and that it is signed by a key the
address has now.

It asks the website only when a person chooses **Check**, because asking tells
the website that someone is looking at that address.

A verified website shows that whoever holds the address also controls that
website. It says nothing about who that is. `alice.example` being verified
means what `alice.example` means to the person looking.

## Trying the setup without a domain

To check the containers and the proxy on a machine that isn't at the domain,
add to `.env`:

```
DID_HOST_DOMAIN=id.localhost.test
DID_HOST_TLS=internal
HTTP_PORT=8080
HTTPS_PORT=8443
```

and ask it by name, accepting a certificate nothing trusts:

```sh
curl -k --resolve id.localhost.test:8443:127.0.0.1 https://id.localhost.test:8443/healthz
```

Nothing hosted this way resolves from an app. It is for seeing that the setup
runs.
