(() => {
  'use strict';

  const T = window.__TAURI__;
  const invoke = T.core.invoke;
  const listen = T.event.listen;

  const $ = (sel) => document.querySelector(sel);
  const els = {
    url: $('#url'),
    hint: $('#hint'),
    modeVideo: $('#mode-video'),
    modeAudio: $('#mode-audio'),
    quality: $('#quality'),
    dir: $('#dir'),
    dirPath: $('#dir-path'),
    watch: $('#watch'),
    queue: $('#queue'),
    empty: $('#empty'),
    clear: $('#clear'),
    version: $('#version'),
  };

  const DEFAULT_HINT = 'YouTube, TikTok and 1000+ other sites';
  const MAX_PARALLEL = 2;
  const ACTIVE = new Set(['downloading', 'processing']);
  const FINISHED = new Set(['done', 'error', 'cancelled']);

  /* ---------- persistence ---------- */
  const store = {
    get(key, fallback) {
      try {
        const raw = localStorage.getItem('siphonpull.' + key);
        return raw === null ? fallback : JSON.parse(raw);
      } catch {
        return fallback;
      }
    },
    set(key, value) {
      try {
        localStorage.setItem('siphonpull.' + key, JSON.stringify(value));
      } catch {
        /* storage unavailable: settings just won't persist */
      }
    },
  };

  /* ---------- state ---------- */
  const state = {
    mode: store.get('mode', 'video'),
    quality: store.get('quality', '1080p'),
    dir: store.get('dir', ''),
    watch: store.get('watch', true),
    items: new Map(),
    order: [], // newest first
    rows: new Map(),
    lastClipboard: '',
  };

  const VIDEO_QUALITIES = [
    ['1080p', '1080p'],
    ['4k', '4K'],
    ['best', 'Best available'],
  ];

  /* ---------- helpers ---------- */
  function extractUrls(text) {
    return (text || '').split(/\s+/).filter((t) => /^https?:\/\/\S+$/i.test(t));
  }

  function formatDuration(seconds) {
    if (!seconds || !isFinite(seconds)) return '';
    const s = Math.round(seconds);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = String(s % 60).padStart(2, '0');
    return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
  }

  function formatSpeed(bytesPerSec) {
    if (!bytesPerSec || !isFinite(bytesPerSec)) return '';
    const units = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
    let v = bytesPerSec;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${units[i]}`;
  }

  function formatEta(seconds) {
    if (seconds === null || seconds === undefined || !isFinite(seconds)) return '';
    return `${formatDuration(seconds) || '0:00'} left`;
  }

  function baseName(path) {
    return (path || '').split(/[\\/]/).pop();
  }

  function showHint(text, warn = false) {
    els.hint.textContent = text;
    els.hint.classList.toggle('warn', warn);
    clearTimeout(showHint.timer);
    if (text !== DEFAULT_HINT) {
      showHint.timer = setTimeout(() => showHint(DEFAULT_HINT), 3500);
    }
  }

  /* ---------- settings UI ---------- */
  function renderOptions() {
    const isAudio = state.mode === 'audio';
    els.modeVideo.classList.toggle('active', !isAudio);
    els.modeAudio.classList.toggle('active', isAudio);

    els.quality.innerHTML = '';
    const options = isAudio ? [['mp3', 'MP3']] : VIDEO_QUALITIES;
    for (const [value, label] of options) {
      const opt = document.createElement('option');
      opt.value = value;
      opt.textContent = label;
      els.quality.appendChild(opt);
    }
    els.quality.disabled = isAudio;
    if (!isAudio) {
      if (!VIDEO_QUALITIES.some(([v]) => v === state.quality)) state.quality = '1080p';
      els.quality.value = state.quality;
    }

    els.dirPath.textContent = state.dir || 'not set';
    els.dirPath.title = state.dir;
    els.watch.checked = state.watch;
  }

  els.modeVideo.addEventListener('click', () => {
    state.mode = 'video';
    store.set('mode', state.mode);
    renderOptions();
  });
  els.modeAudio.addEventListener('click', () => {
    state.mode = 'audio';
    store.set('mode', state.mode);
    renderOptions();
  });
  els.quality.addEventListener('change', () => {
    if (state.mode === 'video') {
      state.quality = els.quality.value;
      store.set('quality', state.quality);
    }
  });
  els.watch.addEventListener('change', () => {
    state.watch = els.watch.checked;
    store.set('watch', state.watch);
    if (state.watch) checkClipboard();
  });
  els.dir.addEventListener('click', async () => {
    try {
      const picked = await T.dialog.open({
        directory: true,
        multiple: false,
        defaultPath: state.dir || undefined,
        title: 'Choose download folder',
      });
      if (typeof picked === 'string' && picked) {
        state.dir = picked;
        store.set('dir', state.dir);
        renderOptions();
      }
    } catch (err) {
      showHint(`Could not open folder picker: ${err}`, true);
    }
  });

  /* ---------- queue rendering ---------- */
  function createRow(item) {
    const li = document.createElement('li');
    li.className = 'row';
    li.innerHTML =
      '<span class="dot"></span>' +
      '<div class="text"><div class="name"></div><div class="meta"></div></div>' +
      '<div class="pct"></div>' +
      '<div class="actions"></div>' +
      '<div class="bar"><i></i></div>';
    state.rows.set(item.id, li);
    return li;
  }

  function metaText(item) {
    switch (item.status) {
      case 'fetching':
        return 'Fetching info…';
      case 'queued':
        return ['Queued', item.sub].filter(Boolean).join(' · ');
      case 'downloading':
        return [formatSpeed(item.speed), formatEta(item.eta)].filter(Boolean).join(' · ') || 'Downloading…';
      case 'processing':
        return 'Processing…';
      case 'done':
        return baseName(item.path) || item.sub || 'Done';
      case 'error':
        return item.error || 'Failed';
      case 'cancelled':
        return 'Cancelled';
      default:
        return '';
    }
  }

  function actionButton(label, title, handler) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'act';
    b.textContent = label;
    b.title = title;
    b.addEventListener('click', handler);
    return b;
  }

  function renderRow(item) {
    let li = state.rows.get(item.id);
    if (!li) li = createRow(item);
    li.dataset.status = item.status;

    const name = li.querySelector('.name');
    name.textContent = item.title;
    name.title = item.title;
    li.querySelector('.meta').textContent = metaText(item);

    const pct = li.querySelector('.pct');
    pct.textContent =
      item.status === 'done'
        ? '100%'
        : ACTIVE.has(item.status) || item.percent > 0
          ? `${Math.floor(item.percent)}%`
          : '';
    li.querySelector('.bar i').style.width = (item.status === 'done' ? 100 : item.percent) + '%';

    const actions = li.querySelector('.actions');
    actions.replaceChildren();
    if (item.status === 'fetching' || item.status === 'queued' || ACTIVE.has(item.status)) {
      actions.appendChild(actionButton('cancel', 'Cancel', () => cancelItem(item.id)));
    } else if (item.status === 'done') {
      if (item.path) {
        actions.appendChild(actionButton('show', 'Show in folder', () => reveal(item)));
      }
      actions.appendChild(actionButton('remove', 'Remove from list', () => removeItem(item.id)));
    } else {
      actions.appendChild(actionButton('retry', 'Try again', () => retryItem(item.id)));
      actions.appendChild(actionButton('remove', 'Remove from list', () => removeItem(item.id)));
    }
    return li;
  }

  function syncList() {
    els.empty.classList.toggle('hidden', state.order.length > 0);
    const wanted = state.order.map((id) => renderRow(state.items.get(id)));
    // Re-attach in order only when needed to keep the DOM stable.
    wanted.forEach((li, index) => {
      if (els.queue.children[index] !== li) els.queue.insertBefore(li, els.queue.children[index] || null);
    });
    while (els.queue.children.length > wanted.length) els.queue.lastElementChild.remove();
  }

  let frameQueued = false;
  function update() {
    if (frameQueued) return;
    frameQueued = true;
    requestAnimationFrame(() => {
      frameQueued = false;
      syncList();
    });
  }

  /* ---------- queue logic ---------- */
  async function enqueue(url) {
    const id = crypto.randomUUID();
    const item = {
      id,
      url,
      title: url,
      sub: '',
      status: 'fetching',
      percent: 0,
      speed: null,
      eta: null,
      path: null,
      error: '',
      opts: { mode: state.mode, quality: state.quality, dir: state.dir },
    };
    state.items.set(id, item);
    state.order.unshift(id);
    update();
    await fetchMetadata(item);
  }

  async function fetchMetadata(item) {
    item.status = 'fetching';
    item.error = '';
    update();
    try {
      const info = await invoke('fetch_info', { url: item.url });
      if (item.status !== 'fetching') return; // cancelled or removed meanwhile
      item.title = info.title || item.url;
      item.sub = [info.uploader, formatDuration(info.duration)].filter(Boolean).join(' · ');
      item.status = 'queued';
      update();
      pump();
    } catch (err) {
      if (item.status !== 'fetching') return;
      fail(item, err);
    }
  }

  function fail(item, err) {
    item.status = 'error';
    item.error = String(err && err.message ? err.message : err);
    item.speed = null;
    item.eta = null;
    update();
    pump();
  }

  function pump() {
    let running = 0;
    for (const item of state.items.values()) if (ACTIVE.has(item.status)) running++;
    // Oldest queued item first.
    for (let i = state.order.length - 1; i >= 0 && running < MAX_PARALLEL; i--) {
      const item = state.items.get(state.order[i]);
      if (item.status === 'queued') {
        startItem(item);
        running++;
      }
    }
  }

  async function startItem(item) {
    item.status = 'downloading';
    item.percent = 0;
    update();
    try {
      await invoke('start_download', {
        req: {
          id: item.id,
          url: item.url,
          mode: item.opts.mode,
          quality: item.opts.quality,
          dir: item.opts.dir || state.dir,
        },
      });
    } catch (err) {
      fail(item, err);
    }
  }

  async function cancelItem(id) {
    const item = state.items.get(id);
    if (!item) return;
    if (ACTIVE.has(item.status)) {
      try {
        await invoke('cancel_download', { id });
      } catch (err) {
        showHint(String(err), true);
      }
      return; // the backend emits "download-cancelled" once the process exits
    }
    item.status = 'cancelled';
    update();
  }

  function retryItem(id) {
    const item = state.items.get(id);
    if (!item) return;
    item.percent = 0;
    item.speed = null;
    item.eta = null;
    item.error = '';
    item.opts = { mode: state.mode, quality: state.quality, dir: state.dir };
    if (item.title === item.url) {
      fetchMetadata(item);
    } else {
      item.status = 'queued';
      update();
      pump();
    }
  }

  function removeItem(id) {
    const item = state.items.get(id);
    if (!item || ACTIVE.has(item.status)) return;
    state.items.delete(id);
    state.order = state.order.filter((x) => x !== id);
    const li = state.rows.get(id);
    if (li) li.remove();
    state.rows.delete(id);
    update();
  }

  async function reveal(item) {
    try {
      await T.opener.revealItemInDir(item.path);
    } catch (err) {
      showHint(`Could not open folder: ${err}`, true);
    }
  }

  els.clear.addEventListener('click', () => {
    for (const id of [...state.order]) {
      const item = state.items.get(id);
      if (item && FINISHED.has(item.status)) removeItem(id);
    }
  });

  /* ---------- backend events ---------- */
  listen('download-progress', ({ payload }) => {
    const item = state.items.get(payload.id);
    if (!item || FINISHED.has(item.status)) return;
    // Video downloads fetch two streams (video, then audio), so the raw
    // percentage resets once. Keep the bar monotonic and hold at 99%.
    item.percent = Math.max(item.percent, Math.min(99, payload.percent));
    item.speed = payload.speed;
    item.eta = payload.eta;
    item.status = payload.percent >= 99.5 ? 'processing' : 'downloading';
    update();
  });

  listen('download-done', ({ payload }) => {
    const item = state.items.get(payload.id);
    if (!item) return;
    item.status = 'done';
    item.percent = 100;
    item.path = payload.path || null;
    item.speed = null;
    item.eta = null;
    update();
    pump();
  });

  listen('download-error', ({ payload }) => {
    const item = state.items.get(payload.id);
    if (item) fail(item, payload.message);
  });

  listen('download-cancelled', ({ payload }) => {
    const item = state.items.get(payload.id);
    if (!item) return;
    item.status = 'cancelled';
    item.speed = null;
    item.eta = null;
    update();
    pump();
  });

  /* ---------- paste box ---------- */
  function submitText(text) {
    const urls = extractUrls(text);
    if (urls.length === 0) {
      showHint("That doesn't look like a link.", true);
      return false;
    }
    if (!state.dir) {
      showHint('Choose a download folder first.', true);
      return false;
    }
    urls.forEach((u) => enqueue(u));
    showHint(urls.length === 1 ? 'Added to queue.' : `Added ${urls.length} links to queue.`);
    return true;
  }

  els.url.addEventListener('paste', (event) => {
    const text = event.clipboardData ? event.clipboardData.getData('text') : '';
    if (extractUrls(text).length > 0) {
      event.preventDefault();
      if (submitText(text)) els.url.value = '';
    }
  });

  els.url.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter') return;
    const text = els.url.value.trim();
    if (!text) return;
    if (submitText(text)) els.url.value = '';
  });

  /* ---------- clipboard watcher ---------- */
  async function checkClipboard() {
    if (!state.watch || !state.dir) return;
    try {
      const text = ((await T.clipboardManager.readText()) || '').trim();
      if (!text || text === state.lastClipboard) return;
      state.lastClipboard = text;
      const urls = extractUrls(text);
      if (urls.length > 0) {
        urls.forEach((u) => enqueue(u));
        showHint(urls.length === 1 ? 'Caught a link from your clipboard.' : `Caught ${urls.length} links from your clipboard.`);
      }
    } catch {
      /* clipboard empty, non-text, or not readable right now */
    }
  }
  window.addEventListener('focus', checkClipboard);

  /* ---------- init ---------- */
  async function init() {
    if (!state.dir) {
      try {
        state.dir = await invoke('default_dir');
        store.set('dir', state.dir);
      } catch {
        state.dir = '';
      }
    }
    renderOptions();
    syncList();

    invoke('ytdlp_version')
      .then((v) => {
        els.version.textContent = `yt-dlp ${v}`;
      })
      .catch(() => {
        els.version.textContent = 'yt-dlp: not found';
        showHint('yt-dlp was not found. Run "npm run setup" or install yt-dlp.', true);
      });

    await checkClipboard();
    els.url.focus();
  }

  init();
})();
