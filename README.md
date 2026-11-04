# Kits Lite

Inventory kits for Play Strawberry servers: staff make kits from their own inventory or in the
settings file, and players claim them with a command or from a menu, with per-kit permissions,
cooldowns that survive restarts, one-time kits and read-only previews.

## Commands

| Command | What it does |
| --- | --- |
| `/kit` | Lists the kits you can use and their cooldowns |
| `/kit <name>` | Claims a kit |
| `/kit preview <name>` | Shows a kit without taking its items |
| `/kits` | Opens the kit menu: left click claims, right click previews |
| `/kitadmin create <name> [seconds] [true\|false]` | Makes a kit from your inventory, armour and offhand; optional cooldown and one-time flag |
| `/kitadmin edit <name>` | Replaces a kit's items from your inventory, keeping its cooldown and history |
| `/kitadmin delete <name>` | Deletes a kit |
| `/kitadmin give <name> <player>` | Gives a kit to an online player, with the normal checks |
| `/kitadmin set <name> cooldown <seconds>` | Changes the cooldown |
| `/kitadmin set <name> once <true\|false>` | Turns one-time use on or off |
| `/kitadmin firstjoin <name\|off>` | Chooses a kit for new players, or turns it off |
| `/kitadmin resetuses <name> confirm` | Forgets every player's use of one kit |
| `/kitadmin resetalluses confirm` | Forgets every player's use of every kit |
| `/kitadmin importuses confirm` | Reads earlier uses from `import-uses.txt` |

Kit names are 1 to 32 lowercase letters, digits, `_` or `-`. New kits have a 600-second cooldown.
The included kits `starter` and `tools` are ready to use.

## Permissions

| Permission | Default |
| --- | --- |
| `kits-lite:kit`, `kits-lite:kits` | everyone |
| `kits-lite:admin` | operators (level 4) |
| `kits-lite:kit.starter`, `kits-lite:kit.tools` | everyone |
| `kits-lite:kit.<name>` for a new kit | operators (level 4); give it to the players who should have it |

## How claiming works

- The whole kit must fit, or nothing is given and no cooldown starts. Nothing is ever dropped on
  the ground, and an item the player holds is never replaced.
- The use is saved before the items are given, so double clicks and repeated commands cannot give
  a kit twice.
- Menus and previews cannot be used to take items out.

## Settings

`plugins/data/kits-lite/kits-settings.txt` is written with these defaults when it is missing and
read when the plugin loads. A line that cannot be used stops the plugin from loading and the
console names it.

| Setting | Values | Default | What it does |
| --- | --- | --- | --- |
| `kit-list` | `text`, `menu` | `text` | what `/kit` alone shows |
| `kits-list` | `text`, `menu` | `menu` | what `/kits` shows |
| `preview` | `on`, `off` | `on` | previews |
| `menu-claim` | `on`, `off` | `on` | claiming from the menu |
| `first-join` | `on`, `off` | `on` | allows a first-join kit (it stays off until you choose one) |
| `language` | `player`, `en`, `fr`, `es`, `tr` | `player` | each player's language, or one for everyone |
| `unknown-kit` | `server`, `plugin` | `server` | how `/kit <unknown>` answers |
| `bundled-kits` | `on`, `off` | `on` | `off` removes `starter` and `tools` |
| `kit-labels`, `kits-labels` | names | none | extra command names for `/kit` and `/kits` |

Kits can also be written in the same file:

```text
[kit daily]
cooldown=900
item=minecraft:stone_sword 1
item=minecraft:bread 16
```

Each kit section takes `cooldown` (seconds), `once` (`true`/`false`), `access`
(`restricted`/`everyone`) and up to 41 `item` lines. These kits are changed only in the file.
A `[text <language>]` section replaces any message, for example:

```text
[text tr]
claimed={0} kiti geldi.
```

## Importing uses

`/kitadmin importuses confirm` reads `import-uses.txt` from the data folder: a first line
`unit=milliseconds` (or `unit=seconds`), then one `<uuid> <kit> <time>` per line. One bad line
refuses the whole file.

## Languages

English, French, Spanish and Turkish, chosen by each player's game language (`lang/`).

## Install on a Play Strawberry server

1. Add Kits Lite to your server in the Play Strawberry panel.
2. Restart the server. The plugin creates `plugins/data/kits-lite/` on the first start.
3. Make your kits with `/kitadmin create` and give their permissions to your players.

Play Strawberry servers load only plugins signed by Play Strawberry, so a file you build yourself
from this source does not load on them.

## Build from source

You need Rust with the `wasm32-wasip2` target and the Strawberry plugin SDK
(`strawberry-plugin-api`), which Play Strawberry gives to its plugin developers.
`Cargo.toml` takes it from the SDK registry named `strawberry`; add that registry to
your Cargo configuration (for example `~/.cargo/config.toml`):

```toml
[registries.strawberry]
index = "sparse+https://sdk.playstrawberry.net/index/"
```

```text
cargo test
cargo build --release --target wasm32-wasip2
```

The plugin file is `target/wasm32-wasip2/release/kits_lite.wasm`.

## License

Copyright Strawberry Interactive LLC. The license text is not published yet; until it is, all
rights are reserved.
