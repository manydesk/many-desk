const $ = (id) => document.getElementById(id);
const api = window.md;
const RV = window.RV;
// UI language: system language by default (refined below once the OS list arrives).
I18N.init(navigator.languages);
const isMac = api.platform === 'darwin';
document.body.classList.add(`os-${api.platform}`);
if (!isMac) document.querySelector('.panel.perms').classList.add('hidden');
const { Session } = window.RTC;
const HEX64 = /^[0-9a-f]{64}$/;
const OS_NAME = { darwin: 'macOS', win32: 'Windows', linux: 'Linux' };

// This computer as viewer: any number of sessions, one tab each, fully isolated.
const tabs = new Map();
let activeTab = null; // null = the Home tab is shown
let selectedScreen = null;
let appInfo = { name: '' };

// ---------------------------------------------------------------- helpers ---
function toast(text, { error = false, timeout = 4500 } = {}) {
  const el = document.createElement('div');
  el.className = 'toast' + (error ? ' err' : '');
  el.textContent = text;
  $('toasts').appendChild(el);
  if (timeout) setTimeout(() => el.remove(), timeout);
  return el;
}

function progressToast(label) {
  const el = toast(label, { timeout: 0 });
  const bar = document.createElement('div');
  bar.className = 'bar';
  bar.innerHTML = '<i></i>';
  el.appendChild(bar);
  return {
    set: (p) => { bar.firstChild.style.width = `${Math.round(p * 100)}%`; },
    done: (text, path) => {
      el.textContent = text + ' ';
      if (path) {
        const a = document.createElement('a');
        a.textContent = t('file.reveal');
        a.onclick = () => api.reveal(path);
        el.appendChild(a);
      }
      setTimeout(() => el.remove(), 8000);
    },
    fail: (text) => { el.textContent = text; el.classList.add('err'); setTimeout(() => el.remove(), 6000); },
  };
}

function fmtStats(s) {
  const parts = [s.relay ? t('st.relay') : s.lan ? t('st.lan') : `P2P · ${s.route}`];
  if (s.rtt !== null) parts.push(`${s.rtt} ms`);
  if (s.size) parts.push(s.size);
  if (s.fps) parts.push(`${s.fps} fps`);
  if (s.codec) parts.push(s.hw ? `${s.codec} HW` : s.codec);
  parts.push(s.kbps > 1000 ? `${(s.kbps / 1000).toFixed(1)} Mbit/s` : `${s.kbps} kbit/s`);
  parts.push(t('st.total', { mb: s.totalMB < 10 ? s.totalMB.toFixed(1) : Math.round(s.totalMB) }));
  return parts.join('  ·  ');
}

// ctx: the host context or a ViewerTab — anything with .session, .statsEl, .statsTimer.
function startStats(ctx) {
  clearInterval(ctx.statsTimer);
  ctx.statsTimer = setInterval(async () => {
    const s = ctx.session;
    if (!s || s.closed) return;
    try {
      const st = await s.stats();
      ctx.statsEl.textContent = fmtStats(st);
      if (ctx.onStats) ctx.onStats(st);
    } catch {}
  }, 1500);
}

function sendFiles(s, files, label) {
  for (const f of files) {
    const p = progressToast(t('file.sending', { who: label, f: f.name }));
    s.sendFile(f, p.set)
      .then(() => p.done(t('file.sent', { who: label, f: f.name })))
      .catch((e) => p.fail(t('file.failed', { who: label, f: f.name, e: e.message })));
  }
}

function bindFileEvents(s, labelFn) {
  const active = new Map();
  s.addEventListener('file-progress', ({ detail }) => {
    let p = active.get(detail.id);
    if (!p) { p = progressToast(t('file.receiving', { who: labelFn(), f: detail.name })); active.set(detail.id, p); }
    p.set(detail.progress);
  });
  s.addEventListener('file-done', ({ detail }) => {
    const p = active.get(detail.id) || progressToast('');
    active.delete(detail.id);
    p.done(t('file.received', { who: labelFn(), f: detail.name }), detail.path);
  });
}

// One shared <input type=file>; the session it is opened for receives the files.
let fileTarget = null;
function pickFiles(s, label) {
  fileTarget = { s, label };
  $('file-input').click();
}
$('file-input').onchange = (e) => {
  const t = fileTarget;
  fileTarget = null;
  if (t && t.s && t.s.connected) sendFiles(t.s, [...e.target.files], t.label);
  e.target.value = '';
};

// Chromium stops gathering as soon as a direct LAN path connects, so a code without a
// public address is only a problem if the session did not connect: check a bit later.
async function warnIfNoStun(code, s) {
  try {
    const sum = await window.RTC.inspectCode(code);
    if (sum.srflx || sum.relay) return;
    await new Promise((r) => setTimeout(r, 4000));
    if (s && (s.connected || s.closed)) return;
    toast(t('warn.noStun') + t('diag.see'), { error: true, timeout: 12000 });
  } catch {}
}

// Fresh Cloudflare TURN credentials before a connection; a failure only means no relay,
// direct paths are still tried.
async function ensureTurn() {
  const r = await window.RTC.prepareTurn();
  if (!r.ok) toast(t('turn.notReady', { e: r.error }), { error: true, timeout: 8000 });
}

// Calling play() inside a user gesture unlocks a media element for later autoplay
// (required by WebKit / Tauri on macOS; harmless elsewhere).
// WKWebView (Tauri macOS) decodes a remote MediaStream but sometimes never composites
// the <video> layer: frames are drawn into a canvas on top instead (pointer-events
// pass through to the video, so input mapping is unchanged).
function mirrorVideo(v) {
  let c = v.nextElementSibling;
  if (!c || c.tagName !== 'CANVAS') { c = document.createElement('canvas'); c.className = 'mirror'; v.after(c); }
  const g = c.getContext('2d', { alpha: false });
  const draw = () => {
    if (!v.srcObject || !c.isConnected) return;
    if (v.videoWidth) {
      if (c.width !== v.videoWidth || c.height !== v.videoHeight) { c.width = v.videoWidth; c.height = v.videoHeight; }
      g.drawImage(v, 0, 0);
    }
    if (v.requestVideoFrameCallback) v.requestVideoFrameCallback(draw); else requestAnimationFrame(draw);
  };
  if (!c.dataset.on) { c.dataset.on = '1'; draw(); }
}

function unlockMedia(...els) {
  for (const el of els) el.play().catch(() => {});
}

// ------------------------------------------------------------------ chat ---
// One ChatBox per session (each viewer tab, plus the host side): messages never
// cross between sessions.
class ChatBox {
  constructor({ panel, log, form, input, close, toggles, getSession, title, onOpenChange, onUnread }) {
    Object.assign(this, { panel, log, input, toggles, getSession, title, onOpenChange, onUnread });
    this.unread = 0;
    form.onsubmit = (e) => {
      e.preventDefault();
      const text = input.value.trim();
      const s = getSession();
      if (!text || !s || !s.connected) return;
      const ts = Date.now();
      s.send({ t: 'chat', text, ts });
      this.add(text, true, ts);
      input.value = '';
    };
    close.onclick = () => this.setOpen(false);
    toggles.forEach((b) => { b.onclick = () => this.setOpen(!this.isOpen()); });
    panel.addEventListener('mousedown', () => setCapture(false));
    this.reset();
  }

  isOpen() { return !this.panel.classList.contains('hidden'); }

  setOpen(open) {
    this.panel.classList.toggle('hidden', !open);
    if (this.onOpenChange) this.onOpenChange(open);
    if (open) {
      setCapture(false);
      this.setUnread(0);
      this.input.focus();
    }
  }

  setUnread(n) {
    this.unread = n;
    this.toggles.forEach((b) => {
      const badge = b.querySelector('.unread');
      badge.textContent = n > 9 ? '9+' : String(n);
      badge.classList.toggle('hidden', n === 0);
    });
    if (this.onUnread) this.onUnread(n);
  }

