# wyvrn-claude

Rooms in which the people let in talk with Claude together, over DIDComm.

It is a community server (`community/1.0`; `services/community`) whose every
channel is a room with a Claude session in it. Any Wyvrn app joins it as it joins
a community: adding the service by its DID, then reading and posting in its rooms
as in channels. Every post in a room is answered by Claude, in the post's thread
where it is in one, from that room's (or thread's) latest posts. The answer is a
post of the server's own DID, signed "Claude". The server says so to everyone it
lets in (`community`'s `assistant`), and the app says it where posts are written:
**everything posted in a room is sent to Anthropic**.

## Running

```
ANTHROPIC_API_KEY=... CLAUDE_ADMINS=did:... docker compose --profile claude up -d
```

or `cargo run -p wyvrn-claude` with `config/default.toml`. The operator's key pays
for every room. Settings beyond a community server's (`[assistant]`; the
environment overrides the first four):

| Setting | Environment | Default | |
|---|---|---|---|
| `api_key` | `ANTHROPIC_API_KEY` | none | Without one, rooms say Claude couldn't answer. |
| `model` | `CLAUDE_MODEL` | `claude-sonnet-5-5` | |
| `daily_requests_per_member` | `CLAUDE_DAILY_REQUESTS` | 50 | Answers one member may ask for a day; 0 for no limit. |
| `period_tokens` | `CLAUDE_PERIOD_TOKENS` | 0 | Tokens, in and out, the whole service may use in 30 days; 0 for no limit. |
| `name` | | `Claude` | What its posts are signed as. |
| `system` | | empty | Said to Claude before every room's conversation, besides where it is. |
| `max_tokens` | | 2048 | The longest answer. |
| `context_posts` | | 40 | How many of a room's latest posts Claude is given. |

Who may use it is who the community lets in: its admins (`COMMUNITY_ADMINS`, or
`CLAUDE_ADMINS` in docker-compose.yml) choose admission in the app (by an admin,
by invite, or open) and manage members and rooms, as in any community. A member
over their day's limit, or the service over its period's, is told so in the room
and the model isn't asked. `wyvrn-claude usage` prints the period's tokens.
