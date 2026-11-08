#!/usr/bin/env node
// Publishes a Redline for TV release on GitHub: the shared client's bundle (content.js, client.css,
// native.css, rules.json — the exact files built into the APK), the APK (as lumen-tv-<version>.apk and,
// for the first-install link, lumen-tv.apk), and lumen-tv-manifest.json with
// its ed25519 signature (lumen-tv-manifest.json.sig). The TV app installs a bundle only when the signature
// matches the public key compiled into it (tv/keys/release.pub) and every SHA-256 matches. The bundle's
// development variant content-dev.js (speed telemetry; ./build.sh --tv --dev) is signed along when it was
// built: the app runs it only while its owner turned telemetry on (About & updates → Developer).
//
//   node tv/scripts/release.cjs                  build (release APK) + sign + publish to GitHub
//   node tv/scripts/release.cjs --dry-run        build + sign into tv/out/release/, publish nothing
//   node tv/scripts/release.cjs --no-build ...   use the last build (tv/shell/src-tauri/bundle, tv/out)
//   node tv/scripts/release.cjs --dry-run --no-build --bundle <dir> --apk <file>
//                                                sign another bundle/APK (tests; APK_VERSION and
//                                                APK_VERSION_CODE override the APK's version)
//   node tv/scripts/release.cjs --serve 8765     serve tv/out/release/ as a fake GitHub API
//                                                (GET /repos/<any>/<any>/releases, /dl/<file> and the API's
//                                                /repos/<any>/<any>/releases/assets/<id>) for testing the
//                                                app's update check (debug builds read files/dev.json, see docs)
//      --fail-dl [status]                        /dl/ answers this (default 504), as GitHub's website did on
//                                                8 Nov 2026; the API asset route still delivers
//      --limit-api                               the API asset route answers GitHub's rate-limit 403
//      --fail-api                                the API asset route answers 502 (both routes down)
// Environment: GITHUB_TOKEN (contents:write on the repo), LUMEN_TV_REPO=owner/name (default
// justerquate-sys/redline, the repository's name since the rename from justerquate-sys/lumen; apps built before
// 0.5.0 have "lumen" compiled in and reach the same releases through GitHub's redirect for renamed repositories,
// so the old name must never be taken by a new repository), LUMEN_TV_KEY (private key,
// default ~/.config/lumen-tv/release-key.pem), SEQ (bundle sequence, default now).
'use strict';
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto'), os = require('node:os');
const { execFileSync } = require('node:child_process');
const tv = path.resolve(__dirname, '..'), root = path.dirname(tv);
const args = process.argv.slice(2), has = f => args.includes(f), opt = f => { const i = args.indexOf(f); return i >= 0 ? args[i + 1] : undefined; };
const outDir = path.join(tv, 'out', 'release');
const FILES = ['content.js', 'client.css', 'native.css', 'rules.json'];
// Optional: signed when present (older apps ignore it).
const DEV_FILE = 'content-dev.js';
// Format 2 (shell 0.3.0+): the Wasm as files of their own, served to the page by the shell. A bundle that has them
// runs only on a shell that serves them, so the manifest's min_shell is at least this app's version code: older apps
// keep the bundle they run and are offered this APK (their update check reports needs_shell, and the APK).
const WASM_FILE = 'client.wasm', DEV_WASM_FILE = 'client-dev.wasm', WASM_SHELL = '0.3.0';
const LATEST_APK = 'lumen-tv.apk';