  add(text, mine, ts = Date.now()) {
    this.log.querySelector('.chat-empty')?.remove();
    const el = document.createElement('div');
    el.className = `msg ${mine ? 'me' : 'them'}`;
    el.textContent = text;
    const time = document.createElement('time');
    time.textContent = new Date(ts).toLocaleTimeString(I18N.lang, { hour: '2-digit', minute: '2-digit' });
    el.appendChild(time);
    this.log.appendChild(el);
    this.log.scrollTop = this.log.scrollHeight;
  }

  receive(text, ts, visible) {
    this.add(text.slice(0, 4000), false, ts);
    if (!this.isOpen() || !visible) this.setUnread(this.unread + 1);
    if (!document.hasFocus() || !visible) new Notification(`ManyDesk — ${this.title()}`, { body: text.slice(0, 140) });
  }

  reset() {
    this.log.innerHTML = '';
    const empty = document.createElement('div');
    empty.className = 'chat-empty';
    empty.dataset.i18n = 'chat.empty';
    empty.textContent = t('chat.empty');
    this.log.appendChild(empty);
    this.setUnread(0);
    this.setOpen(false);
  }
}

function setMicButton(b, on) {
  b.classList.toggle('on', on);
  b.title = t(on ? 'tt.micOn' : 'tt.mic');
}

async function toggleMic(ctx) {
  const s = ctx.session;
  if (!s || !s.connected) return;
  const want = !s.mic;
  try {
    if (want && !(await api.requestMicrophone())) throw new Error(t('mic.noPerm'));
    await s.setMic(want);
    if (ctx.applyIsolation) ctx.applyIsolation(ctx === activeTab);
    setMicButton(ctx.micBtn, !!s.mic);
  } catch (e) {
    toast(t('mic.failed', { e: e.message }), { error: true, timeout: 8000 });
  }
}

// ------------------------------------------------------------- navigation ---
function showPage(name) {
  document.querySelectorAll('.navitem').forEach((b) => b.classList.toggle('active', b.dataset.page === name));
  document.querySelectorAll('.page').forEach((p) => p.classList.toggle('active', p.id === `page-${name}`));
  activateTab(null);
  if (name === 'share') refreshHost();
}

document.querySelectorAll('.navitem').forEach((b) => { b.onclick = () => showPage(b.dataset.page); });
$('open-settings').onclick = () => showPage('settings');
$('home-tab').onclick = () => activateTab(null);
$('perm-banner-fix').onclick = () => showPage('share');

// Isolation between sessions: only the visible tab gets keyboard/mouse, plays its
// remote audio, receives your microphone and exchanges the clipboard. Background
// sessions keep running (picture, chat history) but hear and control nothing.
function activateTab(tab) {
  if (activeTab !== tab) setCapture(false);
  activeTab = tab || null;
  for (const t of tabs.values()) {
    const on = t === activeTab;
    t.root.classList.toggle('hidden', !on);
    t.btn.classList.toggle('active', on);
    t.applyIsolation(on);
  }
  $('home-tab').classList.toggle('active', !activeTab);
  if (!activeTab && fsMode) api.setFullscreen(false);
}

// ---------------------------------------------------------- permissions ---
async function refreshPerms() {
  const st = await api.permStatus();
  $('perm-screen-dot').className = 'dot ' + (st.screen === 'granted' ? 'ok' : 'bad');
  $('perm-ax-dot').className = 'dot ' + (st.accessibility ? 'ok' : 'bad');
  $('perm-banner').classList.toggle('hidden', !isMac || (st.screen === 'granted' && st.accessibility));
  return st;
}
document.querySelector('[data-open="screen"]').onclick = () => api.openSettings('screen');
$('perm-ax-btn').onclick = async () => {
  if (!(await api.requestAccessibility())) api.openSettings('accessibility');
  setTimeout(refreshPerms, 1500);
};
window.addEventListener('focus', () => { if (!hostSessions.size) refreshPerms(); });

// ------------------------------------------------------------------- host ---
// This computer as host: up to MAX_HOST viewers at once. Each gets its own card,
// its own screen stream and quality, permissions, chat, microphone and file transfer.
const MAX_HOST = 5;
const hostSessions = new Map(); // key -> HostCtx
let hostSeq = 0;
// Each viewer gets a colour: its card and its pointer label on the host's screen match.
const VIEWER_COLORS = ['#e5463b', '#2f80ed', '#1f9d55', '#9b51e0', '#e67e22'];

function hostLive() {
  return [...hostSessions.values()].filter((h) => h.session && h.session.connected);
}

// The main process only needs the union (is anyone connected / may anyone control /
// use the clipboard); every viewer's own permissions are enforced per session here.
function updateHostAggregate() {
  const live = hostLive();
  api.hostSession({ active: live.length > 0, control: live.some((h) => h.allowControl()), clipboard: live.some((h) => h.allowClipboard()) });
}

function updateChatColumn() {
  document.body.classList.toggle('chat-open', [...hostSessions.values()].some((h) => h.chat && h.chat.isOpen()));
}

async function refreshHost() {
  await refreshPerms();
  // With the OS screen picker there is nothing to choose here.
  const listed = !api.systemPicker || api.mdp; // native capture shares a screen from our own list
  $('screens').closest('.panel').classList.toggle('hidden', !listed);
  if (!listed || hostSessions.size) return;
  const screens = await api.listScreens().catch(() => []);
  const box = $('screens');
  box.innerHTML = '';
  if (!screens.length) {
    box.innerHTML = '';
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.dataset.i18n = 'share.noScreen';
    hint.textContent = t('share.noScreen');
    box.appendChild(hint);
    return;
  }
  if (!selectedScreen || !screens.find((s) => s.id === selectedScreen.id)) selectedScreen = screens[0];
  for (const s of screens) {
    const el = document.createElement('div');
    el.className = 'screen' + (s.id === selectedScreen.id ? ' selected' : '');
    const img = document.createElement('img');
    img.src = s.thumb;
    const label = document.createElement('span');
    label.textContent = s.name;
    el.append(img, label);
    el.onclick = () => { selectedScreen = s; refreshHost(); };
    box.appendChild(el);
  }
}

// Viewers whose native (MDP) attempt failed: their WebRTC retry is accepted automatically.
const nativeFallback = new Map(); // nostr pubkey → { until, perms }

class HostCtx {
  constructor(req, perms) {
    this.key = ++hostSeq;
    this.req = req;
    this.perms = perms;
    this.name = req.name;
    const used = new Set([...hostSessions.values()].map((h) => h.color));
    this.color = VIEWER_COLORS.find((c) => !used.has(c)) || VIEWER_COLORS[this.key % VIEWER_COLORS.length];
    this.who = { id: this.key, name: req.name, tag: req.from ? RV.formatId(req.from) : '', color: this.color };
    this.session = null;
    this.hs = null;
    this.statsTimer = null;
    this.closing = false;

    this.card = $('tpl-host').content.firstElementChild.cloneNode(true);
    this.r = (n) => this.card.querySelector(`[data-r="${n}"]`);
    this.chatEl = $('tpl-host-chat').content.firstElementChild.cloneNode(true);
    const c = (n) => this.chatEl.querySelector(`[data-r="${n}"]`);
    $('host-sessions').appendChild(this.card);
    document.body.appendChild(this.chatEl);
    this.statsEl = this.r('stats');
    this.micBtn = this.r('mic');
    this.audio = this.r('audio');
    this.r('name').textContent = req.name;
    this.r('from').textContent = req.from ? RV.formatId(req.from) : '';
    this.card.style.setProperty('--who', this.color);
    c('chat-with').textContent = `· ${req.name}`;
    this.r('control').checked = perms.control;
    this.r('clipboard').checked = perms.clipboard;

    this.chat = new ChatBox({
      panel: this.chatEl, log: c('chat-log'), form: c('chat-form'), input: c('chat-input'), close: c('chat-close'),
      toggles: [this.r('chat-btn')],
      getSession: () => this.session,
      title: () => this.name,
      // One host chat column at a time; the rest of the window makes room for it.
      onOpenChange: (open) => {
        if (open) for (const h of hostSessions.values()) if (h !== this && h.chat.isOpen()) h.chat.setOpen(false);
        // Mark which card the open chat belongs to (two viewers may share a computer name).
        this.card.classList.toggle('chat-active', open);
        updateChatColumn();
      },
    });
    this.r('control').onchange = () => this.pushPerms();
    this.r('clipboard').onchange = () => this.pushPerms();
    this.r('stop').onclick = () => this.close();
    this.r('send-file').onclick = () => this.session && pickFiles(this.session, this.name);
    this.micBtn.onclick = () => toggleMic(this);
    this.r('collapse').onclick = () => this.card.classList.toggle('collapsed');
    unlockMedia(this.audio); // created inside the Accept click (see unlockMedia)
    hostSessions.set(this.key, this);
  }

