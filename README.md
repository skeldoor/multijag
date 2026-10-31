# MultiJag

An OSRS-only launcher that keeps several Jagex accounts signed in at the same time. Built with
[Tauri](https://tauri.app): a ~5 MB app that uses the OS webview, for Windows, macOS and Linux.

## Run / build

Needs Node 20+ and [Rust](https://rustup.rs). On Linux also `libwebkit2gtk-4.1-dev libdbus-1-dev`.

```
npm install
npm run dev      # run
npm run build    # installers in src-tauri/target/release/bundle
```

Pushing a `v*` tag (or running the workflow by hand) builds all three platforms in GitHub Actions.

## Use

- **Add Jagex account** opens a Jagex sign-in window. Every account gets its own isolated webview
  data, so adding a second account doesn't sign the first out.
- Each account lists its characters. **Play** starts RuneLite or the official client (dropdown at
  the top) with that character's `JX_SESSION_ID` / `JX_CHARACTER_ID` / `JX_DISPLAY_NAME`. Run as many
  clients at once as you like.
- Client paths are auto-detected where possible; change them under *Client paths*. On Linux, point
  RuneLite at an AppImage or `runelite` binary (Flatpak can't receive the session variables). The
  official client only exists on Windows.

## Where things live

Settings and encrypted tokens are in the app data folder (`%APPDATA%\local.multijag`,
`~/Library/Application Support/local.multijag`, `~/.local/share/local.multijag`). Tokens are
AES-256-GCM encrypted with a key held in the OS keychain (a 0600 key file is the fallback when
there is no keychain). 

## How it works

`src-tauri/src/jagex.rs` follows the official launcher's flow, taken from its logs and cache:

1. OAuth2 + PKCE login as `com_jagex_auth_desktop_launcher`. The redirect to `jagex:code=…` is
   cancelled inside the app, so the real launcher never sees it.
2. A consent request for the game client (`1fddee4e-…`, redirect `http://localhost`) returns a game
   `id_token`.
3. `POST auth.runescape.com/game-session/v1/sessions` returns a session ID, then
   `GET …/v1/accounts` returns the characters.

Before each launch the session is checked and rebuilt from the refresh token if it has expired.

## Trademarks

The Jagex logo in the footer is Jagex's trademark, used only as a link to Jagex Account Management. MultiJag is an unofficial community tool and isn't affiliated with or endorsed by Jagex. RuneLite is a third-party client; see the [Jagex EULA](https://legal.jagex.com/docs/terms/eula#id-5.-other-third-party-services-integration-and-third-party-software-acknowledgments).
