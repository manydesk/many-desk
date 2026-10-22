// Tauri shell adapter: provides the same window.md API that the Electron preload
// exposes (src/main/preload.js), backed by Rust commands in src-tauri. In Electron
// window.md already exists and this file does nothing.
(() => {
  if (window.md || !window.__TAURI__) return;
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const ua = navigator.userAgent;
  const platform = /Mac/.test(ua) ? 'darwin' : /Windows/.test(ua) ? 'win32' : 'linux';

  // One listener per channel, replaced on re-registration (like removeAllListeners).
  const handlers = {};
  const on = (channel) => (fn) => {
    handlers[channel] = fn;
    if (!handlers[`_${channel}`]) handlers[`_${channel}`] = listen(channel, (e) => handlers[channel] && handlers[channel](e.payload));
  };

  window.md = {
    platform,
    // The OS shows its own screen picker (macOS / WebView2): no screen list here,
    // and getDisplayMedia must be called straight from the Accept click.
    // macOS / WebView2 show the OS screen picker; Linux captures natively from our own
    // screen list (rtc-native.js), like the Electron build.
    systemPicker: platform !== 'linux',
    // Native protocol (MDP) with peers that are current Tauri builds.
    mdp: true,
    videoMirror: platform === 'darwin',

    permStatus: () => invoke('perm_status'),
    requestAccessibility: () => invoke('perm_request_accessibility'),
    requestMicrophone: async () => true, // the WebView asks the OS itself
    openSettings: (which) => invoke('perm_open_settings', { which }),

    listScreens: async () => (await invoke('screens_list').catch(() => [])).map((s) => ({ ...s, displayId: s.id })),
    selectSource: async (id) => {
      if (platform !== 'linux' || !window.__nativeScreens) return;
      const s = (await window.__nativeScreens.list()).find((x) => x.id === id);
      window.__nativeScreens.select(s || null);
    },
    setCaptureDisplay: (width, height) => invoke('host_set_display', { width, height }),
    hostSession: (o) => invoke('host_session', { active: !!o.active, control: !!o.control, clipboard: !!o.clipboard }),
    inject: (msg, who) => { invoke('input_event', { msg, who: who || null }).catch((e) => invoke('debug_log', { msg: `input_event: ${e}` })); },
    forgetPointer: (id) => { invoke('input_forget', { id }); },

    watchClipboard: (on) => invoke('clipboard_watch', { on: !!on }),
    setClipboard: (text) => invoke('clipboard_set', { text }),
    onClipboard: on('clipboard:changed'),

    captureMenu: (on) => invoke('menu_capture', { on: !!on }),
    setFullscreen: (on) => invoke('set_fullscreen', { on: !!on }),
    onFullscreen: on('win:fullscreen'),

    appInfo: () => invoke('app_info'),
    rvSubscribe: (topic, lookback) => invoke('rv_subscribe', { topic, lookback: lookback === 0 ? 0 : null }),
    rvUnsubscribe: (topic) => invoke('rv_unsubscribe', { topic }),
    rvPublish: (topic, content) => invoke('rv_publish', { topic, content }).catch(() => ({ ok: 0, sent: 0 })),
    rvStatus: () => invoke('rv_status'),
    cloudflareTurn: (keyId, token) => invoke('turn_cloudflare', { keyId, token }),
    onRv: on('rv:message'),
    onRvStatus: on('rv:status'),

    fileBegin: (id, name) => invoke('file_begin', { id, name }),
    fileChunk: (id, buf) => invoke('file_chunk', new Uint8Array(buf), { headers: { 'x-id': id } }),
    fileEnd: (id, ok) => invoke('file_end', { id, ok: !!ok }),
    reveal: (path) => invoke('file_reveal', { path }),
  };

  // MANYDESK_DEBUG=1: report every <video> element's state (diagnosing rendering).
  invoke('debug_autoaccept').then((on) => {
    if (!on) return;
    setInterval(() => {
      const m = document.getElementById('rv-modal');
      if (m && !m.classList.contains('hidden')) document.getElementById('rv-accept').click();
      invoke('debug_log', { msg: `id=${document.getElementById('my-id').textContent}` });
    }, 1000);
  });

  invoke('debug_autoconnect').then((id) => {
    if (!id) return;
    const go = () => setTimeout(() => {
      const i = document.getElementById('rv-peer-id');
      i.value = id;
      i.dispatchEvent(new Event('input'));
      const f = i.closest('form');
      if (f) f.requestSubmit(); else i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    }, 4000);
    if (document.readyState === 'complete') go(); else window.addEventListener('load', go);
  });

  invoke('debug_enabled').then((on) => {
    if (!on) return;
    const log = (m) => invoke('debug_log', { msg: m });
    window.addEventListener('error', (e) => log(`error ${e.message} @${e.filename}:${e.lineno}`));
    (async () => {
      const q = async (c) => { try { return (await AudioEncoder.isConfigSupported({ codec: 'opus', sampleRate: 48000, numberOfChannels: 1 })).supported; } catch (e) { return e.name; } };
      log(`caps AudioEncoder=${typeof AudioEncoder} AudioDecoder=${typeof AudioDecoder} opus=${await q()} worklet=${typeof AudioWorkletNode}`);
    })();
    setInterval(() => {
      document.querySelectorAll('video').forEach((v, i) => {
        let px = '';
        try {
          const c = document.createElement('canvas'); c.width = 4; c.height = 4;
          const g = c.getContext('2d'); g.drawImage(v, 0, 0, 4, 4);
          const d = g.getImageData(0, 0, 4, 4).data;
          px = [0, 3, 12, 15, 5].map((k) => d.slice(k * 4, k * 4 + 3).join(',')).join(' ');
        } catch (e) { px = 'n/a ' + e.name; }
        const r = v.getBoundingClientRect();
        const cs = getComputedStyle(v);
        log(`video#${i} op=${cs.opacity} vis=${cs.visibility} disp=${cs.display} z=${cs.zIndex} tf=${cs.transform} paused=${v.paused} ready=${v.readyState} size=${v.videoWidth}x${v.videoHeight} box=${Math.round(r.width)}x${Math.round(r.height)} visible=${v.offsetParent !== null} center=${px} hasSrc=${!!v.srcObject}`);
      });
    }, 3000);
  });

  // WKWebView has no Web Notification API: route it to the notification plugin.
  if (typeof window.Notification !== 'function' && window.__TAURI__.notification) {
    window.Notification = class {
      constructor(title, opts = {}) {
        window.__TAURI__.notification.sendNotification({ title, body: opts.body || '' });
      }
      static get permission() { return 'granted'; }
      static requestPermission() { return Promise.resolve('granted'); }
    };
  }
})();