  allowControl() { return this.r('control').checked; }
  allowClipboard() { return this.r('clipboard').checked; }

  pushPerms() {
    if (this.session && this.session.connected) {
      this.session.send({ t: 'perm', control: this.allowControl(), clipboard: this.allowClipboard(), os: api.platform });
    }
    updateHostAggregate();
  }

  // After "Accept": capture the screen, send the offer, wait for the answer.
  // Must be called synchronously from the Accept click: with the OS screen picker
  // (Tauri: WebKit / WebView2) getDisplayMedia is only allowed inside the user gesture.
  async start(req) {
    let s = null;
    // Native protocol: the screen is captured in Rust, so no OS picker / getDisplayMedia.
    const picked = !req.native && api.systemPicker ? navigator.mediaDevices.getDisplayMedia({ video: { frameRate: { ideal: 60, max: 60 } }, audio: false }) : null;
    try {
      let stream = null;
      if (req.native) {
        if (!selectedScreen) await refreshHost();
        if (selectedScreen) api.setCaptureDisplay(selectedScreen.width, selectedScreen.height);
      } else if (picked) {
        stream = await picked;
        const st = stream.getVideoTracks()[0].getSettings();
        if (st.width && st.height) api.setCaptureDisplay(st.width, st.height); // which monitor input maps to
      } else {
        if (!selectedScreen) await refreshHost();
        if (!selectedScreen) throw new Error(t('err.noScreen'));
        await api.selectSource(selectedScreen.id, selectedScreen.displayId);
      }
      await ensureTurn();
      if (this.closing) { if (stream) stream.getTracks().forEach((x) => x.stop()); return; }
      s = this.session = req.native ? new RTC.NativeSession('host', { screen: selectedScreen && selectedScreen.id }) : new Session('host');
      this.wire(s);
      const code = await s.createInvite(stream);
      warnIfNoStun(code, s);
      const reply = RV.randomTopic();
      // Keep a local handle and arm the timeout before sending: sending waits for relay
      // acknowledgements, and the viewer's answer can arrive (and end this handshake)
      // before all of them are in — especially over the fast MQTT brokers.
      const hs = this.hs = { topic: reply, key: req.key, peer: req.peer, timer: null };
      hs.timer = setTimeout(() => {
        if (!s.connected && !s.closed) s.close(t('err.noAnswer'));
      }, 60000);
      api.rvSubscribe(reply);
      const r = await RV.send(req.reply, req.key, { t: 'offer', code, reply, name: appInfo.name, os: api.platform });
      if (!r.ok && this.hs === hs && !s.connected) throw new Error(t('err.relayRejected'));
    } catch (e) {
      if (s && s.connected) return; // already connected; nothing to undo
      toast(t('host.failed', { who: this.name, e: e.message }), { error: true });
      RV.send(req.reply, req.key, { t: 'reject', reason: e.message });
      this.close(true);
    }
  }

  async onAnswer(m) {
    const hs = this.hs;
    if (m.pubkey !== hs.peer) return; // only the requester that was accepted may answer
    const msg = await RV.open(hs.key, m.content);
    if (!msg || msg.t !== 'answer' || typeof msg.code !== 'string') return;
    this.endHandshake();
    try {
      const sas = await this.session.acceptAnswer(msg.code);
      this.r('sas').textContent = sas;
      this.chatEl.querySelector('[data-r="chat-with"]').textContent = `· ${this.name} · ${sas}`;
    } catch (e) {
      this.session.close(t('err.badAnswer', { e: e.message }));
    }
  }

  endHandshake() {
    if (!this.hs) return;
    api.rvUnsubscribe(this.hs.topic);
    clearTimeout(this.hs.timer);
    this.hs = null;
  }

  wire(s) {
    bindFileEvents(s, () => this.name);
    s.addEventListener('audio', ({ detail: stream }) => {
      this.audio.srcObject = stream;
      this.audio.play().catch(() => {});
    });
    s.addEventListener('open', () => {
      this.r('state').dataset.i18n = 'host.connected';
      this.r('state').textContent = t('host.connected');
      this.pushPerms();
      startStats(this);
      new Notification('ManyDesk', { body: t('host.joined', { who: this.name }) });
    });
    s.addEventListener('message', ({ detail: m }) => {
      if (m.t === 'chat' && typeof m.text === 'string') this.chat.receive(m.text, Number(m.ts) || Date.now(), true);
      else if (m.t === 'mic') this.r('remote-mic').classList.toggle('hidden', !m.on);
      else if (m.t === 'quality') s.setQuality(m.mode); // this viewer's stream only
      else if (m.t === 'view') s.setView(m.w, m.h); // encode at the viewer's stage size
      // Written without echo: the main process does not report its own write as a change,
      // so one viewer's clipboard never travels on to the other viewers.
      else if (m.t === 'clip') { if (this.allowClipboard() && typeof m.text === 'string') api.setClipboard(m.text); }
      else if (m.t === 'bye') s.close(t('peer.ended'));
      else if (this.allowControl()) api.inject(m, { ...this.who, acts: overlayActs() }); // labelled with this viewer on the host's screen
    });
    s.addEventListener('closed', () => {
      rememberReport(s);
      // A native attempt that found no route: the viewer retries over WebRTC (TURN-capable);
      // accept that retry without asking again.
      if (s.native && !s.connected && !this.closing && this.req) {
        nativeFallback.set(this.req.peer, { until: Date.now() + 60000, perms: this.perms });
        this.close(true);
        return;
      }
      if (!this.closing && !s.silent) {
        toast(`${this.name}: ${s.connected ? (s.reason || t('sess.ended')) : (s.reason || t('conn.notMade')) + t('diag.see')}`,
          { error: !s.connected, timeout: s.connected ? 4500 : 15000 });
      }
      this.close(true);
    });
  }

  close(silent = false) {
    if (this.closing) return;
    this.closing = true;
    this.endHandshake();
    const s = this.session;
    // Let go of anything this viewer was still holding down.
    if (s && s.connected && this.allowControl()) api.inject({ t: 'release' });
    api.forgetPointer(this.key);
    if (s && !s.closed) {
      if (silent) s.silent = true;
      s.close();
    }
    clearInterval(this.statsTimer);
    this.audio.srcObject = null;
    this.chat.setOpen(false);
    this.card.remove();
    this.chatEl.remove();
    hostSessions.delete(this.key);
    updateChatColumn();
    updateHostAggregate();
    if (!hostSessions.size) refreshHost();
  }
}

// ------------------------------------------------------- viewer sessions ---
// Between a Mac and a PC, Cmd and Ctrl play the same role (copy, paste, …), so swap them.
const MOD_SWAP = { MetaLeft: 'ControlLeft', MetaRight: 'ControlRight', ControlLeft: 'MetaLeft', ControlRight: 'MetaRight' };
const BTN = { 0: 0, 1: 1, 2: 2 };
let tabSeq = 0;

function norm(video, e) {
  // Position inside the letterboxed video content, normalized to 0..1.
  const r = video.getBoundingClientRect();
  const src = video.__native || video;
  const vw = src.videoWidth || r.width;
  const vh = src.videoHeight || r.height;
  const scale = Math.min(r.width / vw, r.height / vh);
  const w = vw * scale;
  const h = vh * scale;
  return { x: (e.clientX - (r.left + (r.width - w) / 2)) / w, y: (e.clientY - (r.top + (r.height - h) / 2)) / h };
}

