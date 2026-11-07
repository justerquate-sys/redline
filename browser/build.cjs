// Writes the loadable extension (extension/) from the Rust build and the browser sources.
//   node browser/build.cjs          public build (what is committed and shipped)
//   node browser/build.cjs --dev    development build (debug-center delivery, auto-reload)
//   node browser/build.cjs --target=tv   the TV app's bundle (tv/shell/src-tauri/bundle), public: content.js
//                                        without the Wasm (client.wasm next to it, served by the app's shell)
//   node browser/build.cjs --target=tv --dev   its development variant: only content-dev.js + client-dev.wasm
//                                        (telemetry), which the app runs while its owner turned telemetry on
// Run through ./build.sh, which compiles the Rust crate and runs wasm-bindgen first.
//
// content.js bundles bridge.js + the wasm-bindgen glue + the embedded Wasm. The Wasm bytes travel
// as base64 inside a block comment of a never-called function, read back via
// Function.prototype.toString (V8 skips comments far faster than it scans a multi-megabyte string
// literal). The base64 alphabet has no '*'.
//
// Development-only code in browser/*.js sits between `/* @dev-start */` and `/* @dev-end */`
// lines: the public build drops those blocks, the development build keeps them. The Rust side of
// the same switch is the Cargo feature `dev` (see build.sh).
'use strict';
const fs = require('node:fs'), path = require('node:path'), assert = require('node:assert/strict');
const root = path.join(__dirname, '..'), src = __dirname, pkg = path.join(root, 'target', 'pkg');
const args = process.argv.slice(2);
const DEV = args.includes('--dev');
// Targets. Chromium (Chrome, Brave, Edge, Vivaldi, Opera…) is the extension; another browser (e.g.
// Firefox) adds its manifest template here and writes its own folder. `tv` is the bundle the Google
// TV / Android TV app (tv/) injects into its WebView: the same content.js, plus the extension's
// stylesheets and request rules, a bundle sequence and the client version (see tv/README.md).
const TARGETS = {
  chromium: { out: path.join(root, 'extension'), manifest: 'manifest.json' },
  tv: { out: path.join(root, 'tv', 'shell', 'src-tauri', 'bundle'), manifest: 'manifest.json', bundle: true },
};
const target = TARGETS[(args.find(a => a.startsWith('--target=')) || '--target=chromium').slice(9)];
assert.ok(target, 'unknown --target');
const DEV_HOST = 'http://127.0.0.1:47321/*';

// Keeps (dev) or drops (public) the @dev blocks.
function flavor(text, name) {
  const out = [];
  let inDev = false;
  for (const line of text.split('\n')) {
    const t = line.trim();
    if (t === '/* @dev-start */') { assert.ok(!inDev, name + ': nested @dev-start'); inDev = true; continue; }
    if (t === '/* @dev-end */') { assert.ok(inDev, name + ': @dev-end without start'); inDev = false; continue; }
    if (!inDev || DEV) out.push(line);
  }
  assert.ok(!inDev, name + ': unclosed @dev-start');
  return out.join('\n');
}

