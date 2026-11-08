# The Transcripts helper

A native Rust program that does what a browser extension cannot: it runs yt-dlp and FFmpeg, sends the audio to your own
Azure Speech resource, and writes transcripts to a folder you choose. The extension talks to it on `127.0.0.1:8765` and
through native messaging (only to get its pairing token).

This folder is its own Cargo workspace with its own lockfile, so the extension's crate never pulls these dependencies in.

## Get it

Build from source. A prebuilt release asset may come later.

1. Install Rust with [rustup](https://rustup.rs) (`rust-toolchain.toml` picks 1.99.1 by itself).
2. Install FFmpeg, FFprobe, yt-dlp and a JavaScript runtime yt-dlp supports, on your `PATH` (`FFMPEG_LOCATION` and
   `YTDLP_JS_RUNTIME` in `.env` can point elsewhere). The installer downloads nothing.
3. On macOS, double-click `Install Helper.command`. It builds the helper, registers a background service and the browser
   connection in Chrome's `NativeMessagingHosts` folder, and creates `.env` and `.data` in the checkout's top folder (git
   ignores them). A browser that does not read that folder (not yet confirmed for Brave) pairs through Manual pairing in
   Settings, or with a copy of that file in its own `NativeMessagingHosts` folder.
4. Load `extension/` in your browser, open Redline's popup and choose **Transcripts: Set up**. Enter your Azure endpoint
   and key there. The key goes to the helper's owner-only `.env` and never into browser storage.

The installed service knows its data folder. If a helper is already installed from another folder, these scripts keep
using that folder (its `.env` and `.data`) rather than starting empty here. That folder must keep its
`extension/manifest.json`: the native host reads the extension id it allows from there, so deleting it breaks pairing.

`Start.command` starts an installed helper. `Uninstall Helper.command` removes the service and browser registrations and
keeps your transcripts and settings. Keep the checkout where it is; if you move it, run the installer again.

Automatic install is macOS only. Elsewhere, run it yourself:

```sh
cargo run --locked --release -p scribe-helper -- --root .. check
cargo run --locked --release -p scribe-helper -- --root .. serve
```

and paste a token into Settings → Manual pairing (`HELPER_TOKEN` in `.env`).

## Costs and privacy

Transcription uses your Azure resource and bills your account. Audio goes to that resource and nowhere else. Transcripts
stay on your computer. There is no telemetry. The helper binds `127.0.0.1` only; every route except `/health` needs the
pairing token, and only a browser-extension origin may call it.

## Develop

```sh
cargo test --locked       # the tests that decode real audio skip without FFmpeg (SCRIBE_TEST_FFMPEG_DIR or PATH)
cargo fmt --all --check
```

Do not start a second helper on the installed service's port or data folder. A test helper takes `--root <temp dir>` and
`serve --port <free port>`. The feature `loopback-speech` (end-to-end tests only, never in a release) lets `.env` name a
Speech stand-in on `127.0.0.1`.

The names `scribe-helper`, `scribe-*` and the native host `com.fulltranscript.helper` stay: installed helpers, their
service and the browser registration depend on them (docs/architecture.md, "Names kept for compatibility").