function serve(port) {
  const http = require('node:http');
  const failDl = has('--fail-dl') ? (/^\d+$/.test(opt('--fail-dl') || '') ? +opt('--fail-dl') : 504) : 0, limitApi = has('--limit-api');
  http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x'), host = req.headers.host;
    console.log(req.method, url.pathname, req.headers.authorization ? 'WITH AUTH' : '');
    const octets = f => { res.writeHead(200, { 'content-type': 'application/octet-stream' }); return fs.createReadStream(f).pipe(res); };
    // Asset ids: the file's place in the listing.
    const names = fs.readdirSync(outDir);
    const list = /^(\/repos\/[^/]+\/[^/]+\/releases)(\/latest)?$/.exec(url.pathname);
    if (list) {
      const rel = { tag_name: 'lumen-tv-local', draft: false, assets: names.map((n, i) => ({ id: i + 1, name: n, url: `http://${host}${list[1]}/assets/${i + 1}`, browser_download_url: `http://${host}/dl/${n}` })) };
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end(JSON.stringify(list[2] ? rel : [rel]));
    }
    // The API's asset route: the file only with this Accept (else its description), like GitHub's.
    const a = /^\/repos\/[^/]+\/[^/]+\/releases\/assets\/(\d+)$/.exec(url.pathname), asset = a && names[+a[1] - 1];
    if (asset) {
      if (has('--fail-api')) { res.writeHead(502); return res.end(); }
      if (limitApi) {
        res.writeHead(403, { 'content-type': 'application/json', 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': String(Math.floor(Date.now() / 1000) + 1800) });
        return res.end('{"message":"API rate limit exceeded"}');
      }
      if (/application\/octet-stream/.test(req.headers.accept || '')) return octets(path.join(outDir, asset));
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end(JSON.stringify({ id: +a[1], name: asset }));
    }
    if (failDl && url.pathname.startsWith('/dl/')) { res.writeHead(failDl); return res.end(); }
    const m = /^\/dl\/([\w.-]+)$/.exec(url.pathname), f = m && path.join(outDir, m[1]);
    if (f && fs.existsSync(f)) { res.writeHead(200, { 'content-type': 'application/octet-stream' }); return fs.createReadStream(f).pipe(res); }
    res.writeHead(404); res.end();
  }).listen(+port, '0.0.0.0', () => console.log(`fake GitHub release of ${outDir} on :${port}`));
}

function versionCode(v) { const [a, b, c] = v.split('.').map(Number); return a * 1e6 + b * 1e3 + c; }