class ViewerTab {
  constructor(peerId, label) {
    this.key = ++tabSeq;
    this.peerId = peerId;
    this.session = null;
    this.hs = null; // ID handshake in progress
    this.remotePerm = { control: false, clipboard: false };
    this.keyMap = {};
    this.statsTimer = null;
    this.closing = false;

    this.root = $('tpl-session').content.firstElementChild.cloneNode(true);
    this.r = (name) => this.root.querySelector(`[data-r="${name}"]`);
    $('app').appendChild(this.root);
    this.video = this.r('video');
    this.audio = this.r('audio');
    this.statsEl = this.r('stats');
    this.micBtn = this.r('mic');

    this.btn = document.createElement('button');
    this.btn.className = 'apptab';
    this.btn.innerHTML = '<span class="tab-state"></span><svg class="ic"><use href="#i-monitor"/></svg><span class="tab-label"></span><span class="tab-id"></span>'
      + '<span class="tab-paused hidden" data-i18n-title="tt.paused"><svg class="ic"><use href="#i-mic"/></svg></span>'
      + '<span class="tab-badge hidden"></span>'
      + '<span class="tab-x" data-i18n-title="tt.closeTab"><svg class="ic"><use href="#i-x"/></svg></span>';
    I18N.apply(this.btn);
    this.btn.onclick = (e) => { if (e.target.closest('.tab-x')) this.close(); else activateTab(this); };
    document.querySelector('.apptabs').appendChild(this.btn);

    this.chat = new ChatBox({
      panel: this.r('chat'), log: this.r('chat-log'), form: this.r('chat-form'), input: this.r('chat-input'), close: this.r('chat-close'),
      toggles: [this.r('chat-btn')],
      getSession: () => this.session,
      title: () => this.label,
      onUnread: (n) => {
        const b = this.btn.querySelector('.tab-badge');
        b.textContent = n > 9 ? '9+' : String(n);
        b.classList.toggle('hidden', n === 0);
      },
    });

    this.r('cancel').onclick = () => this.close();
    this.r('stop').onclick = () => this.close();
    this.r('fullscreen').onclick = () => api.setFullscreen(!fsMode);
    this.r('send-file').onclick = () => this.session && pickFiles(this.session, this.label);
    this.micBtn.onclick = () => toggleMic(this);
    this.r('quality').onchange = (e) => this.session && this.session.send({ t: 'quality', mode: e.target.value });
    this.bindInput();
    this.setLabel(label);
    this.updateFsButton();
    // WebKit (Tauri on macOS) only lets media elements play later if play() was first
    // called inside a user gesture. Tabs are created in the Connect click: unlock now.
    unlockMedia(this.video, this.audio);
    tabs.set(this.key, this);
  }

  setLabel(label) {
    this.label = label;
    this.btn.querySelector('.tab-label').textContent = label;
    // Two computers can share a name; the address tail keeps tabs apart.
    this.btn.querySelector('.tab-id').textContent = label === RV.formatId(this.peerId) ? '' : `· ${this.peerId.slice(-3)}`;
    this.btn.title = `${label} · ${RV.formatId(this.peerId)}`;
    this.r('title').textContent = label;
    this.r('conn-title').textContent = label;
    this.r('chat-with').textContent = `· ${label}`;
  }

  setState(state) {
    this.btn.querySelector('.tab-state').className = `tab-state ${state || ''}`;
  }

  status(text, error = false) {
    const el = this.r('status');
    el.textContent = text;
    el.classList.toggle('err', error);
    if (error) {
      this.r('spinner').classList.add('stopped');
      this.r('cancel').textContent = t('btn.close');
      this.setState('err');
    }
  }

  showError(text) {
    this.r('connecting').classList.remove('hidden');
    this.status(text, true);
  }

  // Background tabs: microphone track disabled and remote audio muted, so a voice
  // conversation only ever happens with the session you are looking at.
  applyIsolation(visible) {
    this.audio.muted = !visible;
    const s = this.session;
    const mic = s && s.mic;
    if (mic && mic.enabled !== visible) {
      mic.enabled = visible;
      s.send({ t: 'mic', on: visible });
    }
    this.btn.querySelector('.tab-paused').classList.toggle('hidden', !mic || visible);
    if (!visible) this.root.classList.remove('tb-show');
  }

  // OS + control/view-only label; data-i18n keeps it right after a language switch.
  refreshPermTag() {
    this.r('perm').textContent = `${this.remoteOs ? this.remoteOs + ' · ' : ''}${t(this.remotePerm.control ? 'perm.active' : 'perm.view')}`;
    const hint = this.r('capture-hint');
    hint.dataset.i18n = this.remotePerm.control ? 'sess.click' : 'sess.viewOnly';
    hint.textContent = t(hint.dataset.i18n);
  }

  updateFsButton() {
    const b = this.r('fullscreen');
    b.innerHTML = `<svg class="ic"><use href="#i-${fsMode ? 'shrink' : 'expand'}"/></svg>`;
    b.title = t(fsMode ? 'tt.fsExit' : 'tt.fs');
  }

  // The host encodes no more pixels than this stage shows (device pixels).
  sendView(force) {
    const st = this.r('stage');
    const dpr = window.devicePixelRatio || 1;
    const w = Math.round(st.clientWidth * dpr);
    const h = Math.round(st.clientHeight * dpr);
    if (!w || !h || !this.session || !this.session.connected) return;
    if (!force && this.lastView && Math.abs(this.lastView.w - w) < 16 && Math.abs(this.lastView.h - h) < 16) return;
    this.lastView = { w, h };
    this.session.send({ t: 'view', w, h });
  }

  // ---- input: only the active tab's video forwards mouse events ----
  bindInput() {
    const v = this.video;
    const stage = this.r('stage');
    let viewTimer = null;
    new ResizeObserver(() => {
      clearTimeout(viewTimer);
      viewTimer = setTimeout(() => this.sendView(), 300);
    }).observe(stage);
    let pendingMove = null;
    let edgeTimer = null;
    const mine = () => capturing && captureTab === this && this.session;
    v.addEventListener('mousedown', (e) => {
      e.preventDefault();
      if (v.paused) v.play().catch(() => {}); // last resort if autoplay was still blocked
      if (!mine()) { if (activeTab === this) setCapture(true); if (!mine()) return; }
      this.session.send({ t: 'down', b: BTN[e.button] ?? 0, c: e.detail || 1, ...norm(v, e) });
    });
    v.addEventListener('mouseup', (e) => {
      if (mine()) this.session.send({ t: 'up', b: BTN[e.button] ?? 0, c: e.detail || 1, ...norm(v, e) });
    });
    v.addEventListener('mousemove', (e) => {
      // Full screen: resting at the very top edge reveals the toolbar (nothing is
      // overlaid there, so the remote's own top rows stay clickable).
      if (fsMode) {
        if (e.clientY <= 3) { if (!edgeTimer) edgeTimer = setTimeout(() => this.root.classList.add('tb-show'), 250); }
        else { clearTimeout(edgeTimer); edgeTimer = null; }
      }
      if (!mine()) return;
      const first = pendingMove === null;
      pendingMove = norm(v, e);
      if (first) requestAnimationFrame(() => {
        if (mine() && pendingMove) this.session.send({ t: 'move', ...pendingMove });
        pendingMove = null;
      });
    });
    v.addEventListener('wheel', (e) => {
      if (!mine()) return;
      e.preventDefault();
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
      this.session.send({ t: 'scroll', dx: e.deltaX * k, dy: e.deltaY * k });
    }, { passive: false });
    v.addEventListener('contextmenu', (e) => e.preventDefault());

    const toolbar = this.r('toolbar');
    toolbar.addEventListener('mousedown', () => setCapture(false));
    let hideTimer = null;
    toolbar.addEventListener('mouseenter', () => clearTimeout(hideTimer));
    toolbar.addEventListener('mouseleave', () => {
      hideTimer = setTimeout(() => { if (!toolbar.contains(document.activeElement)) this.root.classList.remove('tb-show'); }, 700);
    });

    stage.addEventListener('dragover', (e) => { e.preventDefault(); this.r('drop-hint').classList.remove('hidden'); });
    stage.addEventListener('dragleave', () => this.r('drop-hint').classList.add('hidden'));
    stage.addEventListener('drop', (e) => {
      e.preventDefault();
      this.r('drop-hint').classList.add('hidden');
      if (this.session && this.session.connected && e.dataTransfer.files.length) sendFiles(this.session, [...e.dataTransfer.files], this.label);
    });
  }

