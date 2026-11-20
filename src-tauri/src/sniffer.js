// Injected into the hidden sniffer WebView (see main.rs). Runs before page scripts.
// It never touches the Tauri IPC: findings are reported by attempting a
// navigation to siphonpull-sniff://..., which the Rust side intercepts and cancels.
(() => {
  'use strict';
  if (window.__spSniffer) return;
  try {
    Object.defineProperty(window, '__spSniffer', { value: true });
  } catch (e) {}

  const MEDIA = /\.(?:m3u8|mpd)(?![A-Za-z0-9])/i;
  const isTop = window.top === window;
  const seen = new Set();

  // A hidden window reports itself as not visible, and many players refuse to start then.
  try {
    Object.defineProperty(document, 'hidden', { get: () => false, configurable: true });
    Object.defineProperty(document, 'visibilityState', { get: () => 'visible', configurable: true });
  } catch (e) {}

  /* ---------- reporting (serialised so one navigation never stomps another) ---------- */
  const outbox = [];
  let draining = false;
  function drain() {
    draining = true;
    const next = outbox.shift();
    if (!next) {
      draining = false;
      return;
    }
    try {
      window.location.href = 'siphonpull-sniff://' + next[0] + '?' + new URLSearchParams(next[1]).toString();
    } catch (e) {}
    setTimeout(drain, 150);
  }
  function send(kind, data) {
    if (outbox.length > 40) return;
    outbox.push([kind, data]);
    if (!draining) drain();
  }
  function toTop(kind, data) {
    if (isTop) {
      send(kind, data);
    } else {
      try {
        window.top.postMessage({ __spSniff: kind, data: data }, '*');
      } catch (e) {}
    }
  }

  function urlOf(input) {
    if (typeof input === 'string') return input;
    if (input && typeof input.url === 'string') return input.url;
    return String(input);
  }

  function report(input) {
    try {
      const abs = new URL(urlOf(input), location.href).href;
      if (!/^https?:/i.test(abs) || !MEDIA.test(abs) || seen.has(abs)) return;
      seen.add(abs);
      toTop('hit', {
        u: abs,
        r: location.href,
        ua: navigator.userAgent,
        t: document.title || '',
      });
    } catch (e) {}
  }

  /* ---------- hooks ---------- */
  const origFetch = window.fetch;
  if (typeof origFetch === 'function') {
    window.fetch = function (input) {
      report(input);
      return origFetch.apply(this, arguments);
    };
  }

  const origOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    report(url);
    return origOpen.apply(this, arguments);
  };

  // Safety net for requests the two hooks cannot see (native <video src>, Safari/WKWebView HLS).
  try {
    new PerformanceObserver((list) => {
      list.getEntries().forEach((entry) => report(entry.name));
    }).observe({ type: 'resource', buffered: true });
  } catch (e) {}

  /* ---------- nudge click-to-load players ---------- */
  const PLAY_SELECTORS = [
    '.vjs-big-play-button',
    '.jw-icon-display',
    '.plyr__control--overlaid',
    '.ytp-large-play-button',
    '.fp-play',
    '[aria-label="Play"]',
    'button[aria-label*="play" i]',
  ];
  let tries = 0;
  const nudge = setInterval(() => {
    if (++tries > 8) {
      clearInterval(nudge);
      return;
    }
    document.querySelectorAll('video').forEach((v) => {
      try {
        v.muted = true;
        const p = v.play();
        if (p && p.catch) p.catch(() => {});
      } catch (e) {}
    });
    for (const sel of PLAY_SELECTORS) {
      const el = document.querySelector(sel);
      if (el) {
        try {
          el.click();
        } catch (e) {}
        break;
      }
    }
  }, 1500);

  /* ---------- iframes ---------- */
  if (isTop) {
    // Only used when this script also runs inside frames (all-frames injection).
    window.addEventListener('message', (e) => {
      const m = e.data;
      if (m && m.__spSniff === 'hit' && m.data && typeof m.data.u === 'string') {
        m.data.t = document.title || '';
        send('hit', m.data);
      }
    });

    // Cross-origin iframes cannot be hooked from here, so report their URLs;
    // Rust navigates the hidden window into one if the top page shows nothing.
    const scanFrames = () => {
      document.querySelectorAll('iframe[src]').forEach((f) => {
        try {
          const u = new URL(f.getAttribute('src'), location.href).href;
          if (/^https?:/i.test(u) && !seen.has(u)) {
            seen.add(u);
            send('frame', { u: u });
          }
        } catch (e) {}
      });
    };
    window.addEventListener('load', () => {
      setTimeout(scanFrames, 1500);
      setTimeout(scanFrames, 4500);
    });
  }
})();