function cut(text, from, to) {
  const a = text.indexOf(from), b = text.indexOf(to);
  assert.ok(a >= 0 && b > a, 'Unexpected wasm-bindgen glue near: ' + from);
  return text.slice(0, a) + text.slice(b);
}
let glue = fs.readFileSync(path.join(pkg, 'ytc_client.js'), 'utf8');
glue = cut(glue, '    async function __wbg_load(', '    function initSync(');
glue = cut(glue, '    let __wbg_init_promise;', '    return Object.assign(__wbg_init, { initSync }, exports);');
glue = glue.replace('Object.assign(__wbg_init, { initSync }, exports)', 'Object.assign({ initSync }, exports)');
assert.ok(!glue.includes('__wbg_init('), 'async init left in glue');
const bridge = flavor(fs.readFileSync(path.join(src, 'bridge.js'), 'utf8'), 'bridge.js');
const wasm = fs.readFileSync(path.join(pkg, 'ytc_client_bg.wasm'));
// How content.js gets its Wasm. The extension: the bytes travel inside content.js (base64 in a comment). The TV app
// (format 2, shell 0.3.0+): the shell serves them at WASM_URL; the fetch and a streaming compile start before
// anything else, off the main thread, and the boot follows when the module is ready (the 1.1 MB of base64 no longer
// has to be scanned with the script on every page load: ~35 ms at 1×, 150–200 ms on a TV's CPU).
const WASM_URL = '/__lumen/client.wasm';
const tvBoot = !!target.bundle;
const bootEmbedded = `function payload(){/*${wasm.toString('base64')}*/}
try {
  const text = String(payload), enc = text.slice(text.indexOf('/*') + 2, text.lastIndexOf('*/'));
  const bytes = typeof Uint8Array.fromBase64 === 'function' ? Uint8Array.fromBase64(enc) : Uint8Array.from(atob(enc), c => c.charCodeAt(0));
  wasm_bindgen.initSync({ module: bytes });
  document.documentElement.dataset.ytcWasm = performance.now().toFixed(1);
  if (ytcNative) wasm_bindgen.boot_native(); else wasm_bindgen.boot();
} catch (e) { ytcBootFailed(e); }`;
const bootServed = `ytcModule.then(module => {
  wasm_bindgen.initSync({ module });
  document.documentElement.dataset.ytcWasm = performance.now().toFixed(1);
  if (ytcNative) wasm_bindgen.boot_native(); else wasm_bindgen.boot();
}).catch(ytcBootFailed);`;
const out = flavor(`// Generated by browser/build.cjs from src/ (Rust) and browser/bridge.js${DEV ? ' (development build)' : ''}. Do not edit.
(() => {
'use strict';
// Native fallback pages (?yt-native=1) keep YouTube running; the client only
// watches for leaving that video so the next page returns to the client.
const ytcNative = /[?&]yt-native=1(?:&|$)/.test(location.search);
const ytcExtension = /^(chrome|moz)-extension:$/.test(location.protocol);
${tvBoot ? `if (!ytcNative) try { window.stop(); } catch {}` : `// YouTube's document stops at once, except Home's ('/'), the one the client may read instead of asking again:
// boot() stops it (bridge page_stop) or keeps it streaming with its nodes held out of the page (bridge doc_hold).
// Nothing of it runs or renders meanwhile: the HTML parser waits while this script runs.
if (!ytcNative && !ytcExtension && location.pathname !== '/') try { window.stop(); } catch {}`}
${tvBoot ? `// (After the stop, which would abort it.)
const ytcModule = WebAssembly.compileStreaming(fetch('${WASM_URL}', { credentials: 'omit', cache: 'no-store' }));
` : ''}document.documentElement.dataset.ytcScript = performance.now().toFixed(1);
${bridge}
${glue}
// Crash fallback: native YouTube for this page.
function ytcBootFailed(e) {
  if (!ytcNative && !ytcExtension) try { window.stop(); } catch {}
  console.error('[Redline] boot failed', e);
  if (ytcExtension) { const el = document.getElementById('ytc-popup'); if (el) el.textContent = 'Preferences couldn’t load. Close this window and open it again.'; }
  try { chrome.runtime.sendMessage({ ytcIncident: JSON.stringify({ kind: 'boot-failed', reason: String(e && e.stack || e), at: Date.now(), path: location.pathname + location.search }) }).catch(() => {}); } catch {}
  if (!ytcNative && !ytcExtension) { const u = new URL(location.href); u.searchParams.set('yt-native', '1'); location.replace(u.href); }
}
${tvBoot ? bootServed : bootEmbedded}
/* @dev-start */
// Development auto-update (background.js): announce this tab and ask for a build check.
// Once the extension has reloaded, this old script's context is invalidated; the
// tab then refreshes itself into the new build, waiting while media plays.
let devPing = 0;
const playing = () => [...document.querySelectorAll('video,audio')].some(m => !m.paused && !m.ended);
const refresh = () => { if (playing()) setTimeout(refresh, 2000); else setTimeout(() => location.reload(), 400); };
const ping = () => { try { chrome.runtime.sendMessage('ytc-dev-check').catch(() => {}); } catch { clearInterval(devPing); refresh(); } };
ping(); devPing = setInterval(ping, 4000);
/* @dev-end */
})();
`, 'content.js');
const background = flavor(fs.readFileSync(path.join(src, 'background.js'), 'utf8'), 'background.js');

