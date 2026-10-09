# bitchat-linux

A Rust/Ratatui terminal client for [BitChat](https://bitchat.free), with Bluetooth mesh and internet location channels.

Unofficial and experimental. **Public chat is unencrypted. Encrypted direct messages are not supported.**

## Build and run

Requires Linux, Rust 1.88+, `pkg-config` and D-Bus development headers (`libdbus-1-dev` on Debian/Ubuntu). Bluetooth needs BlueZ and an adapter with LE central and peripheral support.

```sh
git clone https://github.com/jfrader/bitchat-linux.git
cd bitchat-linux
cargo run --release --locked -p bitchat-linux -- --nickname alice
```

Optional: install the command with `cargo install --locked --path crates/bitchat-linux`.

## Bluetooth

Enable the adapter before starting:

```sh
rfkill unblock bluetooth
bluetoothctl power on
```

Open the phone's **Mesh** channel nearby. The terminal starts in `#mesh`; `/mesh` returns to it.

## Internet

Enter `/join <geohash>` using the same geohash as the phone. No location is detected automatically. Relays are selected from BitChat's relay directory; both clients must share a relay.

Startup options: `--geohash <geohash>`, repeatable `--relay <url>`, and `--no-bluetooth`. Use `--help` for all options.

## Controls

| Key / command | Action |
| --- | --- |
| Enter | Send to the selected room |
| Tab | Switch between mesh and internet |
| PgUp / PgDn | Scroll messages |
| F1 or `/help` | Show commands |
| Esc | Close help or clear the draft |
| Ctrl-C / `/quit` | Quit and stop both transports |
| `/mesh`, `/internet`, `#mesh`, `#<geohash>` | Select a room |
| `/nick <name>` | Set nickname |
| `/radio auto\|balanced\|saver\|off` | Set Bluetooth radio mode |
| `/clear` | Clear the selected room's local history |
| `//text` | Send text beginning with `/` |

## Limits and data

`/dm` and `/msg` are rejected, never sent as public messages. A mesh message with no connected peers is not confirmed delivered. A relay acknowledgement is not a read receipt. Phone proof-of-work filters may hide unmined internet messages.

Recent failed sends appear as **Not sent** in their room for the current session. They are not retried automatically.

Keys live under `$XDG_DATA_HOME/bitchat-linux` (default `~/.local/share/bitchat-linux`); mesh settings and history under `$XDG_STATE_HOME/bitchat-linux` (default `~/.local/state/bitchat-linux`). Override with `--data-dir` and `--state-dir`. Keys are private files, not encrypted at rest. Nearby devices can observe mesh identities; internet relays can read public messages.

## Development

```sh
make check
make build
```

MIT licensed. Bluetooth backend: [derekross/omarchy-bitchat](https://github.com/derekross/omarchy-bitchat). Relay data: [permissionlesstech/georelays](https://github.com/permissionlesstech/georelays), also MIT. The terminal client needs neither Omarchy nor the optional `bitchatd`/`bitchatctl` programs.
