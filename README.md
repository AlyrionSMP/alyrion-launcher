# Alyrion Launcher

The official launcher for the **Alyrion VII** Minecraft modpack — a desktop
app (Tauri v2) that is **locked to exactly one thing**: the latest version of
[Alyrion VII on Modrinth](https://modrinth.com/modpack/alyrion).

It cannot install other modpacks, other versions of Alyrion, or any other
software. Every launch guarantees the instance is the latest published
version: on startup the launcher checks Modrinth, and if a newer version
exists it automatically updates before you can press Play.

> **You cannot play while an update is ongoing** — the launcher's state
> machine only allows launching from the `Ready` phase.

## Features

- **Auto-update on startup** — fetches the latest pack version from Modrinth,
  downloads the `.mrpack`, extracts it into a fresh staging tree and swaps it
  in atomically (old instance becomes `.instance-old` only after the new one
  is fully verified).
- **Integrity-first installs** — every file downloaded from the pack's index
  is verified against its SHA-1 (and size) before it is accepted; a corrupt
  download is deleted and retried.
- **Resumable downloads** — `.part` files are resumed via HTTP `Range`.
- **Full NeoForge client bootstrap** — the launcher runs the official
  NeoForge installer headless to produce the exact vanilla + NeoForge
  profiles and libraries, then launches the game exactly as the official
  launcher would.
- **Blocked play during updates** — enforced in both the Rust state machine
  (Play command refused unless `Ready`) and the UI (button disabled).
- **Single instance lock** — only one launcher/game can run at a time.
- **Minimal UI** — clean, flat dark interface with a single accent; no
  ornament, just the essentials.

## Development

Prerequisites: Node 18+, Rust stable, and Tauri v2's system deps.

- **Linux**: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libsoup-3.0-dev`,
  `librsvg2-dev`, `pkg-config` — see
  [tauri.app](https://tauri.app/start/prerequisites/).
- **macOS**: Xcode Command Line Tools (`xcode-select --install`). No extra
  libraries are required.
- To actually launch the game you also need a **Java 21+** runtime on the
  machine (the launcher looks for one automatically; on macOS it checks
  `/usr/libexec/java_home`, `/Library/Java/JavaVirtualMachines` and Homebrew —
  e.g. install Temurin 21).

```bash
npm install
npm run tauri dev      # dev run with HMR
npm run tauri build    # release bundle
```

The app bundles into installers via Tauri (`npm run tauri build`). The
Windows build produces an MSI; Linux produces `.deb` and `.AppImage`;
macOS produces a `.dmg` (plus an `.app`).

## Accounts

The launcher supports three ways to sign in:

- **Offline** — any player name, Mojang-style offline UUID. Works for
  single-player, LAN and any server that doesn't enforce online auth.
- **LittleSkin** — Yggdrasil login against `littleskin.cn` (the server can be
  overridden). Password is sent once over HTTPS; only tokens are stored.
- **Ely.by** — direct-credential login against Ely.by's Mojang-compatible
  authserver (`POST https://authserver.ely.by/auth/authenticate`), exactly
  how XMCL and other launchers do it. **No OAuth app is needed**: just enter
  your Ely.by username/email + password in the launcher. The password is sent
  once over HTTPS to Ely.by and never stored — only the access token is kept
  in `accounts.json`.

  At game launch the launcher adds the
  [authlib-injector](https://github.com/yushijinhun/authlib-injector)
  javaagent (downloaded automatically, ~341 KB) so the game's session server
  is Ely.by instead of Mojang — this is what makes online play work.

  If your Ely.by account has two-factor auth enabled, append your TOTP code
  to the password as `password:code` when logging in.

- **LittleSkin** — Yggdrasil login against `littleskin.cn` (the server can be
  overridden in `settings.json`). Same authlib-injector javaagent is used at
  launch so the game talks to LittleSkin's session server. Password is sent
  once over HTTPS; only tokens are stored.

The `settings.json` file lives in the launcher data folder next to
`accounts.json` and may override the LittleSkin server:

```json
{
  "littleskin_server": "https://littleskin.cn/api/yggdrasil"
}
```

## Architecture

```
src-tauri/src/
  modrinth.rs   Modrinth API client (latest version + mrpack)
  install.rs    modrinth.index.json parser
  update.rs     update orchestration (fetch → verify → atomic swap)
  game.rs       NeoForge/vanilla profile merge, classpath, launch
  maven.rs      Maven coordinates + repo locating
  cancellation.rs CancelToken
  jobs.rs       job registry + process watcher
  state.rs      state machine (UiState / Phase)
  lib.rs        Tauri glue: commands, event loop, auto-update
src/            Svelte-free vanilla TS UI + steampunk CSS
```

The theory of operation:

1. On startup the backend resolves the user data dir
   (`<app-config>/AlyrionLauncher/`) and immediately kicks off an
   auto-update check.
2. `update::update_pack` fetches the newest pack version, downloads the
   `.mrpack`, verifies its hashes, extracts `overrides/` and all 133 indexed
   files into a staging dir, verifies every file, then atomically swaps
   `staging → instance`. Worlds / screenshots / resourcepacks are preserved
   across updates.
3. The UI polls `state_snapshot` and receives `state-changed` events. The
   Play button is only enabled when `phase == ready`.
4. `play` builds the launch spec: merged vanilla + NeoForge profiles,
   resolved assets, natives, classpath, token substitution, then spawns the
   game with its log piped to `instance/logs/latest.log`.

## License

The launcher code is MIT. The Alyrion VII modpack and its assets are
© their respective authors (Modrinth project `alyrion`); this launcher is
an unofficial tool and is not affiliated with Modrinth or Mojang.