  // ---- connect by ID ----
  async startHandshake() {
    const id = this.peerId;
    const { raw, key } = await RV.newSessionKey();
    const topic = RV.randomTopic();
    const hs = this.hs = { topic, key, hostPeer: null, timer: null };
    api.rvSubscribe(topic);
    this.status(t('req.sending'));
    const r = await RV.send(await RV.topicForId(id), await RV.keyForId(id), { t: 'req', reply: topic, k: raw, name: appInfo.name, os: api.platform, from: RV.myId(), caps: api.mdp && !this.legacy ? ['mdp1'] : [] });
    if (this.hs !== hs) return;
    if (!r.ok) {
      this.endHandshake();
      return this.status(t('req.sendFailed'), true);
    }
    this.status(t('req.waiting'));
    hs.timer = setTimeout(() => {
      if (this.hs !== hs || hs.hostPeer) return;
      this.endHandshake();
      this.status(t('req.timeout'), true);
    }, 75000);
  }

  endHandshake() {
    if (!this.hs) return;
    api.rvUnsubscribe(this.hs.topic);
    clearTimeout(this.hs.timer);
    this.hs = null;
  }

  async onReply(m) {
    const hs = this.hs;
    if (hs.hostPeer && m.pubkey !== hs.hostPeer) return;
    const msg = await RV.open(hs.key, m.content);
    if (!msg || this.hs !== hs) return;
    if (msg.t === 'reject') {
      this.endHandshake();
      this.status(t('req.rejected') + (msg.reason ? ` (${String(msg.reason).slice(0, 120)})` : ''), true);
    } else if (msg.t === 'busy') {
      this.endHandshake();
      this.status(t('req.busy'), true);
    } else if (msg.t === 'offer' && !hs.hostPeer && HEX64.test(msg.reply) && typeof msg.code === 'string') {
      hs.hostPeer = m.pubkey; // pin the host that accepted
      hs.hostName = String(msg.name || '').slice(0, 60);
      hs.hostOs = msg.os;
      clearTimeout(hs.timer);
      if (hs.hostName) this.setLabel(hs.hostName);
      this.status(t('req.accepted'));
      await ensureTurn();
      if (this.hs !== hs || this.closing) return;
      const native = api.mdp && await RTC.isNativeCode(msg.code);
      if (this.hs !== hs || this.closing) return;
      const s = this.session = native ? new RTC.NativeSession('viewer') : new Session('viewer');
      this.wireSession(s);
      try {
        const { code, sas } = await s.answerInvite(msg.code);
        warnIfNoStun(code, s);
        this.r('sas-big').textContent = sas;
        this.r('sas').textContent = sas;
        this.r('sas-row').classList.remove('hidden');
        // Arm the timeout before sending: the connection may come up before every
        // relay has acknowledged the answer.
        hs.timer = setTimeout(() => {
          if (!s.connected && !s.closed) {
            s.close(t(window.RTC.activeTurn() ? 'conn.noRouteTurn' : 'conn.noRoute'));
          }
        }, 45000);
        const r = await RV.send(msg.reply, hs.key, { t: 'answer', code });
        if (!r.ok && !s.connected) throw new Error(t('err.answerNotSent'));
      } catch (e) {
        if (!s.connected) s.close(t('conn.failed', { e: e.message }));
      }
    }
  }

  // ---- the WebRTC session of this tab ----
  wireSession(s) {
    bindFileEvents(s, () => this.label);
    s.addEventListener('track', ({ detail: stream }) => {
      if (stream.attach) { stream.attach(this.video); return; } // Linux: decoded natively into a canvas
      this.video.srcObject = stream;
      this.video.play().catch(() => {});
      if (api.videoMirror) mirrorVideo(this.video);
    });
    s.addEventListener('audio', ({ detail: stream }) => {
      this.audio.srcObject = stream;
      this.audio.play().catch(() => {});
    });
    s.addEventListener('open', () => {
      this.r('connecting').classList.add('hidden');
      this.setState('live');
      this.r('quality').value = 'auto';
      this.sendView(true);
      if (this.hs) {
        presence.set(this.peerId, { ts: Date.now(), busy: false });
        rememberRecent({ id: this.peerId, name: this.hs.hostName, os: this.hs.hostOs });
        setTimeout(() => updateRecentThumb(this), 2500);
        this.endHandshake();
      }
      this.applyIsolation(this === activeTab);
      startStats(this);
    });
    this.onStats = (st) => {
      // Relay traffic is metered on free TURN plans: start in the frugal preset once.
      if (st.relay && !this.relayNoticed) {
        this.relayNoticed = true;
        if (['auto', 'balanced'].includes(this.r('quality').value)) {
          this.r('quality').value = 'speed';
          s.send({ t: 'quality', mode: 'speed' });
        }
        toast(t('relay.speed', { who: this.label }), { timeout: 8000 });
      }
    };
    s.addEventListener('message', ({ detail: m }) => {
      if (m.t === 'chat' && typeof m.text === 'string') {
        this.chat.receive(m.text, Number(m.ts) || Date.now(), this === activeTab);
      } else if (m.t === 'mic') {
        this.r('remote-mic').classList.toggle('hidden', !m.on);
      } else if (m.t === 'perm') {
        this.remotePerm = { control: !!m.control, clipboard: !!m.clipboard };
        this.keyMap = m.os && (m.os === 'darwin') !== isMac ? MOD_SWAP : {};
        this.remoteOs = OS_NAME[m.os] || '';
        this.refreshPermTag();
        if (!this.remotePerm.control && captureTab === this) setCapture(false);
        updateClipboardWatch();
      } else if (m.t === 'clip') {
        // Only the visible session may write to this computer's clipboard.
        if (this.remotePerm.clipboard && this === activeTab && typeof m.text === 'string') api.setClipboard(m.text);
      } else if (m.t === 'bye') {
        s.close(t('peer.ended'));
      }
    });
    s.addEventListener('closed', () => {
      if (captureTab === this) setCapture(false);
      rememberReport(s);
      if (s.connected) updateRecentThumb(this);
      this.endHandshake();
      clearInterval(this.statsTimer);
      this.remotePerm = { control: false, clipboard: false };
      updateClipboardWatch();
      if (this.closing || s.silent) return;
      if (s.native && !s.connected && !this.legacy) {
        // No direct path for the native protocol: retry over WebRTC, which can relay (TURN).
        this.legacy = true;
        this.session = null;
        this.r('connecting').classList.remove('hidden');
        this.status(t('conn.fallback'));
        this.startHandshake();
        return;
      }
      if (s.connected) {
        toast(`${this.label}: ${s.reason || t('conn.lost')}`);
        this.close();
      } else {
        this.showError(s.reason || t('conn.notMade'));
      }
    });
  }

  // Ends everything that belongs to this tab and removes it.
  async close() {
    if (this.closing) return;
    this.closing = true;
    const hs = this.hs;
    this.endHandshake();
    if (hs && !hs.hostPeer) RV.send(await RV.topicForId(this.peerId), await RV.keyForId(this.peerId), { t: 'cancel', reply: hs.topic });
    if (captureTab === this) setCapture(false);
    const s = this.session;
    if (s && !s.closed) s.close();
    clearInterval(this.statsTimer);
    this.video.srcObject = null;
    this.audio.srcObject = null;
    this.root.remove();
    this.btn.remove();
    tabs.delete(this.key);
    updateClipboardWatch();
    if (activeTab === this) {
      const rest = [...tabs.values()];
      activateTab(rest.length ? rest[rest.length - 1] : null);
    }
  }
}

// ---- keyboard: only to the session that holds the capture ----
let capturing = false;
let captureTab = null;
const keysDown = new Set();