async function main() {
  if (has('--serve')) return serve(opt('--serve') || 8765);
  const seq = process.env.SEQ || String(Math.floor(Date.now() / 1000));
  if (!has('--no-build')) execFileSync(path.join(__dirname, 'build.sh'), ['--release'], { stdio: 'inherit', env: { ...process.env, SEQ: seq } });
  const bundle = opt('--bundle') || path.join(tv, 'shell', 'src-tauri', 'bundle');
  const bseq = fs.readFileSync(path.join(bundle, 'seq'), 'utf8').trim(), client = fs.readFileSync(path.join(bundle, 'client'), 'utf8').trim();
  const version = /^version = "(.*)"/m.exec(fs.readFileSync(path.join(tv, 'shell', 'src-tauri', 'Cargo.toml'), 'utf8'))[1];
  const apkVersion = process.env.APK_VERSION || version;
  const apkName = `lumen-tv-${apkVersion}.apk`, apkPath = opt('--apk') || path.join(tv, 'out', apkName);
  fs.rmSync(outDir, { recursive: true, force: true }); fs.mkdirSync(outDir, { recursive: true });
  const sha = b => crypto.createHash('sha256').update(b).digest('hex');
  const files = {};
  for (const f of FILES) { const b = fs.readFileSync(path.join(bundle, f)); files[f] = { sha256: sha(b), size: b.length }; fs.writeFileSync(path.join(outDir, f), b); }
  const add = f => { const b = fs.readFileSync(path.join(bundle, f)); files[f] = { sha256: sha(b), size: b.length }; fs.writeFileSync(path.join(outDir, f), b); };
  // Format 2: content.js loads client.wasm (written with it by ./build.sh --tv): both or the release is broken.
  const content = fs.readFileSync(path.join(bundle, 'content.js'), 'utf8');
  const format2 = content.includes('/__lumen/client.wasm');
  if (format2) {
    if (!fs.existsSync(path.join(bundle, WASM_FILE))) throw new Error(`content.js loads ${WASM_FILE}, which is missing: run tv/scripts/build.sh`);
    add(WASM_FILE);
  }
  // The development variant must be built from the same sources as content.js (tv/scripts/build.sh builds both).
  const fresh = f => fs.existsSync(path.join(bundle, f)) && fs.statSync(path.join(bundle, f)).mtimeMs >= fs.statSync(path.join(bundle, 'content.js')).mtimeMs - 15 * 60e3;
  if (fresh(DEV_FILE) && (!format2 || fresh(DEV_WASM_FILE))) { add(DEV_FILE); if (format2) add(DEV_WASM_FILE); }
  else console.warn(`no fresh ${DEV_FILE}: the release carries no telemetry variant`);
  const minShell = Math.max(+(process.env.MIN_SHELL || 1), format2 ? versionCode(WASM_SHELL) : 1);
  const manifest = { schema: 1, seq: +bseq, client, min_shell: minShell, files };
  if (fs.existsSync(apkPath)) {
    const apk = fs.readFileSync(apkPath);
    manifest.apk = { version_code: +(process.env.APK_VERSION_CODE || versionCode(apkVersion)), version: apkVersion, file: apkName, sha256: sha(apk), size: apk.length };
    fs.writeFileSync(path.join(outDir, apkName), apk);
    // The same APK under a fixed name: .../releases/latest/download/lumen-tv.apk is the newest app for the
    // first install (the short link typed into Downloader points there).
    fs.writeFileSync(path.join(outDir, LATEST_APK), apk);
  } else console.warn(`no APK at ${apkPath}: the release carries the bundle only`);
  const mbytes = Buffer.from(JSON.stringify(manifest, null, 2) + '\n');
  const keyPath = process.env.LUMEN_TV_KEY || path.join(os.homedir(), '.config', 'lumen-tv', 'release-key.pem');
  const sig = crypto.sign(null, mbytes, crypto.createPrivateKey(fs.readFileSync(keyPath))).toString('base64');
  // The app's key must accept it (a mismatched key would publish a release no TV installs).
  const pub = Buffer.from(fs.readFileSync(path.join(tv, 'keys', 'release.pub'), 'utf8').trim(), 'base64');
  const spki = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), pub]);
  if (!crypto.verify(null, mbytes, crypto.createPublicKey({ key: spki, format: 'der', type: 'spki' }), Buffer.from(sig, 'base64'))) throw new Error('signature does not verify with tv/keys/release.pub');
  fs.writeFileSync(path.join(outDir, 'lumen-tv-manifest.json'), mbytes);
  fs.writeFileSync(path.join(outDir, 'lumen-tv-manifest.json.sig'), sig + '\n');
  console.log(`signed release in ${outDir}: client ${client}, seq ${bseq}, shell ${version}, min_shell ${minShell}${format2 ? ' (format 2: client.wasm)' : ''}`);
  if (has('--dry-run')) return;

  const repo = process.env.LUMEN_TV_REPO || 'justerquate-sys/redline', token = process.env.GITHUB_TOKEN;
  if (!token) throw new Error('GITHUB_TOKEN is not set (or use --dry-run)');
  const gh = async (url, init = {}) => {
    const r = await fetch(url, { ...init, headers: { authorization: `Bearer ${token}`, accept: 'application/vnd.github+json', 'x-github-api-version': '2022-11-28', ...(init.headers || {}) } });
    if (!r.ok) throw new Error(`${init.method || 'GET'} ${url}: HTTP ${r.status} ${await r.text()}`);
    return r.json();
  };
  const tag = `lumen-tv-${version}-${bseq}`;
  const rel = await gh(`https://api.github.com/repos/${repo}/releases`, { method: 'POST', body: JSON.stringify({
    // "latest" is what /releases/latest/download/lumen-tv.apk resolves to. (The tag prefix and the asset names keep
    // "lumen-tv": installed apps look for exactly these names.)
    tag_name: tag, make_latest: 'true', name: `Redline for TV ${version} (client ${client.split(' ')[0]})`,
    body: `Redline for Google TV / Android TV.\n\n- Client ${client}\n- Shell ${version}\n\nThe app updates itself from this release (signed lumen-tv-manifest.json). First install: see tv/README.md.`,
  }) });
  for (const name of fs.readdirSync(outDir)) {
    const b = fs.readFileSync(path.join(outDir, name));
    await gh(`https://uploads.github.com/repos/${repo}/releases/${rel.id}/assets?name=${encodeURIComponent(name)}`, { method: 'POST', body: b, headers: { 'content-type': 'application/octet-stream' } });
    console.log('uploaded', name);
  }
  console.log(rel.html_url);
}
main().catch(e => { console.error(e.message || e); process.exit(1); });
