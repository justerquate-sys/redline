// A stand-in for the local helper with the same routes and shapes (the Rust contract tests in helper/ pin the real
// ones), so the e2e suite can drive every state. It also counts every request: the unconfigured checks rely on that.
'use strict';
const http = require('node:http');

const ACTIVE = new Set(['queued', 'downloading', 'preparing', 'transcribing']);
const ROOT_TOKEN = 'a'.repeat(64);

function makeState() {
  return {
    configured: false, hasKey: false, endpoint: '', braveLogin: false, tested: 0, saveBodies: [],
    export: { directory: '/synthetic/Transcripts', enabled: true, format: 'md' },
    pickerPolls: 0,
    folders: [{ id: 'f-motion', name: 'Motion', kind: 'manual' }, { id: 'f-chan', name: 'Some channel', kind: 'channel' }],
    defaultFolder: null,
    jobs: [], batch: 'batch-1',
    previews: {}, reveal: [], revealDirs: [], copies: [], attachments: {}, exports: [],
    statsFail: false, jobsDelayMs: 0,
    finishedBatch: false,
  };
}

function job(id, video, status, extra = {}) {
  return { id, video_id: video, url: `https://www.youtube.com/watch?v=${video}`, title: extra.title ?? `Synthetic ${video}`, status, progress: status === 'completed' ? 100 : 0, document_available: status === 'completed', created_at: extra.created_at ?? new Date(Date.now()).toISOString(), folder_id: extra.folder_id ?? null, metadata: { channel: 'Synthetic channel', duration_seconds: 3725, published_date: '20260828' }, batch_id: 'batch-1', ...extra };
}

function counts(state) {
  const r = { total: state.jobs.length, queued: 0, processing: 0, completed: 0, saved: 0, removed: 0, failed: 0, cancelled: 0, save_failed: 0, missing: 0, unsaved: 0, saving: 0, discovering_channels: 0, waiting_channels: 0, discovery_unknown: false, discovery_complete: true, finished: false };
  for (const j of state.jobs) {
    if (j.status === 'queued') r.queued++;
    else if (ACTIVE.has(j.status)) r.processing++;
    else if (j.status === 'completed') { r.completed++; if (j.saved !== false) r.saved++; else r.unsaved++; }
    else if (j.status in r) r[j.status]++;
  }
  r.discovering_channels = Object.values(state.previews).filter(p => p.status === 'scanning').length;
  r.waiting_channels = Object.values(state.previews).filter(p => p.status === 'waiting').length;
  r.finished = !r.queued && !r.processing && !r.discovering_channels && !r.waiting_channels;
  return r;
}

function queueStatus(state) {
  const c = counts(state);
  const attention = state.jobs.filter(j => j.status === 'failed' || (j.status === 'completed' && j.saved === false)).map(j => ({ id: j.id, video_id: j.video_id, title: j.title, kind: j.status === 'failed' ? 'failed' : 'unsaved', message: j.error || 'This transcript was not saved.' }));
  return { scope: 'all', ...c, attention, attention_count: attention.length, batch_progress: { ...c, scope: 'batch', id: state.batch } };
}

function document(j) {
  return `# ${j.title}\n\nChannel: Synthetic channel\nPublished: 2026-08-28\nSource: ${j.url}\n\n${j.text ?? `First paragraph of ${j.video_id}.\n\nSecond paragraph, with enough words to read.`}\n`;
}