// The manifest: the template is the public manifest; the development build adds the local
// collector's host permission, a "(dev)" name and the build id.
const manifest = JSON.parse(fs.readFileSync(path.join(src, target.manifest), 'utf8'));
assert.ok(!manifest.host_permissions.includes(DEV_HOST) && !manifest.version_name, 'the manifest template is the public one');
if (DEV) {
  manifest.name += ' (dev)';
  manifest.host_permissions.splice(1, 0, DEV_HOST);
}

const dir = target.out;
if (target.bundle && DEV) {
  // The TV bundle's development variant: content-dev.js only (the public files, seq and client stay as they are;
  // a release signs both, the app runs this one only while telemetry is on).
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'content-dev.js'), out);
  fs.writeFileSync(path.join(dir, 'client-dev.wasm'), wasm);
  console.log(`tv bundle, development variant: ${path.join(dir, 'content-dev.js')} (${out.length} bytes, wasm ${wasm.length})`);
  process.exit(0);
}
if (target.bundle) {
  // The TV bundle: content.js as built here; the stylesheets and rules are the extension's own files.
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'content.js'), out);
  fs.writeFileSync(path.join(dir, 'client.wasm'), wasm);
  const hash = require('node:crypto').createHash('sha256').update(out).update(wasm);
  for (const f of ['client.css', 'native.css', 'rules.json']) {
    const b = fs.readFileSync(path.join(root, 'extension', f));
    fs.writeFileSync(path.join(dir, f), b);
    hash.update(b);
  }
  // seq orders bundles (the app installs a newer one); client names it in the app's About.
  const seq = process.env.SEQ || String(Math.floor(Date.now() / 1000));
  assert.ok(/^\d+$/.test(seq), 'SEQ must be a number');
  fs.writeFileSync(path.join(dir, 'seq'), seq + '\n');
  fs.writeFileSync(path.join(dir, 'client'), `${manifest.version} (${hash.digest('hex').slice(0, 16)})\n`);
  console.log(`tv bundle: ${dir} (content.js ${out.length} bytes, client.wasm ${wasm.length} bytes, seq ${seq})`);
  process.exit(0);
}
fs.writeFileSync(path.join(dir, 'content.js'), out);
fs.writeFileSync(path.join(dir, 'background.js'), background);
fs.copyFileSync(path.join(src, 'popup.html'), path.join(dir, 'popup.html'));
const idFile = path.join(dir, 'build-id.txt');
if (DEV) {
  // Any change to a shipped file changes the build id. It is written to build-id.txt (read fresh
  // from disk by background.js) and baked into the manifest's version_name (what the browser
  // actually loaded, unaffected by the worker sleeping and restarting); a mismatch means "reload".
  const hash = require('node:crypto').createHash('sha256');
  hash.update(JSON.stringify(manifest));
  for (const f of ['content.js', 'client.css', 'native.css', 'rules.json', 'background.js', 'popup.html', 'popup.css']) hash.update(fs.readFileSync(path.join(dir, f)));
  const buildId = hash.digest('hex').slice(0, 16);
  manifest.version_name = `${manifest.version} (${buildId})`;
  fs.writeFileSync(idFile, buildId + '\n');
} else {
  fs.rmSync(idFile, { force: true });
}
fs.writeFileSync(path.join(dir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`${DEV ? 'development' : 'public'} build: content.js ${out.length} bytes (wasm ${wasm.length} bytes, glue+bridge ${out.length - Math.ceil(wasm.length / 3) * 4} bytes)`);
