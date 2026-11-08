#![allow(dead_code)]
use scribe_common::Settings;
use std::path::Path;

pub fn settings(root: &Path) -> Settings {
    Settings {
        root: root.into(),
        data_dir: root.join("data"),
        speech_key: "offline-test-key".into(),
        speech_endpoint: "https://offline.cognitiveservices.azure.com".into(),
        helper_token: "offline-token".into(),
        ffmpeg_location: String::new(),
        cookies_browser: String::new(),
        js_runtime: String::new(),
        chunk_seconds: 2,
        audio_speed: 1.25,
    }
}
pub fn executable(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
/// A local shell fixture, never the real downloader and never a network client.
pub fn downloader(root: &Path) {
    std::fs::create_dir_all(root.join("metadata")).unwrap();
    std::fs::create_dir_all(root.join("tabs")).unwrap();
    executable(
        &root.join(".venv/bin/yt-dlp"),
        r##"#!/bin/sh
set -eu
base="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
mode=stream
previous=
info=
for arg in "$@"; do
  if [ "$previous" = --load-info-json ]; then info=$arg; fi
  case "$arg" in --dump-single-json) mode=metadata;; --load-info-json) mode=download;; esac
  previous=$arg
  last=$arg
done
printf '%s %s\n' "$mode" "$last" >> "$base/calls"
if [ "$mode" = download ]; then
  test -f "$info"
  cp "$info" "$base/reused-info.json"
  cp "$base/source.wav" "$PWD/source.wav"
  printf '%s\n' "$PWD/source.wav"
elif [ "$mode" = metadata ]; then
  id=${last##*=}
  if [ -f "$base/metadata/$id.error" ]; then cat "$base/metadata/$id.error" >&2; exit 1; fi
  cat "$base/metadata/$id.json"
else
  tab=${last##*/}
  if [ -f "$base/tabs/$tab.error" ]; then cat "$base/tabs/$tab.error" >&2; exit 1; fi
  if [ -f "$base/block-streams" ]; then
    printf '%s\n' "$last" >> "$base/stream-started"
    while [ ! -f "$base/release-streams" ]; do sleep 0.02; done
  fi
  if [ -f "$base/tabs/$tab.jsonl" ]; then cat "$base/tabs/$tab.jsonl"; fi
  if [ -f "$base/hold-after-list" ]; then sleep 300; fi
fi
"##,
    );
}
pub async fn wait_file(path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
