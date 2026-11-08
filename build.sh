#!/bin/sh
# Builds the loadable extension (extension/) from the Rust crate (src/) and the browser sources
# (browser/).
#   ./build.sh         public build: what is committed and what users load
#   ./build.sh --dev   development build: local debug-center delivery, speed telemetry, auto-reload
#   ./build.sh --tv    the Google TV app's client bundle (tv/shell/src-tauri/bundle; public build,
#                      extension/ is left as it is). tv/scripts/build.sh runs it; see tv/README.md.
#   ./build.sh --tv --dev   the TV bundle's development variant (content-dev.js next to it: speed
#                      telemetry, which the app runs only while its owner turned telemetry on).
# Needs Rust 1.99.1 with the wasm32-unknown-unknown target (rust-toolchain.toml installs both through
# rustup), wasm-bindgen-cli 0.2.130 and Node 22+. On Windows run it from Git Bash or WSL.
set -eu
cd "$(dirname "$0")"
DEV=
TARGET=
for a in "$@"; do
  case "$a" in
    --dev) DEV=1 ;;
    --tv) TARGET=--target=tv ;;
    *) echo 'usage: ./build.sh [--dev] [--tv]' >&2; exit 2 ;;
  esac
done
find_tool() { command -v "$1" 2>/dev/null || echo "$HOME/.cargo/bin/$1"; }
CARGO="${CARGO:-$(find_tool cargo)}"
BINDGEN="${WASM_BINDGEN:-$(find_tool wasm-bindgen)}"
if [ "$("$BINDGEN" --version 2>/dev/null)" != 'wasm-bindgen 0.2.130' ]; then
  echo 'This build requires wasm-bindgen-cli 0.2.130 (cargo install wasm-bindgen-cli --version 0.2.130 --locked).' >&2
  exit 1
fi
# The TV bundle compiles Transcripts out (the Cargo feature `transcribe`); the extension also builds its second Wasm.
"$CARGO" build --release --locked --target wasm32-unknown-unknown ${TARGET:+--no-default-features} ${DEV:+--features dev}
"$BINDGEN" target/wasm32-unknown-unknown/release/ytc_client.wasm --target no-modules --remove-name-section --remove-producers-section --out-dir target/pkg --out-name ytc_client
if [ -z "$TARGET" ]; then
  "$CARGO" build --release --locked --target wasm32-unknown-unknown --manifest-path transcripts/Cargo.toml --target-dir target/transcripts
  "$BINDGEN" target/transcripts/wasm32-unknown-unknown/release/ytc_transcripts.wasm --target no-modules --remove-name-section --remove-producers-section --out-dir target/pkg-tr --out-name transcripts
fi
node browser/build.cjs ${DEV:+--dev} $TARGET
