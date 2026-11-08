// Transcripts never goes to the TV app: fails if its strings, routes or names are in the TV bundle.
// Runs inside ./build.sh --tv, and by itself: node tests/check-tv-bundle.cjs
'use strict';
const fs = require('node:fs'), path = require('node:path');
// Words of the feature itself. (YouTube's own 'transcript' panels and 'subscribe' are not it, so the plain word is no test.)
const NEVER = ['Full transcript', 'Transcripts', 'ytcTranscripts', 'transcripts.wasm', 'transcripts-core', 'tsc-', 'tsc:', 'scribe-helper', 'pairingToken', 'getVideoStatuses', 'openReaderVideo', 'channelPanel', '127.0.0.1:8765', '/videos/status', '/queue/status', '/channels/import', 'com.fulltranscript', 'nativeMessaging', 'sendNativeMessage', 'Azure', 'data-tr-page', 'popup-transcripts', 'popup-export', 'redline-settings.json'];

function check(dir) {
  const problems = [];
  for (const f of fs.readdirSync(dir)) {
    if (f === 'seq' || f === 'client') continue;
    const bytes = fs.readFileSync(path.join(dir, f));
    for (const s of NEVER) if (bytes.includes(s)) problems.push(`${f} contains "${s}"`);
  }
  if (problems.length) {
    console.error('The TV bundle must not carry Transcripts:\n- ' + problems.join('\n- '));
    process.exit(1);
  }
}
module.exports = { check };
if (require.main === module) {
  const dir = path.join(__dirname, '..', 'tv', 'shell', 'src-tauri', 'bundle');
  if (!fs.existsSync(path.join(dir, 'content.js'))) { console.error('No TV bundle here: run ./build.sh --tv first.'); process.exit(1); }
  check(dir);
  console.log('The TV bundle carries nothing of Transcripts.');
}