async function start(token) {
  const state = makeState();
  const log = [];
  let down = false;
  const route = (req, url, body) => {
    const p = url.pathname, m = req.method, seg = p.split('/').filter(Boolean);
    const J = (v, s = 200) => [s, v];
    if (m === 'GET' && p === '/health') return J({ status: 'ok', configured: state.configured, provider: 'azure', model: 'MAI-Transcribe-2', dependencies: { ffmpeg: true, ffprobe: true, yt_dlp: true }, version: '2.0.1' });
    if (req.headers.authorization !== `Bearer ${token}`) return J({ detail: 'The helper token is missing or incorrect. Update the extension settings.' }, 401);
    const settings = () => ({ endpoint: state.endpoint, has_api_key: state.hasKey, configured: state.configured, model: 'MAI-Transcribe-2', provider: 'azure', use_brave_login: state.braveLogin });
    const library = () => ({ folders: state.folders, default_folder_id: state.defaultFolder, jobs: [] });
    if (p === '/settings' && m === 'GET') return J(settings());
    if (p === '/settings' && m === 'POST') {
      state.saveBodies.push(body);
      // As the real helper: a Foundry project URL becomes its Speech resource endpoint; a blank key keeps the saved one.
      const f = /^https:\/\/([a-z0-9-]+)\.services\.ai\.azure\.com\/api\/projects\//.exec(body.endpoint || '');
      state.endpoint = f ? `https://${f[1]}.cognitiveservices.azure.com` : body.endpoint;
      if (body.api_key) state.hasKey = true;
      if (typeof body.use_brave_login === 'boolean') state.braveLogin = body.use_brave_login;
      state.configured = state.hasKey && !!state.endpoint;
      return J(settings());
    }
    if (p === '/settings/test') { state.tested++; return J({ ok: true, endpoint: state.endpoint, model: 'MAI-Transcribe-2', provider: 'azure', message: 'Azure accepted the MAI-Transcribe-2 test request.' }); }
    if (p === '/library') return J(library());
    if (p === '/library/stats') {
      if (state.statsFail) return J({ detail: 'Could not count files.' }, 500);
      const f = url.searchParams.get('folder_id');
      const n = state.jobs.filter(j => j.status === 'completed' && (!f || j.folder_id === f)).length;
      return J({ saved_here: n, saved_videos: n, availability: { saved_here: true }, scan_complete: true, inventory_complete: true, details_available: true, destination_selected: true, destination_available: true, source: 'files' });
    }
    if (p === '/library/export-folders') return J({ directory: state.export.directory, root_token: ROOT_TOKEN, folders: state.folders.filter(f => f.kind !== 'gone').map(f => ({ name: f.name })) });
    if (p === '/library/attachments' && m === 'POST') {
      const id = 'b'.repeat(32);
      state.attachments[id] = body;
      return J({ id, filename: 'Transcripts.zip', count: 3, bytes: 1234, root_token: body.root_token, excluded_folders: body.excluded_folders || [] });
    }
    if (seg[0] === 'library' && seg[1] === 'attachments' && seg[3] === 'copy') { state.copies.push(seg[2]); return J({ copied: true, filename: 'Transcripts.zip', count: 3 }); }
    if (seg[0] === 'library' && seg[1] === 'attachments' && seg[3] === 'reveal') { state.reveal.push(seg[2]); return J({ revealed: true }); }
    if (p === '/folders' && m === 'POST') { const f = { id: 'f-' + body.name.toLowerCase().replace(/\W/g, ''), name: body.name, kind: 'manual' }; state.folders.push(f); return J(f); }
    if (p === '/library/default-folder') { state.defaultFolder = body.folder_id; return J(library()); }
    if (p === '/export-settings' && m === 'GET') return J(state.export);
    if (p === '/export-settings' && m === 'POST') { state.export = { ...state.export, ...body }; return J(state.export); }
    if (p === '/export-settings/choose-directory' && m === 'POST') { state.pickerPolls = 2; return J({ pending: true }); }
    if (p === '/export-settings/choose-directory' && m === 'GET') {
      if (state.pickerPolls > 0) { state.pickerPolls--; return J({ pending: true }); }
      return J({ ...state.export, directory: '/synthetic/Chosen', pending: false, cancelled: false });
    }
    if (p === '/export-settings/reveal-directory') { state.revealDirs.push(body.folder_id ?? null); return J({ revealed: true }); }
    if (p === '/queue/status') return J(queueStatus(state));
    if (p === '/queue/attention') return J({ attention: queueStatus(state).attention });
    if (p === '/queue/cancel') {
      for (const j of state.jobs) if (ACTIVE.has(j.status)) j.status = 'cancelled';
      for (const v of Object.values(state.previews)) if (['scanning', 'waiting'].includes(v.status)) v.status = 'cancelled';
      return J({ status: queueStatus(state) });
    }
    if (p === '/jobs' && m === 'GET') return J({ jobs: state.jobs });
    if (p === '/jobs' && m === 'POST') {
      const video = new URL(body.url).searchParams.get('v');
      const had = state.jobs.find(j => j.video_id === video && !['failed', 'cancelled'].includes(j.status));
      if (had) return J(had);
      if (state.failNextStart) { state.failNextStart = false; return J({ detail: 'Azure is not configured yet.' }, 503); }
      const j = job('job-' + video.replace(/[^\w-]/g, ''), video, 'queued', { folder_id: body.folder_id ?? state.defaultFolder });
      state.jobs.unshift(j);
      return J(j);
    }
    if (p === '/jobs/move') { const moved = state.jobs.filter(j => body.job_ids.includes(j.id)); for (const j of moved) j.folder_id = body.folder_id; return J({ jobs: moved }); }
    if (p === '/exports') {
      state.exports.push(body);
      const n = body.job_ids ? body.job_ids.length : state.jobs.filter(j => j.status === 'completed' && (!('folder_id' in body) || j.folder_id === body.folder_id)).length;
      return J({ saved_count: n, failed_count: 0, removed_duplicate_count: 0, skipped_removed_count: 0, errors: [] });
    }
    if (p === '/videos/status') {
      const out = {};
      for (const id of body.video_ids) {
        const j = state.jobs.find(x => x.video_id === id);
        if (j) out[id] = { status: j.status, saved: j.status === 'completed' && j.saved !== false, title: 'must not reach a tab', export_path: '/private/must-not-reach-a-tab' };
      }
      return J({ statuses: out });
    }
    if (p === '/channels/import' || p === '/channels/preview') {
      if (state.previewFail) return J({ detail: 'Discovery could not start.' }, 500);
      const id = 'pv' + (Object.keys(state.previews).length + 1);
      state.previews[id] = { id, channel_name: 'Synthetic channel', scope: body.scope, include_shorts: body.include_shorts, status: 'scanning', processed_count: 4, found_count: 2, queued_count: 2, existing_count: 1, skipped_old: 3, skipped_unknown: 0, skipped_unavailable: 1, warnings: [], progress: { total: 2, saved: 0, queued: 2, processing: 0, saving: 0, discovery_complete: false, finished: false } };
      return J(state.previews[id]);
    }
    if (seg[0] === 'channels' && seg[1] === 'previews') {
      const v = state.previews[seg[2]];
      if (!v) return J({ detail: 'Not found.' }, 404);
      if (seg[3] === 'cancel') { v.status = 'cancelled'; return J(v); }
      if (seg[3] === 'queue') return J(v);
      return J(v);
    }
    if (seg[0] === 'jobs' && seg[1]) {
      const j = state.jobs.find(x => x.id === seg[1]);
      if (!j) return J({ detail: 'Transcript not found.' }, 404);
      if (seg[2] === 'cancel') { j.status = 'cancelled'; return J(j); }
      if (seg[2] === 'document') return J({ filename: j.title + '.md', content: document(j), mime_type: 'text/markdown;charset=utf-8', ...(j.sections ? { sections: j.sections } : {}) });
      return J(j);
    }
    if (seg[0] === 'videos' && seg[2] === 'cancel') {
      const j = state.jobs.find(x => x.video_id === seg[1]);
      if (!j) return J({ status: null, saved: false });
      if (ACTIVE.has(j.status)) j.status = 'cancelled';
      return J({ status: j.status, saved: j.status === 'completed' });
    }
    if (seg[0] === 'videos' && seg[2] === 'reveal') {
      const j = state.jobs.find(x => x.video_id === seg[1]);
      if (!j || j.status !== 'completed') return J({ detail: 'No completed transcript exists for this video.' }, 404);
      if (state.revealFail) return J({ detail: 'The saved transcript could not be verified.' }, 409);
      j.saved = true;
      state.reveal.push(seg[1]);
      return J({ revealed: true });
    }
    return J({ detail: 'The requested helper endpoint was not found.' }, 404);
  };
  const server = http.createServer((req, res) => {
    let raw = '';
    req.on('data', c => { raw += c; });
    req.on('end', () => {
      if (down) { req.socket.destroy(); return; }
      const url = new URL(req.url, 'http://127.0.0.1');
      let body = {};
      try { body = raw ? JSON.parse(raw) : {}; } catch {}
      log.push({ method: req.method, path: url.pathname, query: url.search, body, at: Date.now() });
      const [status, value] = route(req, url, body);
      const send = () => {
        res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
        res.end(JSON.stringify(value));
      };
      if (state.jobsDelayMs && url.pathname === '/jobs') setTimeout(send, state.jobsDelayMs); else send();
    });
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  return {
    state, log, job, port: server.address().port,
    origin: `http://127.0.0.1:${server.address().port}`,
    down(on) { down = on; if (on) server.closeAllConnections(); },
    close() { server.closeAllConnections(); server.close(); },
    reset() { Object.assign(state, makeState()); log.length = 0; },
  };
}

module.exports = { start, job };
