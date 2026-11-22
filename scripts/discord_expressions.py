#!/usr/bin/env python3
"""Fetch a Discord server's own custom emoji and stickers into a folder, for
`wyvrn-community-host import-expressions <community> <folder>` to make them a
community's packs.

    DISCORD_BOT_TOKEN=... python3 discord_expressions.py <guild-id> <folder> [--original]

The token is a bot's that is in the server (no permissions needed), read from
DISCORD_BOT_TOKEN and never printed or written down. Only the list of emoji and
stickers is asked of Discord's API; the pictures come from its CDN.

Sizes. Wyvrn holds a custom emoji to 128 px and 128 KB and a sticker to 512 px
and 500 KB when a member uploads one. By default each picture is fetched at a
size that fits: an emoji at 128 px, and where that is still too many bytes (a
long animation), smaller, then as animated WebP. With --original every picture
is fetched as Discord keeps it, whatever its size, and the import takes it so.

Left out, and listed in the manifest's `skipped`: Lottie stickers (Wyvrn shows
PNG, GIF and WebP), and anything that wouldn't download.

The folder gets manifest.json and the pictures (emoji/<id>.<ext>,
stickers/<id>.<ext>). Run again into the same folder, what is there is fetched
again, so it is the server as it is now.
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

API = os.environ.get("DISCORD_API", "https://discord.com/api/v10")
CDN = os.environ.get("DISCORD_CDN", "https://cdn.discordapp.com")
AGENT = "DiscordBot (https://github.com/wyvrn-cloud/services, 1.0)"

EMOJI_PX, EMOJI_BYTES = 128, 128 * 1024
STICKER_PX, STICKER_BYTES = 512, 500 * 1024
# Discord's sticker formats.
PNG, APNG, LOTTIE, GIF = 1, 2, 3, 4


def get(url, token=None, tries=5):
    """The bytes at `url`, waiting out Discord's rate limits."""
    headers = {"User-Agent": AGENT}
    if token:
        headers["Authorization"] = f"Bot {token}"
    for _ in range(tries):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if error.code == 429:
                wait = float(error.headers.get("Retry-After") or 1)
                time.sleep(min(wait, 60) + 0.1)
                continue
            if error.code in (401, 403):
                raise SystemExit(f"Discord refused the token ({error.code}): is DISCORD_BOT_TOKEN a bot's, and is the bot in the server?")
            raise
    raise RuntimeError(f"still rate limited: {url}")


def api(path, token):
    return json.loads(get(f"{API}{path}", token))


def emoji_picture(emoji, original):
    """The bytes and extension of one emoji: as Discord keeps it, or the first of
    a few sizes and formats that fits Wyvrn's."""
    base = f"{CDN}/emojis/{emoji['id']}"
    ext = "gif" if emoji.get("animated") else "png"
    if original:
        return get(f"{base}.{ext}"), ext
    tries = [(ext, f"?size={EMOJI_PX}")]
    if emoji.get("animated"):
        tries += [("webp", f"?size={EMOJI_PX}&animated=true"), ("gif", "?size=96"), ("webp", "?size=96&animated=true"), ("webp", "?size=64&animated=true")]
    else:
        tries += [("webp", f"?size={EMOJI_PX}"), ("png", "?size=96"), ("webp", "?size=64")]
    smallest = None
    for ext, query in tries:
        data = get(f"{base}.{ext}{query}")
        if len(data) <= EMOJI_BYTES:
            return data, ext
        if smallest is None or len(data) < len(smallest[0]):
            smallest = (data, ext)
    return smallest  # still too big: the import keeps it, and says so


def sticker_picture(sticker, original):
    ext = "gif" if sticker["format_type"] == GIF else "png"
    # An APNG is fetched whole: resized by the CDN it would stop moving.
    query = "" if original or sticker["format_type"] == APNG else f"?size={STICKER_PX}"
    data = get(f"{CDN}/stickers/{sticker['id']}.{ext}{query}")
    if not original and len(data) > STICKER_BYTES and sticker["format_type"] == PNG:
        data = get(f"{CDN}/stickers/{sticker['id']}.png?size=320")
    return data, ext


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("guild", help="the server's id")
    parser.add_argument("folder", help="where to put manifest.json and the pictures")
    parser.add_argument("--original", action="store_true", help="fetch every picture at its original size, not shrunk to fit Wyvrn's usual sizes")
    args = parser.parse_args()
    token = os.environ.get("DISCORD_BOT_TOKEN", "").strip()
    if not token:
        raise SystemExit("set DISCORD_BOT_TOKEN to a bot's token (the bot has to be in the server)")
    if not args.guild.isdigit():
        raise SystemExit(f"{args.guild}: a server id is a number")

    guild = api(f"/guilds/{args.guild}", token)
    emojis = api(f"/guilds/{args.guild}/emojis", token)
    stickers = api(f"/guilds/{args.guild}/stickers", token)
    print(f"{guild['name']}: {len(emojis)} emoji, {len(stickers)} stickers", file=sys.stderr)

    os.makedirs(os.path.join(args.folder, "emoji"), exist_ok=True)
    os.makedirs(os.path.join(args.folder, "stickers"), exist_ok=True)
    manifest = {"guild": {"id": guild["id"], "name": guild["name"]}, "original": args.original, "fetched_at": int(time.time()), "emoji": [], "stickers": [], "skipped": []}

    def keep(kind, item, data, ext, extra):
        if not str(item["id"]).isdigit():
            raise RuntimeError(f"an id that isn't a number: {item['id']!r}")
        file = f"{kind}/{item['id']}.{ext}"
        with open(os.path.join(args.folder, file), "wb") as out:
            out.write(data)
        manifest[kind].append({"id": item["id"], "name": item["name"], "file": file, "bytes": len(data), **extra})

    for n, emoji in enumerate(emojis, 1):
        try:
            data, ext = emoji_picture(emoji, args.original)
            keep("emoji", emoji, data, ext, {"animated": bool(emoji.get("animated")), "available": emoji.get("available", True)})
        except Exception as error:  # noqa: BLE001 -- one bad picture shouldn't stop the rest
            manifest["skipped"].append({"kind": "emoji", "id": emoji["id"], "name": emoji["name"], "why": str(error)})
        if n % 25 == 0:
            print(f"  emoji {n}/{len(emojis)}", file=sys.stderr)
    for sticker in stickers:
        if sticker["format_type"] == LOTTIE:
            manifest["skipped"].append({"kind": "sticker", "id": sticker["id"], "name": sticker["name"], "why": "a Lottie animation, which Wyvrn can't show"})
            continue
        try:
            data, ext = sticker_picture(sticker, args.original)
            keep("stickers", sticker, data, ext, {"animated": sticker["format_type"] in (APNG, GIF), "tags": sticker.get("tags", ""), "description": sticker.get("description") or ""})
        except Exception as error:  # noqa: BLE001
            manifest["skipped"].append({"kind": "sticker", "id": sticker["id"], "name": sticker["name"], "why": str(error)})

    with open(os.path.join(args.folder, "manifest.json"), "w") as out:
        json.dump(manifest, out, indent=2, ensure_ascii=False)
    print(f"{len(manifest['emoji'])} emoji and {len(manifest['stickers'])} stickers in {args.folder}; {len(manifest['skipped'])} left out", file=sys.stderr)
    for skipped in manifest["skipped"]:
        print(f"  left out: {skipped['kind']} {skipped['name']}: {skipped['why']}", file=sys.stderr)


if __name__ == "__main__":
    main()