function setCapture(on) {
  const t = activeTab;
  on = !!(on && t && t.remotePerm.control && t.session && t.session.connected);
  if (on === capturing && (!on || captureTab === t)) return;
  if (capturing && captureTab && captureTab.session) captureTab.session.send({ t: 'release' });
  keysDown.clear();
  capturing = on;
  captureTab = on ? t : null;
  for (const x of tabs.values()) x.r('stage').classList.toggle('capturing', on && x === t);
  api.captureMenu(on);
}

const isFsShortcut = (e) => e.ctrlKey && e.altKey && e.shiftKey && e.code === 'KeyF';

window.addEventListener('keydown', (e) => {
  // Ctrl+Alt+Shift+F toggles full screen and is never sent to the remote computer.
  if (isFsShortcut(e)) {
    e.preventDefault();
    if (activeTab) api.setFullscreen(!fsMode);
    return;
  }
  if (!capturing) return;
  e.preventDefault();
  keysDown.add(e.code);
  captureTab.session.send({ t: 'key', code: captureTab.keyMap[e.code] || e.code, d: true });
});
window.addEventListener('keyup', (e) => {
  if (isFsShortcut(e)) return e.preventDefault();
  if (!capturing) return;
  e.preventDefault();
  const t = captureTab;
  keysDown.delete(e.code);
  t.session.send({ t: 'key', code: t.keyMap[e.code] || e.code, d: false });
  // macOS does not deliver keyup for other keys while Cmd is held; release them now.
  if (e.key === 'Meta') {
    for (const code of keysDown) t.session.send({ t: 'key', code: t.keyMap[code] || code, d: false });
    keysDown.clear();
  }
});
window.addEventListener('blur', () => setCapture(false));

// ---- clipboard: host sessions that allow it, and the visible viewer tab only ----
function updateClipboardWatch() {
  api.watchClipboard([...tabs.values()].some((t) => t.remotePerm.clipboard && t.session && t.session.connected));
}
api.onClipboard((text) => {
  for (const h of hostLive()) if (h.allowClipboard()) h.session.send({ t: 'clip', text });
  const t = activeTab;
  if (t && t.remotePerm.clipboard && t.session && t.session.connected) t.session.send({ t: 'clip', text });
});

// ---- full screen (AnyDesk-style) ----
let fsMode = false;
api.onFullscreen((on) => {
  fsMode = on;
  document.body.classList.toggle('fs-mode', on);
  for (const t of tabs.values()) {
    t.updateFsButton();
    t.root.classList.remove('tb-show');
  }
  if (on) toast(t('fs.hint'), { timeout: 4000 });
});

// ---------------------------------------------------------- connect by ID ---
let hostTopic = null; // requests for this computer's ID arrive here
let pendingReq = null; // request currently shown in the Accept / Reject dialog
const reqQueue = []; // further requests wait here and are shown one after another

api.appInfo().then((i) => {
  appInfo = i;
  $('app-version').textContent = `ManyDesk ${i.version || ''}`;
  I18N.init(i.languages); // the OS preference list (macOS, Windows, Linux)
});

// ---- language ----
function fillLangSelect() {
  const sel = $('lang-select');
  sel.innerHTML = '';
  const add = (value, label) => {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = label;
    sel.appendChild(o);
  };
  add('system', `${t('lang.system')} · ${I18N.LANGS[I18N.system]}`);
  for (const [code, name] of Object.entries(I18N.LANGS)) add(code, name);
  sel.value = I18N.setting();
}
$('lang-select').onchange = (e) => I18N.setLanguage(e.target.value);

// Pointer labels on the host's screen use the host's language.
function overlayActs() {
  return { down: t('ov.click'), scroll: t('ov.scroll'), key: t('ov.type') };
}

// Static markup is re-translated by I18N; refresh the texts built in code.
window.addEventListener('languagechange-app', () => {
  fillLangSelect();
  if (lastRvStatus) showRelayStatus(lastRvStatus);
  renderRecent();
  for (const tb of tabs.values()) {
    tb.updateFsButton();
    tb.refreshPermTag();
    setMicButton(tb.micBtn, !!(tb.session && tb.session.mic));
  }
  for (const h of hostSessions.values()) setMicButton(h.micBtn, !!(h.session && h.session.mic));
});

async function listenOnMyId() {
  if (hostTopic) api.rvUnsubscribe(hostTopic);
  const id = RV.myId();
  $('my-id').textContent = RV.formatId(id);
  hostTopic = await RV.topicForId(id);
  api.rvSubscribe(hostTopic, 0); // live requests only — never replay an old one
}

let lastRvStatus = null;
function showRelayStatus(st) {
  lastRvStatus = st;
  const ok = st.connected > 0;
  const el = $('rv-host-status');
  el.innerHTML = `<span class="dot ${ok ? 'ok' : 'bad'}"></span> `;
  el.append(ok ? t('rv.ready', { n: st.connected, total: st.total }) : t('rv.down'));
}
api.onRvStatus(showRelayStatus);
api.rvStatus().then(showRelayStatus);

$('my-id-copy').onclick = async () => {
  await navigator.clipboard.writeText(RV.myId());
  toast(t('id.copied'));
};
$('my-id-new').onclick = async () => {
  RV.newId();
  await listenOnMyId();
  toast(t('id.new'));
};

api.onRv(async (m) => {
  if (m.topic === hostTopic) return onHostRequest(m);
  if (m.topic === presenceTopic) return onPong(m);
  for (const h of hostSessions.values()) if (h.hs && h.hs.topic === m.topic) return h.onAnswer(m);
  for (const t of tabs.values()) if (t.hs && t.hs.topic === m.topic) return t.onReply(m);
});

// ---- host side: requests are queued and decided one at a time ----
let modalTimer = null;

function showRequestModal(req) {
  $('rv-modal-name').textContent = req.name;
  $('rv-modal-os').textContent = [req.os, req.from && RV.formatId(req.from)].filter(Boolean).join(' · ');
  $('rv-modal-control').checked = $('opt-control').checked;
  $('rv-modal-clipboard').checked = $('opt-clipboard').checked;
  $('rv-modal').classList.remove('hidden');
  let left = 60;
  const tick = () => {
    const live = hostLive().length;
    $('rv-modal-timer').textContent = [t('acc.left', { s: left }), live && t('acc.live', { n: live }), reqQueue.length && t('acc.queue', { n: reqQueue.length })].filter(Boolean).join(' · ');
  };
  tick();
  clearInterval(modalTimer);
  modalTimer = setInterval(() => { if (--left <= 0) $('rv-reject').click(); else tick(); }, 1000);
  if (!document.hasFocus()) new Notification(`ManyDesk — ${t('notif.request')}`, { body: t('notif.requestBody', { who: req.name }) });
}

function hideRequestModal() {
  clearInterval(modalTimer);
  $('rv-modal').classList.add('hidden');
}

function showNextRequest() {
  if (pendingReq || !reqQueue.length) return;
  pendingReq = reqQueue.shift();
  showRequestModal(pendingReq);
}

async function onHostRequest(m) {
  const msg = await RV.open(await RV.keyForId(RV.myId()), m.content);
  if (!msg) return;
  if (msg.t === 'ping') return answerPing(msg);
  if (msg.t === 'cancel') {
    const same = (r) => r.reply === msg.reply && r.peer === m.pubkey;
    if (pendingReq && same(pendingReq)) {
      pendingReq = null;
      hideRequestModal();
      toast(t('req.cancelled'));
      showNextRequest();
    } else {
      const i = reqQueue.findIndex(same);
      if (i >= 0) reqQueue.splice(i, 1);
    }
    return;
  }
  if (msg.t !== 'req' || !HEX64.test(msg.reply) || typeof msg.k !== 'string') return;
  if ((pendingReq && pendingReq.reply === msg.reply) || reqQueue.some((r) => r.reply === msg.reply)) return;
  const key = await RV.importSessionKey(msg.k).catch(() => null);
  if (!key) return;
  if (hostSessions.size + reqQueue.length + (pendingReq ? 1 : 0) >= MAX_HOST) {
    RV.send(msg.reply, key, { t: 'busy' });
    return;
  }
  const from = /^\d{9}$/.test(String(msg.from)) ? String(msg.from) : '';
  const native = !!api.mdp && Array.isArray(msg.caps) && msg.caps.includes('mdp1');
  const req = { reply: msg.reply, key, peer: m.pubkey, name: String(msg.name || t('host.unknown')).slice(0, 60), os: OS_NAME[msg.os] || '', from, native };
  const fb = nativeFallback.get(m.pubkey);
  if (fb && !native && fb.until > Date.now()) {
    nativeFallback.delete(m.pubkey);
    new HostCtx(req, fb.perms).start(req);
    return;
  }
  reqQueue.push(req);
  showNextRequest();
}

