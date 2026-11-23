# Armory Hall Uploader

A small tray app that keeps your World of Warcraft Forever bags and bank up to date on
[Armory Hall](https://armoryhall.app/uploader).

- Finds your World of Warcraft folder and installs the **Armory Hall** addon (`addon/`, folder `LionholdExport`).
- You connect it once: it opens the website, you sign in with Battle.net and approve.
- After every logout (or `/reload`) the game rewrites the addon's SavedVariables file; the uploader sends it.
- Starts with your computer, lives in the tray, and updates itself.

## What it reads and sends

Only `World of Warcraft/_forever_/WTF/Account/*/SavedVariables/LionholdExport.lua`, which holds what the addon saved:
each character's name, level, class, race, guild, bag and bank contents, and equipped items. It is sent to
`/api/upload` with a per-computer key stored in your system keychain. You can disconnect a computer from the website
at any time. Uploaded bags are private unless you share them with your guild.

## Building

Requires Node 24 and Rust. `npm install`, then `npm run dev` or `npm run build`. Releases are built by
`.github/workflows/release.yml` when a `v*` tag is pushed.

MIT licensed.
