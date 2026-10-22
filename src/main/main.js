const {
  app, BrowserWindow, ipcMain, desktopCapturer, session, systemPreferences,
  shell, clipboard, Menu, powerSaveBlocker,
} = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { InputInjector } = require('./input');
const { Rendezvous } = require('./rendezvous');

// Expose real LAN IPs as ICE host candidates instead of mDNS ".local" names,
// so peers on the same network connect directly without any server.
app.commandLine.appendSwitch('disable-features', 'WebRtcHideLocalIpsWithMdns');

const isMac = process.platform === 'darwin';
let win = null;
const injector = new InputInjector();
const rendezvous = new Rendezvous();
const host = { sourceId: null, active: false, control: false, clipboard: false, sleepBlock: null };
let clipboardTimer = null;
let lastClipboard = '';
const incomingFiles = new Map();
let overlay = null; // host-only window that labels each viewer's pointer
let overlayReady = false;
const overlayQueue = []; // messages sent while the overlay page is still loading

function createWindow() {
  win = new BrowserWindow({
    width: 1180,
    height: 760,
    minWidth: 900,
    minHeight: 600,
    title: 'ManyDesk',
    ...(isMac ? { titleBarStyle: 'hiddenInset' } : { autoHideMenuBar: true }),
    backgroundColor: '#0f1115',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  win.loadFile(path.join(__dirname, '..', 'renderer', 'index.html'));
  if (!app.isPackaged) {
    win.webContents.on('console-message', (e) => console.log(`[renderer:${e.level}] ${e.message}`));
  }
  win.on('closed', () => { win = null; });
  // Session full screen uses the real window full screen (not the HTML Fullscreen API,
  // which would leave on Esc — Esc must reach the remote computer).
  win.on('enter-full-screen', () => win.webContents.send('win:fullscreen', true));
  win.on('leave-full-screen', () => win.webContents.send('win:fullscreen', false));
}

// --- application menu -------------------------------------------------------
// While the viewer is controlling a remote machine, shortcuts such as Cmd+C / Cmd+V
// must reach the remote side instead of the local Edit menu.
function setMenu(captureMode) {
  // Windows text fields copy/paste natively; a menu bar would only steal Ctrl shortcuts.
  if (!isMac) return Menu.setApplicationMenu(null);
  const template = [
    { role: 'appMenu' },
    captureMode
      ? { label: 'Edit', submenu: [] }
      : { role: 'editMenu' },
    { label: 'View', submenu: [{ role: 'togglefullscreen' }, { role: 'toggleDevTools' }] },
    { role: 'windowMenu' },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

// --- clipboard sync -----------------------------------------------------------
function startClipboardWatch() {
  stopClipboardWatch();
  lastClipboard = clipboard.readText();
  clipboardTimer = setInterval(() => {
    const t = clipboard.readText();
    if (t !== lastClipboard) {
      lastClipboard = t;
      if (win && t) win.webContents.send('clipboard:changed', t);
    }
  }, 700);
}

function stopClipboardWatch() {
  if (clipboardTimer) clearInterval(clipboardTimer);
  clipboardTimer = null;
}

// --- files ------------------------------------------------------------------
function downloadsDir() {
  const dir = path.join(app.getPath('downloads'), 'ManyDesk');
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

function uniquePath(dir, name) {
  const safe = path.basename(String(name || 'file')).replace(/[\/\\:\0]/g, '_').slice(0, 200) || 'file';
  const ext = path.extname(safe);
  const stem = safe.slice(0, safe.length - ext.length);
  let p = path.join(dir, safe);
  for (let i = 1; fs.existsSync(p); i++) p = path.join(dir, `${stem} (${i})${ext}`);
  return p;
}

// ---- viewer pointer overlay ---------------------------------------------------
// A transparent, click-through window over the shared display. Each viewer's actions
// show a coloured marker with its name, so the host sees who is doing what. It is
// excluded from screen capture (content protection), so viewers never see each other.
function ensureOverlay() {
  if (overlay && !overlay.isDestroyed()) return overlay;
  const { bounds } = injector.display();
  overlay = new BrowserWindow({
    ...bounds,
    transparent: true,
    backgroundColor: '#00000000',
    frame: false,
    hasShadow: false,
    focusable: false,
    resizable: false,
    movable: false,
    skipTaskbar: true,
    show: false,
    enableLargerThanScreen: true,
    webPreferences: { preload: path.join(__dirname, 'overlay-preload.js'), contextIsolation: true, sandbox: true },
  });
  overlay.setIgnoreMouseEvents(true);
  // MANYDESK_OVERLAY_IN_CAPTURE is only for the automated check that proves the
  // protection is what keeps the labels out of the viewers' picture.
  overlay.setContentProtection(!process.env.MANYDESK_OVERLAY_IN_CAPTURE);
  overlay.setAlwaysOnTop(true, 'screen-saver');
  overlay.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  overlay.loadFile(path.join(__dirname, '..', 'renderer', 'overlay.html'));
  overlayReady = false;
  overlay.webContents.once('did-finish-load', () => {
    if (!overlay || overlay.isDestroyed()) return;
    overlayReady = true;
    for (const [ch, data] of overlayQueue.splice(0)) overlay.webContents.send(ch, data);
    overlay.showInactive();
  });
  overlay.on('closed', () => { overlay = null; overlayReady = false; });
  return overlay;
}

function overlaySend(channel, data) {
  ensureOverlay();
  if (overlayReady) overlay.webContents.send(channel, data);
  else overlayQueue.push([channel, data]);
}

function closeOverlay() {
  if (overlay && !overlay.isDestroyed()) overlay.destroy();
  overlay = null;
  overlayReady = false;
  overlayQueue.length = 0;
}

function cleanWho(who) {
  if (!who || !Number.isInteger(who.id)) return null;
  return {
    id: who.id,
    name: String(who.name || '').slice(0, 40),
    tag: String(who.tag || '').slice(0, 16),
    color: /^#[0-9a-f]{6}$/i.test(who.color) ? who.color : '#e5463b',
    acts: Object.fromEntries(['down', 'scroll', 'key'].map((k) => [k, String((who.acts && who.acts[k]) || '').slice(0, 24)])),
  };
}

function registerIpc() {
  // Only macOS gates screen capture and input injection behind privacy permissions.
  ipcMain.handle('perm:status', () => (isMac ? {
    screen: systemPreferences.getMediaAccessStatus('screen'),
    accessibility: systemPreferences.isTrustedAccessibilityClient(false),
  } : { screen: 'granted', accessibility: true }));
  ipcMain.handle('perm:microphone', () => (isMac ? systemPreferences.askForMediaAccess('microphone') : true));
  ipcMain.handle('perm:request-accessibility', () => !isMac || systemPreferences.isTrustedAccessibilityClient(true));
  ipcMain.handle('perm:open-settings', (_e, which) => {
    if (!isMac) return;
    const anchor = which === 'screen' ? 'Privacy_ScreenCapture' : 'Privacy_Accessibility';
    return shell.openExternal(`x-apple.systempreferences:com.apple.preference.security?${anchor}`);
  });

  ipcMain.handle('screens:list', async () => {
    const sources = await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: { width: 320, height: 200 } });
    return sources.map((s) => ({ id: s.id, name: s.name, displayId: s.display_id, thumb: s.thumbnail.toDataURL() }));
  });

  ipcMain.handle('host:select-source', (_e, sourceId, displayId) => {
    host.sourceId = sourceId;
    injector.setDisplay(displayId);
    if (overlay && !overlay.isDestroyed()) overlay.setBounds(injector.display().bounds);
  });

  ipcMain.handle('host:session', (_e, opts) => {
    host.active = !!opts.active;
    host.control = host.active && !!opts.control;
    const wantClipboard = host.active && !!opts.clipboard;
    if (host.control) injector.start(); else { injector.stop(); closeOverlay(); }
    if (host.active && host.sleepBlock === null) host.sleepBlock = powerSaveBlocker.start('prevent-display-sleep');
    if (!host.active && host.sleepBlock !== null) { powerSaveBlocker.stop(host.sleepBlock); host.sleepBlock = null; }
    if (wantClipboard !== host.clipboard) wantClipboard ? startClipboardWatch() : stopClipboardWatch();
    host.clipboard = wantClipboard;
  });

  // Viewer-side clipboard sync uses the same watcher.
  ipcMain.handle('clipboard:watch', (_e, on) => {
    if (host.clipboard) return;
    on ? startClipboardWatch() : stopClipboardWatch();
  });
  ipcMain.handle('clipboard:set', (_e, text) => {
    if (typeof text !== 'string' || text.length > 1_000_000) return;
    lastClipboard = text;
    clipboard.writeText(text);
  });

  ipcMain.on('input:event', (_e, msg, who) => {
    if (!(host.active && host.control)) return;
    const point = injector.handle(msg);
    const w = cleanWho(who);
    if (w && msg && ['move', 'down', 'up', 'scroll', 'key'].includes(msg.t)) {
      overlaySend('cursor', { ...w, kind: msg.t, ...(point || {}) });
    }
  });
  ipcMain.on('input:forget', (_e, id) => {
    if (overlay && !overlay.isDestroyed()) overlaySend('forget', id);
  });

  ipcMain.handle('menu:capture', (_e, on) => setMenu(!!on));
  ipcMain.handle('win:set-fullscreen', (_e, on) => { if (win) win.setFullScreen(!!on); });

  // --- rendezvous (connect by ID) ---
  const topicOk = (t) => typeof t === 'string' && /^[0-9a-f]{64}$/.test(t);
  ipcMain.handle('rv:subscribe', (_e, topic, lookback) => {
    if (process.env.MANYDESK_DEBUG) console.log('[rv] subscribe', String(topic).slice(0, 8), 'lookback', lookback);
    return topicOk(topic) && rendezvous.subscribe(topic, lookback === 0); // 0 = live requests only
  });
  ipcMain.handle('rv:unsubscribe', (_e, topic) => topicOk(topic) && rendezvous.unsubscribe(topic));
  ipcMain.handle('rv:publish', (_e, topic, content) => {
    if (!topicOk(topic) || typeof content !== 'string' || content.length > 60000) return { ok: 0, sent: 0 };
    if (process.env.MANYDESK_DEBUG) console.log('[rv] publish', topic.slice(0, 8), content.length, 'bytes');
    return rendezvous.publish(topic, content);
  });
  ipcMain.handle('rv:status', () => rendezvous.status());

  // Cloudflare Realtime TURN (1,000 GB/month free): turn the user's own TURN key into
  // short-lived ICE credentials. Runs here because the page's CSP allows no network calls.
  ipcMain.handle('turn:cloudflare', async (_e, keyId, token) => {
    if (!/^[A-Za-z0-9_-]{8,128}$/.test(String(keyId)) || typeof token !== 'string' || !token.trim()) {
      return { error: 'cf.badInput' };
    }
    try {
      const res = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${keyId}/credentials/generate-ice-servers`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token.trim()}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ ttl: 86400 }),
        signal: AbortSignal.timeout(10000),
      });
      // Errors are message keys; the page shows them in the user's language.
      if (!res.ok) return { error: res.status === 401 || res.status === 403 ? 'cf.auth' : res.status === 404 ? 'cf.notFound' : 'cf.http', status: res.status };
      const data = await res.json();
      const turn = (data.iceServers || []).find((s) => s.username && s.credential);
      if (!turn) return { error: 'cf.noTurn' };
      const urls = (Array.isArray(turn.urls) ? turn.urls : [turn.urls]).filter((u) => /^turns?:/.test(u));
      return { urls, username: turn.username, credential: turn.credential, expires: Date.now() + 86400 * 1000 };
    } catch (err) {
      return { error: 'cf.net', detail: err.message };
    }
  });
  // System UI languages (macOS, Windows and Linux alike) for the default app language.
  ipcMain.handle('app:info', () => ({
    name: os.hostname().replace(/\.local$/, ''),
    platform: process.platform,
    version: app.getVersion(),
    languages: [...app.getPreferredSystemLanguages(), app.getLocale()].filter(Boolean),
  }));

  ipcMain.handle('file:begin', (_e, id, name, size) => {
    const p = uniquePath(downloadsDir(), name);
    incomingFiles.set(id, { path: p, stream: fs.createWriteStream(p), size, written: 0 });
    return p;
  });
  ipcMain.handle('file:chunk', (_e, id, buf) => {
    const f = incomingFiles.get(id);
    if (!f) return;
    const b = Buffer.from(buf);
    f.written += b.length;
    return new Promise((resolve) => (f.stream.write(b) ? resolve() : f.stream.once('drain', resolve)));
  });
  ipcMain.handle('file:end', (_e, id, ok) => {
    const f = incomingFiles.get(id);
    if (!f) return null;
    incomingFiles.delete(id);
    return new Promise((resolve) => f.stream.end(() => {
      if (!ok) { fs.rmSync(f.path, { force: true }); resolve(null); } else resolve(f.path);
    }));
  });
  ipcMain.handle('file:reveal', (_e, p) => {
    if (typeof p === 'string' && p.startsWith(downloadsDir())) shell.showItemInFolder(p);
  });
}

app.whenReady().then(() => {
  setMenu(false);
  registerIpc();

  // Renderer calls getDisplayMedia(); we answer with the screen the user picked in the UI.
  session.defaultSession.setDisplayMediaRequestHandler(async (_req, callback) => {
    const sources = await desktopCapturer.getSources({ types: ['screen'] });
    const src = sources.find((s) => s.id === host.sourceId) || sources[0];
    if (!src) return callback({});
    callback({ video: src });
  }, { useSystemPicker: false });

  rendezvous.on('message', (m) => {
    if (process.env.MANYDESK_DEBUG) console.log('[rv] message', m.topic.slice(0, 8), 'from', m.pubkey.slice(0, 8));
    if (win) win.webContents.send('rv:message', m);
  });
  rendezvous.on('status', (s) => win && win.webContents.send('rv:status', s));
  rendezvous.start();

  createWindow();
  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('window-all-closed', () => {
  injector.stop();
  rendezvous.stop();
  app.quit();
});
