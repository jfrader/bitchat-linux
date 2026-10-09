# Changelog

## Unreleased

- Add a standalone Ratatui client for public BitChat Bluetooth mesh and Nostr location channels.
- Add nickname and radio controls, independent transport status, scrollback and failed-send feedback.
- Keep failed sends visible per room and report Bluetooth history-write errors.
- Name the radio state in the status line (`searching`, `1 link`, `N links`) and accept `#mesh` / `#<geohash>` as room switches.
- Render the chat log as an IRC-style column (time, right-aligned nickname, aligned text); mute borders, labels and hints.
