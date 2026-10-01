# bitchat-linux

A Rust terminal client for [BitChat](https://bitchat.free), built with Ratatui.

- **Bluetooth:** nearby peer discovery, signed public `#mesh` chat, relaying and gossip sync through BlueZ.
- **Internet:** public Nostr location channels, selected by geohash, with independent connection status and relay acknowledgements.
- **Terminal:** room switching, Unicode input, scrollback, nickname and radio controls.

Public messages are **not encrypted**. Encrypted direct messages over Bluetooth and Nostr are planned, not implemented. This is an unofficial, experimental client; phone-to-desktop interoperability still needs real-device testing.

## Build and run

Requires Linux, Rust 1.88 or newer, `pkg-config`, and the D-Bus development library. Bluetooth also needs a running BlueZ service and an adapter with LE central and peripheral roles. Internet chat works without Bluetooth.

```sh
cargo build --locked -p bitchat-linux
./target/debug/bitchat-linux
```

Start directly in an internet channel:

```sh
./target/debug/bitchat-linux --geohash dr5rs --nickname raven
```

Use the **same geohash as the phone app**. No location is inferred automatically. The client selects five nearby relays from BitChat's georelays directory; `--relay` overrides discovery and can be repeated. Clients must share at least one relay to exchange messages.

```sh
./target/debug/bitchat-linux --no-bluetooth --geohash dr5rs --relay wss://your-relay.example
./target/debug/bitchat-linux --help
```

The client does not unblock or power on the Bluetooth adapter. Check it yourself:

```sh
bluetoothctl show
rfkill list bluetooth
rfkill unblock bluetooth
bluetoothctl power on
```

## Controls

| Key / command | Action |
| --- | --- |
| Enter | Send to the selected room |
| Tab | Switch between mesh and internet |
| PgUp / PgDn | Scroll messages |
| F1 | Show commands |
| Esc | Close help or clear the draft |
| Ctrl-C / `/quit` | Quit and stop both transports |
| `/join <geohash>` | Join an internet location channel |
| `/mesh`, `/internet` | Select a room |
| `/nick <name>` | Set nickname |
| `/radio auto\|balanced\|saver\|off` | Set Bluetooth radio mode |
| `/clear` | Clear the selected room's local history |
| `//text` | Send text beginning with `/` |

`/dm` and `/msg` are rejected; they never send the supplied text into public chat. A mesh message with no connected peers is stored locally, not confirmed delivered. A Nostr acknowledgement means a relay accepted the event, not that another person read it. Phones with proof-of-work filtering enabled may hide unmined Nostr messages.

## Local data

- `$XDG_DATA_HOME/bitchat-linux/mesh/identity.json`: persistent mesh identity.
- `$XDG_DATA_HOME/bitchat-linux/nostr/<geohash>.key`: a separate persistent signing key per internet channel.
- `$XDG_STATE_HOME/bitchat-linux/mesh/`: radio settings, pinned peer keys and recent public mesh history.

The usual XDG fallbacks are `~/.local/share` and `~/.local/state`. `--data-dir` and `--state-dir` override the app directories. Keys are private files, not encrypted at rest. Corrupt identities cause an error rather than a silent replacement. Internet history is kept in bounded memory; clearing a public room does not delete messages from other devices or relays.

Radio presence, stable mesh identifiers and nicknames are observable to nearby devices. Public internet relays see the channel, signing identity and messages. This client is not independently security-audited.

## Verify

```sh
make check
make build
```

Tests include signed public-event validation, local-relay send/receive, room isolation, failed-send drafts, terminal-control sanitization and Ratatui rendering. They do not prove radio interoperability with a real phone. GitHub verification is manual-only; there is no deployment or published package.

## Upstream

The Bluetooth engine and protocol are derived from [derekross/omarchy-bitchat](https://github.com/derekross/omarchy-bitchat), revision `75ff492ed9c7b01acd89e290b46ca2bcbdd07053`, under its MIT license. Its Omarchy frontend and installation scripts are not used. The same engine remains available as the optional `bitchatd` / `bitchatctl` programs; the terminal client embeds it and needs neither.

Relay data comes from [permissionlesstech/georelays](https://github.com/permissionlesstech/georelays), under MIT. Public Nostr events follow the current official BitChat kind-20000 protocol with `g` and `n` tags; this is not a general-purpose Nostr client.