$('rv-reject').onclick = () => {
  const req = pendingReq;
  pendingReq = null;
  hideRequestModal();
  if (req) RV.send(req.reply, req.key, { t: 'reject' });
  showNextRequest();
};

$('rv-accept').onclick = () => {
  const req = pendingReq;
  if (!req) return;
  pendingReq = null;
  hideRequestModal();
  const ctx = new HostCtx(req, { control: $('rv-modal-control').checked, clipboard: $('rv-modal-clipboard').checked });
  ctx.start(req);
  showNextRequest();
};

// ---- viewer side: every Connect opens (or focuses) a tab of its own ----
$('rv-peer-id').addEventListener('input', (e) => {
  const digits = RV.normalizeId(e.target.value).slice(0, 9);
  e.target.value = RV.formatId(digits);
});

$('rv-connect-form').onsubmit = (e) => {
  e.preventDefault();
  const id = RV.normalizeId($('rv-peer-id').value);
  if (!/^\d{9}$/.test(id)) return toast(t('addr.invalid'), { error: true });
  if (id === RV.myId()) return toast(t('addr.self'), { error: true });
  const existing = [...tabs.values()].find((t) => t.peerId === id);
  if (existing) {
    // Already connected or connecting to this computer: just show that tab.
    if (existing.hs || (existing.session && existing.session.connected)) {
      $('rv-peer-id').value = '';
      return activateTab(existing);
    }
    existing.close(); // a failed attempt: replace it with a fresh one
  }
  const known = loadRecent().find((r) => r.id === id);
  const tab = new ViewerTab(id, known && known.name ? known.name : RV.formatId(id));
  activateTab(tab);
  tab.startHandshake();
  $('rv-peer-id').value = '';
};

// -------------------------------------------------------- recent sessions ---
function loadRecent() {
  try { return JSON.parse(localStorage.getItem('recent') || '[]'); } catch { return []; }
}

function saveRecent(list) {
  try { localStorage.setItem('recent', JSON.stringify(list.slice(0, 12))); } catch {}
}

function rememberRecent(entry) {
  const list = loadRecent();
  const old = list.find((r) => r.id === entry.id) || {};
  saveRecent([{ ...old, ...entry, name: entry.name || old.name || '', ts: Date.now() }, ...list.filter((r) => r.id !== entry.id)]);
  renderRecent();
}

function updateRecentThumb(tab) {
  const video = tab.video.__native || tab.video;
  if (!video.videoWidth) return;
  const c = document.createElement('canvas');
  c.width = 320;
  c.height = Math.round((320 * video.videoHeight) / video.videoWidth);
  c.getContext('2d').drawImage(video.canvas || video, 0, 0, c.width, c.height);
  const list = loadRecent();
  const r = list.find((x) => x.id === tab.peerId);
  if (!r) return;
  r.thumb = c.toDataURL('image/jpeg', 0.6);
  saveRecent(list);
  renderRecent();
}

function timeAgo(ts) {
  const m = Math.round((Date.now() - ts) / 60000);
  if (m < 1) return t('time.now');
  if (m < 60) return t('time.min', { n: m });
  if (m < 1440) return t('time.hour', { n: Math.round(m / 60) });
  return new Date(ts).toLocaleDateString(I18N.lang);
}

function renderRecent() {
  const grid = $('recent-grid');
  grid.innerHTML = '';
  const list = loadRecent();
  if (!list.length) {
    grid.innerHTML = '<div class="empty"><svg class="ic"><use href="#i-monitor"/></svg><b data-i18n="recent.empty"></b><span data-i18n="recent.emptyHint"></span></div>';
    I18N.apply(grid);
    return;
  }
  for (const r of list) {
    const tile = document.createElement('button');
    tile.className = 'tile';
    tile.title = `${t('tt.connect')} · ${RV.formatId(r.id)}`;
    const thumb = document.createElement('div');
    thumb.className = 'tile-thumb';
    if (r.thumb) thumb.style.backgroundImage = `url("${r.thumb}")`;
    else thumb.innerHTML = '<svg class="ic"><use href="#i-monitor"/></svg>';
    const meta = document.createElement('div');
    meta.className = 'tile-meta';
    meta.innerHTML = '<div class="grow"><div class="tile-name"></div><div class="tile-id"></div></div><div class="tile-time"></div>';
    const st = presenceOf(r.id);
    const badge = document.createElement('span');
    badge.className = `presence ${st}`;
    badge.textContent = t(PRESENCE_LABEL[st]);
    thumb.appendChild(badge);
    tile.classList.toggle('offline', st === 'offline');
    meta.querySelector('.tile-name').textContent = r.name || RV.formatId(r.id);
    meta.querySelector('.tile-id').textContent = RV.formatId(r.id);
    meta.querySelector('.tile-time').textContent = timeAgo(r.ts);
    const del = document.createElement('span');
    del.className = 'tile-remove';
    del.title = t('tt.remove');
    del.innerHTML = '<svg class="ic"><use href="#i-x"/></svg>';
    del.onclick = (e) => {
      e.stopPropagation();
      saveRecent(loadRecent().filter((x) => x.id !== r.id));
      renderRecent();
    };
    tile.onclick = () => {
      $('rv-peer-id').value = RV.formatId(r.id);
      $('rv-connect-form').requestSubmit();
    };
    tile.append(thumb, meta, del);
    grid.appendChild(tile);
  }
}

// ------------------------------------------------------------- presence ---
// "Is that computer online?" without a server: a small encrypted ping goes to each
// recent address over the same relays/brokers as connection requests; a running
// ManyDesk there answers with a pong (unless its owner switched that off).
const presenceCfg = { every: 45000, fresh: 100000, settle: 7000 };
const PRESENCE_LABEL = { online: 'pr.online', busy: 'pr.busy', offline: 'pr.offline', checking: 'pr.checking' };
const presence = new Map(); // address -> { ts, busy }
let presenceTopic = null;
let presenceKey = null;
let presenceKeyRaw = '';
let presenceRound = 0;

function presenceOf(id) {
  const p = presence.get(id);
  if (p && Date.now() - p.ts < presenceCfg.fresh) return p.busy ? 'busy' : 'online';
  return Date.now() - presenceRound < presenceCfg.settle ? 'checking' : 'offline';
}

async function pollPresence() {
  const list = loadRecent();
  if (!list.length || !presenceTopic) return;
  presenceRound = Date.now();
  renderRecent();
  for (const r of list) {
    RV.send(await RV.topicForId(r.id), await RV.keyForId(r.id), { t: 'ping', reply: presenceTopic, k: presenceKeyRaw });
  }
  setTimeout(renderRecent, presenceCfg.settle + 200);
}

async function onPong(m) {
  const msg = await RV.open(presenceKey, m.content);
  if (!msg || msg.t !== 'pong' || !/^\d{9}$/.test(String(msg.id))) return;
  const id = String(msg.id);
  const recent = loadRecent();
  const r = recent.find((x) => x.id === id);
  if (!r) return;
  presence.set(id, { ts: Date.now(), busy: !!msg.busy });
  // Keep the shown name current if that computer was renamed.
  if (typeof msg.name === 'string' && msg.name && msg.name !== r.name) {
    r.name = msg.name.slice(0, 60);
    saveRecent(recent);
  }
  renderRecent();
}

async function initPresence() {
  const k = await RV.newSessionKey();
  presenceKey = k.key;
  presenceKeyRaw = k.raw;
  presenceTopic = RV.randomTopic();
  api.rvSubscribe(presenceTopic);
  setTimeout(pollPresence, 1500); // give the relays a moment to connect
  setInterval(pollPresence, presenceCfg.every);
  // Statuses age out between rounds; refresh the badges.
  setInterval(() => { if (!activeTab) renderRecent(); }, 10000);
}

