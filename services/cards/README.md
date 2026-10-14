# wyvrn-cards

What a link to someone looks like where it is posted.

The app hands out two kinds of link: to a public address
(`https://chat.example/dl/did:web:id.example:alice`) and carrying a contact
invite (`https://chat.example/dl?_oob=…`). Both open a page of the web app, which
a person's browser runs. A chat or a social network that is shown the link runs
nothing: it fetches the address once and reads the page's
[OpenGraph](https://ogp.me) tags. This service answers that fetch.

| Path | What it answers |
| --- | --- |
| `/dl/<did>` | A page of tags saying who a public address is: its name, where it is hosted, its description, and the card below. |
| `/dl?_oob=…` | The same for a contact invite, from what the invite carries. |
| `/card/a/<did>.png` | An address's card: 1200 by 630, with its picture, name, address, description, banner and background. |
| `/card/i/<id>.png` | An invite's card, for an hour after its page was asked for. |
| `/healthz` | `ok`. |

It is not a DIDComm service. It has no identity, signs nothing and keeps
nothing; what it remembers (who an address was, for five minutes) is in memory.

## What a card says

An address is looked up the way an app looks one up before adding it
([`wyvrn-hosted-did`](../../crates/wyvrn-hosted-did)): resolved, its history
verified where it has one, and its profile checked against it. The card then
shows what the app's own card does:

- the profile's name, and under it `alice via id.example`: the name is the
  holder's own word, where it is hosted is not;
- **History verified** for a `did:webvh` whose log checked out, **On its host's
  word** for a bare `did:web`;
- **Official** for an `official:` name at `CARDS_DID_HOST`, or the `did:web` that
  is one of `CARDS_OFFICIAL_DOMAINS`. Nothing an address says about itself earns it.

Only pictures that are inside the profile are drawn. One a profile names
elsewhere is left out: this service fetches a DID's own files and nothing else.

Nothing in an invite is signed, so an invite's card marks nothing: it shows the
name and picture the invite carries, and says it is an invite.

An address that can't be read still gets a page (it is a link to somewhere),
with no picture.

## Running it

```sh
docker compose up cards     # from the root of this repository: http://localhost:8085/
curl http://localhost:8085/dl/did:web:id.wyvrn.app:<name>
```

Everything is optional:

| Setting | |
| --- | --- |
| `CARDS_BIND` | Where to listen. `0.0.0.0:8080`. |
| `CARDS_DID_HOST` | The DID host whose `official:` names are official. `id.wyvrn.app`. |
| `CARDS_OFFICIAL_DOMAINS` | The domains whose own `did:web` is official, separated by commas. The DID host. |
| `CARDS_SITE_NAME` | What the links' site is called. `Wyvrn`. |
| `CARDS_PUBLIC_URL` | Where the links point (`https://chat.example`), if not where the proxy says a request came to (`X-Forwarded-Proto`, `Host`). |

## In front of the web app

The `/dl` paths are the web app's. A person's browser is to get the app there,
and only what fetches a link to show it gets this service, told apart by
`User-Agent` in the web server: see
[`deploy/services`](../../deploy/services/README.md#link-cards) and its
`Caddyfile.example`. The service itself answers whoever it is sent.

## What it will and won't fetch

A DID names any host it likes, so the service asks only public ones: over HTTPS,
following no redirects, never an address written as a number, and never a name
that resolves to this machine or its network. A file is read up to 512 KB and
an address in at most eight files.

It limits nothing else. Put a rate limit in front of it, as for the other
services.

## Text

Names are set in DejaVu Sans, which is built in (`fonts/`, with its licence).
For what that has no letters for, the machine's own fonts are used: the image
has Noto Color Emoji, and more scripts are a matter of installing or mounting
more fonts under `/usr/share/fonts`.
