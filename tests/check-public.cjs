// Fails unless extension/ holds the public build: run before committing (the development build
// from ./build.sh --dev writes the same files). Usage: npm run check:public
'use strict';
const fs = require('node:fs'), path = require('node:path');
const { isDevBuild } = require('./browser.cjs');
const dir = path.join(__dirname, '..', 'extension');
const problems = [];
if (isDevBuild(dir)) problems.push('manifest.json grants the local debug-center collector (development build)');
const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
if (manifest.version_name || / \(dev\)$/.test(manifest.name)) problems.push('manifest.json carries a development name or build id');
if (fs.existsSync(path.join(dir, 'build-id.txt'))) problems.push('build-id.txt exists (development build)');
for (const f of ['content.js', 'background.js']) {
  const text = fs.readFileSync(path.join(dir, f), 'utf8');
  for (const s of ['127.0.0.1:47321', 'ytc-dev-check', 'build-id.txt', 'ytcIncidentsV1', 'ytcPerfQueueV1', '@dev-start'])
    if (text.includes(s)) problems.push(`${f} contains "${s}"`);
}
const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));
const cargo = /^version = "([^"]+)"/m.exec(fs.readFileSync(path.join(__dirname, '..', 'Cargo.toml'), 'utf8'))[1];
if (manifest.version !== pkg.version || manifest.version !== cargo) problems.push(`versions differ: manifest ${manifest.version}, package.json ${pkg.version}, Cargo.toml ${cargo}`);
// Nothing secret or machine-specific is tracked: signing keys and keystores (the TV app's live in
// ~/.config/lumen-tv/), or node_modules (once committed as a symlink to a personal path).
const repo = path.join(__dirname, '..');
// (Outside a git checkout, as in an unpacked download, there is nothing tracked to check. Inside one a failure is a failure.)
if (fs.existsSync(path.join(repo, '.git'))) {
  const git = (...a) => require('node:child_process').execFileSync('git', a, { cwd: repo, encoding: 'utf8', maxBuffer: 1 << 28 }).split('\0').filter(Boolean);
  const tracked = git('ls-files', '-z'), incoming = git('ls-files', '-z', '--others', '--exclude-standard');
  const present = tracked.concat(incoming);
  for (const f of present) {
    if (/(^|\/)(node_modules|keystore\.properties)$|\.(jks|keystore|pem|p12)$/.test(f)) problems.push(`${f} is tracked (keys, keystores and node_modules never are)`);
    // The Transcripts helper's settings and data: the Azure key and a person's transcripts. `.env.example` is the template.
    if (/(^|\/)\.env(\.(?!example$)[^/]*)?$|(^|\/)\.data(\/|$)|(^|\/)\.venv(\/|$)/.test(f)) problems.push(`${f} must never be committed (the helper's settings and data)`);
  }
  // The imported helper, the Transcripts sources and the docs carry no personal paths, addresses, IPs or keys.
  // Documentation addresses and loopback are fine; so are example.com and synthetic test values.
  const scanned = present.filter(f => /^(helper|transcripts)\/|^tests\/transcripts|^browser\/(transcripts|reader|settings)|^src\/features\/transcribe|^(README|CONTRIBUTING|AGENTS)\.md$|^docs\/.*\.md$/.test(f) && !/(^|\/)Cargo\.lock$/.test(f));
  const RULES = [
    [/(?:\/Users|\/home)\/(?!runner\b)[a-z][\w.-]+\//, 'a personal path'],
    [/\bC:\\Users\\/, 'a personal path'],
    [/[\w.+-]+@(?!example\.|.*\.invalid|youtube\.com|resource\.)[a-z0-9-]+\.[a-z]{2,}/i, 'an email address'],
    [/\b(?!127\.0\.0\.1\b|0\.0\.0\.0\b|10\.0\.2\.2\b|203\.0\.113\.|192\.0\.2\.|198\.51\.100\.)(?:\d{1,3}\.){3}\d{1,3}\b/, 'an IP address'],
    [/AZURE_SPEECH_KEY\s*=\s*['"]?[A-Za-z0-9]{16,}/, 'an Azure key'],
    [/HELPER_TOKEN\s*=\s*['"]?[a-f0-9]{32,}/, 'a helper token'],
    // An Azure key is commonly a bare 32 hex digits. (A run of one digit is a test pattern, not a key.)
    [/(?<![\w-])(?!([a-f0-9])\1{31})[a-f0-9]{32}(?![\w-])/, 'what looks like a bare key'],
    [/justerquate/, 'an account name'],
  ];
  // (The repository's own public address and its author are allowed where the docs and links name them.)
  const OWN = /github\.com\/justerquate-sys\/redline|justerquate-sys\/(lumen|redline)|justerquate-sys/g;
  for (const f of scanned) {
    let text;
    try { text = fs.readFileSync(path.join(repo, f), 'utf8'); } catch { continue; } // deleted, not yet staged
    if (text.includes('\u0000')) continue;
    text = text.replace(OWN, '');
    for (const [re, what] of RULES) { const m = re.exec(text); if (m) problems.push(`${f} contains ${what}: ${m[0]}`); }
  }
  // The feature's old name in what people read, in the sources that produce it. Internal names stay (docs/architecture.md):
  // the crates and binary `scribe-*`, the module paths `scribe_*`, the `SCRIBE_*` switches, and this one mapping from an
  // installed helper's older wording.
  const userFacing = present.filter(f => /^(helper\/crates\/[^/]+\/src|transcripts\/src)\/.*\.rs$|^helper\/.*\.command$/.test(f));
  for (const f of userFacing) {
    let text;
    try { text = fs.readFileSync(path.join(repo, f), 'utf8'); } catch { continue; }
    const lines = text.split('\n');
    lines.forEach((line, i) => {
      if (!/\bScribe\b/.test(line)) return;
      if (f === 'transcripts/src/core.rs' && /Scribe is retrying that status write/.test(line)) return;
      if (line.includes('modern_wording("A talk about Scribe")')) return;
      problems.push(`${f}:${i + 1} says Scribe: ${line.trim().slice(0, 80)}`);
    });
  }
}
// One extension id: the helper's native host allows exactly this one (helper/README.md, docs/architecture.md).
const EXTENSION_ID = 'lamdpdfcpkihifbpmcalmjdnbfegjjjh';
const idOf = key => [...require('node:crypto').createHash('sha256').update(Buffer.from(key || '', 'base64')).digest().subarray(0, 16)].flatMap(b => [b >> 4, b & 15]).map(n => String.fromCharCode(97 + n)).join('');
if (idOf(manifest.key) !== EXTENSION_ID) problems.push(`manifest.json's key gives the extension id ${idOf(manifest.key)}, not ${EXTENSION_ID}`);
for (const p of ['nativeMessaging', 'alarms']) if (!manifest.permissions.includes(p)) problems.push(`manifest.json lacks the ${p} permission`);
if (!manifest.host_permissions.includes('http://127.0.0.1:8765/*')) problems.push('manifest.json lacks the helper\'s host permission');
// The feature's old name is gone from everything a person sees (internal names stay: docs/architecture.md).
for (const f of ['reader.html', 'settings.html', 'popup.html', 'pages.css', 'transcripts.css', 'popup.css', 'transcripts-core.js', 'background.js', 'content.js', 'transcripts.wasm', 'manifest.json']) {
  // (The worker holds an installed helper's older wording only to replace it, core::modern_wording.)
  const text = fs.readFileSync(path.join(dir, f)).toString('latin1').replaceAll('Scribe is retrying that status write', '');
  if (text.includes('Scribe')) problems.push(`${f} still says Scribe`);
}
// The extension pages' palette is client.css's.
{
  const tokens = text => Object.fromEntries([...text.matchAll(/--(bg|pn|rs|cb|hv|on|fi|bd|tk|ln|tx|mu|fa|ac|er|brand|progress):([^;}]+)[;}]/g)].map(m => [m[1], m[2]]));
  const a = tokens(fs.readFileSync(path.join(dir, 'client.css'), 'utf8')), b = tokens(fs.readFileSync(path.join(dir, 'pages.css'), 'utf8'));
  for (const k of Object.keys(b)) if (a[k] !== b[k]) problems.push(`pages.css --${k} is ${b[k]}, client.css has ${a[k]}`);
}
if (problems.length) { console.error('extension/ is not a clean public build:\n- ' + problems.join('\n- ') + '\nRun ./build.sh (without --dev).'); process.exit(1); }
console.log('extension/ is the public build (' + manifest.version + ').');