// ---- answering (this computer as host) ----
function presenceEnabled() {
  try { return localStorage.getItem('presence') !== 'off'; } catch { return true; }
}
$('opt-presence').checked = presenceEnabled();
$('opt-presence').onchange = (e) => {
  try { localStorage.setItem('presence', e.target.checked ? 'on' : 'off'); } catch {}
  toast(t(e.target.checked ? 'pr.shown' : 'pr.hidden'));
};

// Answer at most once per asker every 3 s and 60 times a minute in total.
const pongLog = new Map();
let pongWindow = { start: 0, count: 0 };
async function answerPing(msg) {
  if (!presenceEnabled() || !HEX64.test(msg.reply) || typeof msg.k !== 'string') return;
  const now = Date.now();
  if (now - (pongLog.get(msg.reply) || 0) < 3000) return;
  if (now - pongWindow.start > 60000) pongWindow = { start: now, count: 0 };
  if (++pongWindow.count > 60) return;
  pongLog.set(msg.reply, now);
  if (pongLog.size > 500) pongLog.clear();
  const key = await RV.importSessionKey(msg.k).catch(() => null);
  if (!key) return;
  RV.send(msg.reply, key, { t: 'pong', id: RV.myId(), name: appInfo.name, os: api.platform, busy: hostSessions.size >= MAX_HOST });
}

// ------------------------------------------------------------ diagnostics ---
let lastReport = '';

function rememberReport(s) {
  if (s.finalReport) s.finalReport.then((r) => { lastReport = r; });
}

function natLine(n) {
  return [
    `NAT: ${n.type}`,
    n.verdict,
    `Public IP: ${n.publicIps.join(', ') || '—'}`,
    `Local addresses: ${n.hostCount}, port preserved: ${n.portPreserved ? 'yes' : 'no'}`,
    'Mappings (local → public, server):',
    ...n.mappings.map((m) => '  ' + m),
    'STUN servers:',
    ...n.servers.map((m) => '  ' + m),
    n.turn,
  ].join('\n');
}

$('diag-run').onclick = async () => {
  const out = $('diag-out');
  out.classList.remove('hidden');
  out.textContent = t('diag.running');
  $('diag-run').disabled = true;
  try {
    const [n, rv] = await Promise.all([window.RTC.detectNat(), api.rvStatus()]);
    const rvLines = [
      `Rendezvous channels: ${rv.connected}/${rv.total} open`,
      `System clock: ${Math.abs(rv.clockOffsetS) < 3 ? 'ok' : `${rv.clockOffsetS > 0 ? rv.clockOffsetS + ' s behind' : -rv.clockOffsetS + ' s ahead'} (network time used)`}`,
      ...rv.relays.map((r) => `  ${r.open ? '✓' : '✗'} ${r.kind === 'mqtt' ? 'MQTT ' : 'Nostr'} ${r.url}`),
    ];
    out.textContent = `${api.platform} · ${new Date().toLocaleString('az')}\n${natLine(n)}\n${rvLines.join('\n')}`;
    out.className = 'diag ' + (['cone', 'open', 'multi-ip'].includes(n.type) ? 'ok' : 'bad');
  } catch (e) {
    out.textContent = t('diag.error', { e: e.message });
  } finally {
    $('diag-run').disabled = false;
  }
};

$('diag-copy').onclick = async () => {
  const s = (activeTab && activeTab.session) || (hostLive()[0] && hostLive()[0].session);
  let text = s ? await s.report() : lastReport;
  if (!text) return toast(t('diag.noLog'), { error: true });
  const diag = $('diag-out').textContent;
  if (diag && !$('diag-out').classList.contains('hidden')) text = `${diag}\n\n${text}`;
  await navigator.clipboard.writeText(text);
  toast(t('diag.copied'));
};

// ------------------------------------------------------------------- TURN ---
function turnProvider() {
  return document.querySelector('input[name=turn-provider]:checked').value;
}

function showTurnProvider() {
  const cf = turnProvider() === 'cloudflare';
  $('turn-cf').classList.toggle('hidden', !cf);
  $('turn-manual').classList.toggle('hidden', cf);
}
document.querySelectorAll('input[name=turn-provider]').forEach((r) => { r.onchange = showTurnProvider; });

function readTurnForm() {
  return {
    enabled: $('turn-enabled').checked,
    provider: turnProvider(),
    urls: $('turn-urls').value.split('\n'),
    username: $('turn-user').value.trim(),
    credential: $('turn-pass').value,
    cfKeyId: $('turn-cf-key').value.trim(),
    cfToken: $('turn-cf-token').value.trim(),
  };
}

function loadTurn() {
  const t = window.RTC.turnConfig();
  $('turn-enabled').checked = t.enabled;
  document.querySelector(`input[name=turn-provider][value=${t.provider === 'manual' && !t.urls.length && !t.enabled ? 'cloudflare' : t.provider}]`).checked = true;
  $('turn-urls').value = t.urls.join('\n');
  $('turn-user').value = t.username;
  $('turn-pass').value = t.credential;
  $('turn-cf-key').value = t.cfKeyId;
  $('turn-cf-token').value = t.cfToken;
  showTurnProvider();
}

$('turn-save').onclick = async () => {
  const form = readTurnForm();
  const saved = window.RTC.saveTurnConfig(form);
  loadTurn();
  if (form.enabled && !saved.enabled) {
    return toast(t(form.provider === 'cloudflare' ? 'turn.needCf' : 'turn.needUrl'), { error: true });
  }
  if (saved.enabled && saved.provider === 'cloudflare') {
    const r = await window.RTC.prepareTurn(true);
    if (!r.ok) return toast(t('turn.savedErr', { e: r.error }), { error: true, timeout: 10000 });
  }
  toast(t(saved.enabled ? 'turn.saved' : 'turn.off'));
};

$('turn-test').onclick = async () => {
  const form = readTurnForm();
  const out = $('turn-out');
  out.classList.remove('hidden');
  out.className = 'diag';
  out.textContent = t('diag.running');
  if (form.provider === 'cloudflare') {
    if (!form.cfKeyId || !form.cfToken) { out.className = 'diag bad'; out.textContent = t('turn.needCf'); return; }
    $('turn-test').disabled = true;
    const cf = await api.cloudflareTurn(form.cfKeyId, form.cfToken);
    if (cf.error) {
      $('turn-test').disabled = false;
      out.className = 'diag bad';
      out.textContent = `✗ ${t(cf.error, { status: cf.status })}`;
      return;
    }
    Object.assign(form, { urls: cf.urls, username: cf.username, credential: cf.credential });
    $('turn-test').disabled = false;
  }
  const urls = form.urls.map((u) => u.trim()).filter(Boolean);
  if (!urls.length) { out.className = 'diag bad'; out.textContent = t('turn.needUrl'); return; }
  $('turn-test').disabled = true;
  // Test each address separately so a blocked port/transport is easy to spot.
  const results = await Promise.all(urls.map(async (u) => [u, await window.RTC.testTurn({ ...form, urls: [u] })]));
  $('turn-test').disabled = false;
  const ok = results.some(([, r]) => r.ok);
  out.className = 'diag ' + (ok ? 'ok' : 'bad');
  out.textContent = results.map(([u, r]) => (r.ok ? `✓ ${r.ms} ms  relay ${r.address}  ` : `✗ ${r.error}  `) + u).join('\n')
    + `\n\n${t(ok ? 'turn.works' : 'turn.none')}`;
};

// --------------------------------------------------------------- settings ---
function loadStun() { $('stun-list').value = window.RTC.stunServers().join('\n'); }
$('stun-save').onclick = () => {
  const saved = window.RTC.saveStunServers($('stun-list').value.split('\n'));
  $('stun-list').value = saved.join('\n');
  toast(t('stun.saved'));
};
$('stun-reset').onclick = () => { localStorage.removeItem('stun'); loadStun(); toast(t('stun.reset')); };

loadStun();
loadTurn();
fillLangSelect();
listenOnMyId();
renderRecent();
initPresence();
refreshHost();
