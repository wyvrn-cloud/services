# wyvrn-community

A community server: a set of channels that the people let in, and nobody else,
read and post in. To an app it is a contact that says, when asked what it
supports, that it is one.

It speaks [`https://wyvrn.app/community/1.0`](https://github.com/wyvrn-cloud/protocols/blob/community/protocols/community/1.0/readme.md),
and [`workflow/1.0`](https://didcomm.org/workflow/1.0) for answering questions.

- **By invitation.** Only a DID on its list is answered. Anyone else is told
  they aren't on it, and nothing of what is in it. An admin lets people in.
- **Channels.** An admin opens, renames and closes them (members too, if the
  community says so). What is posted in one is sent to every member whose app
  has joined, and kept, so that it can be read back.
- **Questions.** A post can ask up to four questions, each with a few answers
  to choose from. A member's app answers with a small form; the first to
  finish has answered it, and the answer is posted in their name.
- **It knows nothing of who its members are for.** A program that posts on
  something's behalf (a bot) is a member like any other, let in the same way.

It does not show its channels on the web, and has no page that says who is in it.

## Running it

```sh
COMMUNITY_ADMINS="did:peer:4zQm..." docker compose up community
```

from the root of this repository. `COMMUNITY_ADMINS` is whoever runs it: the ID
their app shows for them. Without one nobody is let in, and the server says so
when it starts. Its own DID is logged at start-up, and is what members add as a
contact.

Without Docker: `cargo run` in this directory (it reads `config/default.toml`,
and keeps its data in `data/`).

| Setting | What it is |
|---|---|
| `COMMUNITY_PUBLIC_URL` | Where the server is reached. |
| `COMMUNITY_DID_METHOD` | `peer` (the default), or `web`: the server is then the `did:web` at its public URL, which has to be HTTPS. |
| `COMMUNITY_NAME` | What the community is called. |
| `COMMUNITY_ADMINS` | DIDs let in as admins, separated by commas. |
| `COMMUNITY_MEMBERS` | DIDs let in as members. |

Both lists are added to what the server holds each time it starts. They never
take anyone out: that is done from an admin's app, or here.

A phone app won't send to a plain `http://` address, and a server at home has
none the phone can reach. Either put it behind HTTPS, or have it reached
through a mediator: the `[mediator]` section of `config/default.toml`.

## Who is let in

```sh
docker compose exec community wyvrn-community members
docker compose exec community wyvrn-community add "did:peer:4zQm..."          # a member
docker compose exec community wyvrn-community add "did:peer:4zQm..." admin
docker compose exec community wyvrn-community remove "did:peer:4zQm..."
docker compose exec community wyvrn-community channels
```

Someone whose devices change gets a new DID. Their app tells the server, signed
by the old one, and they stay who they were: nobody needs letting in again.

## What it keeps

The `community-data` volume holds the server's own keys, the list of members,
every channel and everything said in them. **Posts are kept as they were sent:**
the server can read them. A channel here is not end-to-end encrypted between its
members; each member's messages are encrypted to the server, and the server's to
each member.

## In the app

Add the server's DID as a contact. Someone on its list sees the community in
their chats with its channels under it; someone who isn't sees "Waiting to be
let in". In a channel, `/ask Which one? | This | That` posts a question with
those answers (`/ask*` lets several be chosen).

## Not built

- Editing or deleting a post, reactions, and replies.
- Limits on how fast anyone posts.
- Removing someone does not tell their app; it is told the next time it asks.
