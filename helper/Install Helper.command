#!/bin/zsh
# Double-click on macOS, or run from a terminal. Builds the helper with cargo if needed. Its settings (.env) and data
# (.data) live in the checkout's top folder, next to extension/, and git ignores them.
set -eu
cd -- "${0:A:h}"
root="${PWD:h}"
# A helper already installed elsewhere keeps its folder: its settings and transcripts live there.
installed="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:2' "$HOME/Library/LaunchAgents/com.fulltranscript.helper.plist" 2>/dev/null || true)"
if [[ -n "$installed" && -f "$installed/.env" ]]; then
  root="$installed"
fi
if [[ -x target/release/scribe-helper ]]; then
  helper="$PWD/target/release/scribe-helper"
else
  cargo build --locked --release -p scribe-helper
  helper="$PWD/target/release/scribe-helper"
fi
exec "$helper" --root "$root" install
