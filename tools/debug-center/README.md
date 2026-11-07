# Redline debug center (optional developer tool)

A local collector for Redline **development builds** (`./build.sh --dev`). Rust, standard library only.
It listens on `127.0.0.1:47321` only (unless started with `--lan`, below) and appends every report the extension's background worker POSTs to
`/incident` (incidents and speed-telemetry batches) as one line to its incidents file. The public build
has no collector code and never talks to it.

| Route | What |
|---|---|
| `POST /incident` | Append a report (≤ 512 KB): `{"received": <epoch ms>, "report": <the JSON>}` per line. |
| `GET /` | The latest 30 incidents (telemetry batches are only counted). |
| `GET /perf?since=<7d>&version=<build>&metric=<prefix*>&platform=<tv\|desktop>` | Speed dashboard: per metric count/p50/p90/p99/max, interactions per action (p50/p90/max, slowest with timeline links), hedged-request rates, p50 trend per build, incidents per build, the 20 slowest events per metric with their breakdown. |
| `GET /timeline?platform=<tv\|desktop>` · `GET /timeline?page=<id>` | The documents seen (newest first) · one document's every event (interactions, navigations, requests, long frames…) in order with ms. |
| `GET /perf.json?metric=<name or prefix*>&since=<epoch ms, ms ago, 30m/6h/7d, all>&version=<build>&platform=<tv\|desktop>&limit=<n>` | Raw telemetry rows (JSON array, oldest first): the event plus `received`, `version`, `platform`, `page`. |
| `GET /health` | `ok` |

The file is streamed for every view (lines received before the window are skipped unparsed); each
connection runs on its own thread. 50,000 lines (69 MB, ~550,000 events) render in ~0.35 s; a 24-hour
window of them in ~0.03 s. What the reports and events mean: [docs/debug-center.md](../../docs/debug-center.md).

## Where the incidents file lives

The first argument names the file. Without one it goes to your user data folder, never into the
repository:

| OS | Default incidents file |
|---|---|
| macOS | `~/Library/Application Support/Lumen/incidents.ndjson` |
| Linux | `$XDG_DATA_HOME/lumen/incidents.ndjson` (usually `~/.local/share/lumen/incidents.ndjson`) |
| Windows | `%APPDATA%\Lumen\incidents.ndjson` |

Reports contain no cookies, tokens, search terms or keys, but they do contain video ids, timings and your
browser's user agent: keep the file to yourself.

## Run it

```sh
cd tools/debug-center
cargo build --release --locked && cargo test --locked
./target/release/lumen-debug-center            # default incidents file (above)
./target/release/lumen-debug-center /tmp/x.ndjson
```

Then open http://127.0.0.1:47321/ and http://127.0.0.1:47321/perf.

**Redline for TV** (your TV with telemetry turned on, tv/README.md "Speed telemetry"): start it with `--lan`
(`./target/release/lumen-debug-center --lan`, or `YTC_DEBUG_CENTER_LAN=1`). It then also listens on the network, prints
the address to type on the TV, and answers the TV's discovery probe (UDP `LUMEN-DC?` on the same port, from devices on
your network only). Devices other than this computer may only `POST /incident` (and `GET /health`): every page and
`/perf.json` answer 403 to them, so the reports stay readable on this computer only. Without `--lan` nothing changes:
127.0.0.1 only. The TV's reports carry `"platform":"tv"`; the dashboards' *Platform* filter (`&platform=tv`) shows
them apart from the desktop's.
`YTC_DEBUG_CENTER_ADDR=127.0.0.1:<port>` runs a second instance on another port (tests); the extension
itself only talks to port 47321 (or to the URL in its `ytcCollectorV1` storage key).

## Start it at login (macOS)

Copy the binary somewhere stable and add a LaunchAgent, e.g.
`~/Library/LaunchAgents/com.lumen.debug-center.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.lumen.debug-center</string>
  <key>ProgramArguments</key><array>
    <string>/Users/YOU/Library/Application Support/Lumen/lumen-debug-center</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/Users/YOU/Library/Application Support/Lumen/debug-center.log</string>
</dict></plist>
```

With no second `ProgramArguments` entry the incidents file is the default one above. Load it with
`launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.lumen.debug-center.plist`, restart it
after replacing the binary with `launchctl kickstart -k gui/$(id -u)/com.lumen.debug-center`, stop it
with `launchctl bootout gui/$(id -u)/com.lumen.debug-center`.

On Linux a systemd user service (`ExecStart=/path/to/lumen-debug-center`) does the same; on Windows a
Task Scheduler task "At log on".
