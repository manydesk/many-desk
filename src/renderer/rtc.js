// WebRTC session + serverless signaling.
//
// There is no signaling server. The full SDP (with every gathered ICE candidate)
// is compressed into a one-time text code that the two people exchange over any
// channel they already have (chat, e-mail, phone). Only public STUN servers are
// used to discover each side's public address; media and data flow peer-to-peer.

(() => {
// Different providers, countries and UDP ports (all verified to answer): networks that
// block one provider's addresses or the usual 19302/3478 ports still reach some of
// them — UDP 443 (QUIC) and 80 are rarely blocked.
const DEFAULT_STUN = [
  'stun:stun.l.google.com:19302',
  'stun:stun1.l.google.com:3478',
  'stun:stun.cloudflare.com:3478',
  'stun:stun.cloudflare.com:443',
  'stun:stun.nextcloud.com:443',
  'stun:stun.relay.metered.ca:80',
  'stun:global.stun.twilio.com:3478',
  'stun:stun.telnyx.com:3478',
  'stun:stun.voip.blackberry.com:3478',
  'stun:stun.framasoft.org:3478',
  'stun:stun.t-online.de:3478',
  'stun:stun.ipfire.org:3478',
  'stun:stun.zadarma.com:3478',
  'stun:stun.sipnet.ru:3478',
  'stun:stun.miwifi.com:3478',
];
// Defaults shipped by earlier versions; a saved list identical to one of them is upgraded.
const OLD_DEFAULT_STUN = [
  ['stun:stun.l.google.com:19302', 'stun:stun1.l.google.com:19302', 'stun:stun2.l.google.com:19302', 'stun:stun.cloudflare.com:3478'],
  ['stun:stun.l.google.com:19302', 'stun:stun1.l.google.com:19302', 'stun:stun.cloudflare.com:3478', 'stun:stun.cloudflare.com:443',
    'stun:stun.nextcloud.com:443', 'stun:stun.relay.metered.ca:80', 'stun:global.stun.twilio.com:3478'],
].map((l) => l.join());

const CODE_PREFIX = 'MD1-';
const FILE_CHUNK = 16 * 1024; // the interoperable SCTP message size (webrtc-rs on Linux drops larger ones)
const FILE_HIGH_WATER = 4 * 1024 * 1024;
// The two codes are pasted minutes apart, so each side keeps punching NAT holes
// towards the other until the peer starts its own checks.
const PUNCH_INTERVAL_MS = 4000;
// Video codecs already send only what changed between frames (a static screen costs ~0 kbit/s).
// VP9 has a dedicated screen-content mode that compresses text/UI much better than VP8.
// Software fallback order; hardware encoders (VideoToolbox / Media Foundation) that the
// platform reports as power-efficient are moved to the front (see codecOrder).
const CODEC_ORDER = ['video/VP9', 'video/AV1', 'video/H264', 'video/VP8'];
const HW_CANDIDATES = ['video/H265', 'video/H264'];

// Viewer-selectable trade-offs, applied to the host's video sender. The resolution
// always follows the viewer's stage (no more pixels than it can show), times `extra`.
// Bitrate budget = pixels × fps × bpp, clamped to [minBr, maxBr].
const QUALITY = {
  auto: { fps: 30, bpp: 0.08, minBr: 300_000, maxBr: 8_000_000, extra: 1 },
  speed: { fps: 20, bpp: 0.05, minBr: 200_000, maxBr: 1_500_000, extra: 1.5, pref: 'maintain-framerate' },
  balanced: { fps: 30, bpp: 0.08, minBr: 300_000, maxBr: 5_000_000, extra: 1, pref: 'balanced' },
  quality: { fps: 60, bpp: 0.12, minBr: 1_000_000, maxBr: 16_000_000, extra: 1, pref: 'maintain-resolution' },
};
// "auto" steps: s = extra downscale, fps cap, br = bitrate factor. Starts at 1.
const AUTO_LEVELS = [
  { s: 1, fps: 60, br: 1.5 },
  { s: 1, fps: 30, br: 1 },
  { s: 1.33, fps: 30, br: 0.8 },
  { s: 1.6, fps: 24, br: 0.6 },
  { s: 2, fps: 15, br: 0.45 },
];
const AUTO_START = 1;

const VIEWER_WAIT_MS = 15 * 60 * 1000; // viewer: waiting for the host to paste the answer
const HOST_CONNECT_MS = 60 * 1000; // host: answer pasted, viewer is already punching

function stunServers() {
  try {
    const saved = JSON.parse(localStorage.getItem('stun') || 'null');
    if (Array.isArray(saved) && saved.length && !OLD_DEFAULT_STUN.includes(saved.join())) return saved;
  } catch {}
  return DEFAULT_STUN;
}

// Optional TURN relay, used only when a direct path cannot be found (e.g. both sides
// on mobile data / symmetric NAT). Credentials stay on this computer: codes carry
// only the relay's address, never the username or password.
// provider "cloudflare": the user's own Cloudflare Realtime TURN key (1,000 GB/month
// free); 24-hour credentials are generated on demand and cached.
// provider "manual": any TURN service with static username/password.
function turnConfig() {
  let t = null;
  try { t = JSON.parse(localStorage.getItem('turn') || 'null'); } catch {}
  return {
    enabled: false, provider: 'manual', urls: [], username: '', credential: '', cfKeyId: '', cfToken: '', cfCache: null,
    ...(t && typeof t === 'object' ? t : {}),
  };
}

function saveTurnConfig(cfg) {
  const prev = turnConfig();
  const provider = cfg.provider === 'cloudflare' ? 'cloudflare' : 'manual';
  const urls = (cfg.urls || []).map((u) => u.trim()).filter((u) => /^turns?:[^\s]+$/i.test(u));
  const cfKeyId = String(cfg.cfKeyId || '').trim();
  const cfToken = String(cfg.cfToken || '').trim();
  const ready = provider === 'cloudflare' ? !!(cfKeyId && cfToken) : urls.length > 0;
  const clean = {
    enabled: !!cfg.enabled && ready, provider, urls,
    username: String(cfg.username || ''), credential: String(cfg.credential || ''),
    cfKeyId, cfToken,
    cfCache: prev.cfKeyId === cfKeyId && prev.cfToken === cfToken ? prev.cfCache : null,
  };
  localStorage.setItem('turn', JSON.stringify(clean));
  return clean;
}

// The TURN entry to use right now (null when off or Cloudflare credentials not fetched yet).
function activeTurn(t = turnConfig()) {
  if (!t.enabled) return null;
  if (t.provider === 'cloudflare') {
    const c = t.cfCache;
    return c && c.expires > Date.now() + 5 * 60 * 1000 ? { urls: c.urls, username: c.username, credential: c.credential } : null;
  }
  return t.urls.length ? { urls: t.urls, username: t.username, credential: t.credential } : null;
}

// Call before creating a session: refreshes Cloudflare credentials when missing or old.
async function prepareTurn(force = false) {
  const t = turnConfig();
  if (!t.enabled || t.provider !== 'cloudflare') return { ok: true };
  const c = t.cfCache;
  if (!force && c && c.expires > Date.now() + 60 * 60 * 1000) return { ok: true };
  const r = await window.md.cloudflareTurn(t.cfKeyId, t.cfToken);
  if (r.error) return { ok: false, error: t(r.error, { status: r.status }) };
  t.cfCache = r;
  localStorage.setItem('turn', JSON.stringify(t));
  return { ok: true };
}

function iceServers() {
  const list = [{ urls: stunServers() }];
  const turn = activeTurn();
  if (turn) list.push(turn);
  return list;
}

// Allocate a relay address with the given TURN settings; proves they work.
async function testTurn(cfg, timeoutMs = 8000) {
  const pc = new RTCPeerConnection({
    iceServers: [{ urls: cfg.urls, username: cfg.username, credential: cfg.credential }],
    iceTransportPolicy: 'relay',
  });
  pc.createDataChannel('probe');
  let error = '';
  pc.onicecandidateerror = (e) => { error = `${e.errorCode} ${e.errorText} (${e.url})`; };
  const t0 = performance.now();
  const relay = new Promise((resolve) => {
    pc.onicecandidate = (e) => {
      if (e.candidate && e.candidate.type === 'relay') resolve({ ok: true, ms: Math.round(performance.now() - t0), address: `${e.candidate.address}:${e.candidate.port}`, url: e.candidate.url || '' });
    };
    setTimeout(() => resolve({ ok: false, error: error || t('turn.noResp') }), timeoutMs);
  });
  await pc.setLocalDescription(await pc.createOffer());
  const r = await relay;
  pc.close();
  return r;
}

function saveStunServers(list) {
  const clean = list.map((s) => s.trim()).filter((s) => /^stuns?:[^\s]+$/i.test(s));
  localStorage.setItem('stun', JSON.stringify(clean.length ? clean : DEFAULT_STUN));
  return stunServers();
}

// --- code encoding ----------------------------------------------------------
function b64url(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function unb64url(str) {
  const s = atob(str.replace(/-/g, '+').replace(/_/g, '/'));
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

async function pipeBytes(bytes, stream) {
  const res = new Response(new Blob([bytes]).stream().pipeThrough(stream));
  return new Uint8Array(await res.arrayBuffer());
}

async function encodeCode(obj) {
  const raw = new TextEncoder().encode(JSON.stringify(obj));
  const packed = await pipeBytes(raw, new CompressionStream('deflate-raw'));
  return CODE_PREFIX + b64url(packed);
}

async function decodeCode(code) {
  const clean = String(code || '').replace(/\s+/g, '');
  if (!clean.startsWith(CODE_PREFIX)) throw new Error('Invalid code');
  const bytes = await pipeBytes(unb64url(clean.slice(CODE_PREFIX.length)), new DecompressionStream('deflate-raw'));
  const obj = JSON.parse(new TextDecoder().decode(bytes));
  if (!obj || obj.v !== 1 || !(obj.sdp || obj.mdp)) throw new Error('Unrecognized code'); // mdp: native protocol (mdp.js)
  return obj;
}

// Short Authentication String: both sides derive the same 6 digits from the
// two DTLS certificate fingerprints. Matching digits prove nobody tampered with the codes.
async function verificationCode(sdpA, sdpB) {
  const fp = (sdp) => (sdp.match(/a=fingerprint:\S+ (\S+)/) || [])[1] || '';
  return codeFromFingerprints(fp(sdpA), fp(sdpB));
}

// Also used by the native protocol (mdp.js) with its certificate fingerprints.
async function codeFromFingerprints(a, b) {
  const material = [a, b].sort().join('|');
  const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(material)));
  const n = ((hash[0] << 16) | (hash[1] << 8) | hash[2]) % 1000000;
  const s = String(n).padStart(6, '0');
  return `${s.slice(0, 3)} ${s.slice(3)}`;
}

// Codes carry every candidate (no trickle ICE), so wait for gathering to finish.
// Some STUN servers never answer (e.g. no IPv6 route); once a public (srflx)
// address is known, give the rest a short grace period instead of the full timeout.
function waitForIce(pc, timeoutMs = activeTurn() ? 7000 : 5000, graceMs = 1200) {
  // With TURN configured, the relay address is the one that matters on hostile networks.
  const want = activeTurn() ? 'relay' : 'srflx';
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve) => {
    let grace = null;
    const done = () => {
      clearTimeout(timer);
      clearTimeout(grace);
      pc.removeEventListener('icegatheringstatechange', check);
      pc.removeEventListener('icecandidate', onCand);
      resolve();
    };
    const check = () => { if (pc.iceGatheringState === 'complete') done(); };
    const onCand = (e) => {
      if (!grace && e.candidate && e.candidate.type === want) grace = setTimeout(done, graceMs);
    };
    const timer = setTimeout(done, timeoutMs);
    pc.addEventListener('icegatheringstatechange', check);
    pc.addEventListener('icecandidate', onCand);
  });
}

// Hardware-encodable codecs first (checked for this capture size), then the software order.
async function codecOrder(width, height) {
  const hw = [];
  if (navigator.mediaCapabilities && navigator.mediaCapabilities.encodingInfo) {
    for (const contentType of HW_CANDIDATES) {
      try {
        const r = await navigator.mediaCapabilities.encodingInfo({
          type: 'webrtc', video: { contentType, width, height, bitrate: 4_000_000, framerate: 30 },
        });
        if (r.supported && r.powerEfficient) hw.push(contentType);
      } catch {}
    }
  }
  return [...hw, ...CODEC_ORDER.filter((c) => !hw.includes(c))];
}

async function preferCodecs(transceiver, width, height) {
  if (!transceiver || !transceiver.setCodecPreferences || !window.RTCRtpReceiver.getCapabilities) return [];
  const caps = RTCRtpReceiver.getCapabilities('video');
  if (!caps) return [];
  const order = await codecOrder(width, height);
  const rank = (c) => {
    const i = order.indexOf(c.mimeType);
    return i === -1 ? order.length : i; // rtx/red/fec keep their place after real codecs
  };
  const codecs = [...caps.codecs].sort((a, b) => rank(a) - rank(b));
  try { transceiver.setCodecPreferences(codecs); } catch (e) { console.warn('setCodecPreferences', e); }
  return order;
}

// --- network diagnostics --------------------------------------------------------
// Asks every STUN server separately which public address our *same* local socket
// maps to. Same mapping for all servers = "cone" NAT (direct P2P works). Different
// ports per server = symmetric NAT: without a relay (TURN) a connection only works
// if the other side has an open/cone NAT.
// Which STUN servers answer at all, each probed on its own (shows blocked ports).
async function probeStun(url, timeoutMs) {
  const t0 = performance.now();
  const pc = new RTCPeerConnection({ iceServers: [{ urls: url }] });
  pc.createDataChannel('probe');
  const ms = new Promise((resolve) => {
    pc.onicecandidate = (e) => {
      if (e.candidate && e.candidate.type === 'srflx') resolve(Math.round(performance.now() - t0));
    };
    setTimeout(() => resolve(null), timeoutMs);
  });
  await pc.setLocalDescription(await pc.createOffer());
  const result = await ms;
  pc.close();
  return { url, ms: result };
}

async function detectNat(timeoutMs = 6000) {
  const servers = stunServers();
  const reach = Promise.all(servers.map((u) => probeStun(u, timeoutMs)));
  const pc = new RTCPeerConnection({ iceServers: servers.map((u) => ({ urls: u })) });
  const host = [];
  const srflx = [];
  pc.createDataChannel('probe');
  pc.onicecandidate = (e) => {
    const c = e.candidate;
    if (!c || !c.address || c.protocol !== 'udp') return;
    if (c.type === 'host') host.push(`${c.address}:${c.port}`);
    if (c.type === 'srflx') srflx.push({ pub: `${c.address}:${c.port}`, ip: c.address, port: c.port, base: `${c.relatedAddress}:${c.relatedPort}`, server: c.url || e.url || '' });
  };
  await pc.setLocalDescription(await pc.createOffer());
  await waitForIce(pc, timeoutMs, timeoutMs);
  pc.close();

  const byBase = new Map();
  for (const c of srflx) {
    if (!byBase.has(c.base)) byBase.set(c.base, new Set());
    byBase.get(c.base).add(c.pub);
  }
  // Symmetric = the same local socket gets different public *ports* per destination.
  // Different public IPs with the same port is a multi-WAN / load-balanced router.
  const portsOf = (set) => new Set([...set].map((p) => p.slice(p.lastIndexOf(':') + 1)));
  const symmetric = [...byBase.values()].some((set) => portsOf(set).size > 1);
  const multiIp = [...byBase.values()].some((set) => set.size > 1);
  const portPreserved = srflx.length > 0 && srflx.every((c) => c.base.endsWith(`:${c.port}`));
  const publicIps = [...new Set(srflx.map((c) => c.ip))];
  const openInternet = srflx.some((c) => c.base.split(':')[0] === c.ip);
  let type;
  let verdict;
  if (!srflx.length) {
    type = 'blocked';
    verdict = t('nat.blocked');
  } else if (openInternet) {
    type = 'open';
    verdict = t('nat.open');
  } else if (symmetric) {
    type = 'symmetric';
    verdict = t('nat.symmetric');
  } else if (multiIp) {
    type = 'multi-ip';
    verdict = t('nat.multi');
  } else {
    type = 'cone';
    verdict = t('nat.cone');
  }
  const servers2 = await reach;
  const prep = await prepareTurn();
  const turn = activeTurn();
  const turnResult = turn ? await testTurn(turn) : null;
  return {
    type, verdict, publicIps, portPreserved, hostCount: host.length,
    mappings: srflx.map((c) => `${c.base} → ${c.pub} (${c.server})`),
    servers: servers2.map((r) => `${r.ms === null ? '✗ no answer ' : `✓ ${String(r.ms).padStart(4)} ms  `} ${r.url}`),
    turn: !prep.ok ? `TURN: ✗ ${prep.error}`
      : !turn ? 'TURN: not configured'
      : turnResult.ok ? `TURN: ✓ relay ${turnResult.address} (${turnResult.ms} ms)` : `TURN: ✗ ${turnResult.error}`,
  };
}

// --- session ------------------------------------------------------------------
class Session extends EventTarget {
  // native: the subclass in mdp.js brings its own transport instead of RTCPeerConnection.
  constructor(role, native = false) {
    super();
    this.role = role; // 'host' | 'viewer'
    this.pc = native ? null : new RTCPeerConnection({ iceServers: iceServers(), bundlePolicy: 'max-bundle' });
    this.ctl = null;
    this.file = null;
    this.stream = null;
    this.closed = false;
    this.sendQueue = Promise.resolve();
    this.incoming = null;
    this.disconnectTimer = null;
    this.connected = false;
    this.punchTimer = null;
    this.deadline = null;
    this.reason = '';
    this.audioTx = null;
    this.mic = null;
    this.t0 = performance.now();
    this.logLines = [];
    const turn = activeTurn();
    this.log(`role: ${role}, STUN: ${stunServers().join(', ')}, TURN: ${turn ? `${turnConfig().provider}: ${turn.urls.join(', ')}` : 'none'}`);
    if (native) return;

    this.pc.addEventListener('connectionstatechange', () => this.onState());
    this.pc.addEventListener('iceconnectionstatechange', () => this.log(`ICE: ${this.pc.iceConnectionState}`));
    this.pc.addEventListener('icecandidate', (e) => {
      if (e.candidate && e.candidate.address) this.log(`local candidate: ${e.candidate.type} ${e.candidate.protocol} ${e.candidate.address}:${e.candidate.port}`);
    });
    this.pc.addEventListener('icecandidateerror', (e) => this.log(`STUN error: ${e.url} ${e.errorCode} ${e.errorText}`));
    this.pc.addEventListener('track', (e) => {
      // Remote desktop wants the newest frame now, not a smooth buffer: ~0 jitter buffer.
      if (e.track.kind === 'video') {
        if ('jitterBufferTarget' in e.receiver) e.receiver.jitterBufferTarget = 0;
        else if ('playoutDelayHint' in e.receiver) e.receiver.playoutDelayHint = 0;
      }
      const stream = e.streams[0] || new MediaStream([e.track]);
      this.emit(e.track.kind === 'audio' ? 'audio' : 'track', stream);
    });
    this.pc.addEventListener('datachannel', (e) => this.bindChannel(e.channel));
  }

  emit(type, detail) { this.dispatchEvent(new CustomEvent(type, { detail })); }

  log(line) {
    const t = ((performance.now() - this.t0) / 1000).toFixed(1).padStart(6);
    this.logLines.push(`${t}s  ${line}`);
    if (this.logLines.length > 400) this.logLines.splice(1, 1);
  }

  // Summary of every ICE candidate pair: did any check get an answer? Explains failures.
  async pairReport() {
    const lines = [];
    try {
      const rep = await this.pc.getStats();
      const byId = new Map();
      rep.forEach((s) => byId.set(s.id, s));
      rep.forEach((s) => {
        if (s.type !== 'candidate-pair') return;
        const l = byId.get(s.localCandidateId) || {};
        const r = byId.get(s.remoteCandidateId) || {};
        lines.push(`  ${l.candidateType}/${l.address}:${l.port} ↔ ${r.candidateType}/${r.address}:${r.port}  ${s.state}  requests ${s.requestsSent || 0}, responses ${s.responsesReceived || 0}, incoming ${s.requestsReceived || 0}`);
      });
    } catch {}
    return lines.length ? lines.join('\n') : '  (no pairs)';
  }

  async report() {
    let pairs = await this.pairReport();
    if (pairs.includes('no pairs') && this.lastPairs) pairs = `${this.lastPairs}\n  (last seen)`;
    return `ManyDesk connection log\n${this.logLines.join('\n')}\nCandidate pairs:\n${pairs}`;
  }

  onState() {
    const s = this.pc.connectionState;
    this.log(`connection: ${s}`);
    this.emit('state', s);
    if (s === 'connected') {
      this.connected = true;
      this.stopPunching();
    }
    if (!this.connected) {
      // Before the first connection "failed" only means the peer hasn't answered our
      // checks yet (it may still be pasting the code). Retry instead of giving up.
      if (s === 'failed' || s === 'disconnected') this.punch();
      return;
    }
    clearTimeout(this.disconnectTimer);
    if (s === 'failed' || s === 'closed') this.close(t('conn.lost'));
    // "disconnected" can recover on its own; give it a moment.
    if (s === 'disconnected') this.disconnectTimer = setTimeout(() => this.close(t('conn.lost')), 8000);
  }

  // Keep ICE checks flowing toward the peer's candidates. Chromium drops all candidate
  // pairs once ICE fails; re-adding the candidates (with a fresh foundation so they are
  // not deduplicated) recreates the pairs, which keeps our NAT mapping open for the peer.
  startPunching(remoteSdp, timeoutMs) {
    const mid = (remoteSdp.match(/^a=mid:(\S+)/m) || [])[1] || '0';
    this.remoteCandidates = (remoteSdp.match(/^a=candidate:.*$/gm) || []).map((l) => l.slice(2).trim());
    this.remoteCandidates.forEach((c) => {
      const f = c.split(' ');
      this.log(`remote candidate: ${f[7]} ${f[2]} ${f[4]}:${f[5]}`);
    });
    if (!this.remoteCandidates.some((c) => / typ srflx/.test(c))) this.log('WARNING: no public (srflx) address from the peer — its STUN failed');
    if (this.remoteCandidates.some((c) => / typ relay/.test(c))) this.log('peer offers a TURN relay address');
    this.punchMid = mid;
    this.punchGen = 0;
    this.stopPunching();
    this.punchTimer = setInterval(() => {
      // Chromium drops failed pairs, so keep the last non-empty snapshot for the report.
      this.pairReport().then((r) => { if (!r.includes('no pairs')) this.lastPairs = r; });
      const st = this.pc.iceConnectionState;
      if (st === 'failed' || st === 'disconnected') this.punch();
    }, PUNCH_INTERVAL_MS);
    this.deadline = setTimeout(() => {
      if (!this.connected) this.close(this.role === 'viewer' ? t('conn.acceptTimeout') : t(activeTurn() ? 'conn.noRouteTurn' : 'conn.noRoute'));
    }, timeoutMs);
  }

  punch() {
    if (this.closed || this.connected || !this.remoteCandidates || this.punching) return;
    this.punching = true;
    const gen = ++this.punchGen;
    if (gen === 1 || gen % 10 === 0) this.pairReport().then((r) => this.log(`retry #${gen}, pairs:\n${r}`));
    Promise.all(this.remoteCandidates.map((c) => this.pc.addIceCandidate({
      candidate: c.replace(/^candidate:(\S+)/, (_m, f) => `candidate:${f}p${gen}`),
      sdpMid: this.punchMid,
    }).catch(() => {}))).finally(() => { this.punching = false; });
  }

  stopPunching() {
    clearInterval(this.punchTimer);
    clearTimeout(this.deadline);
    this.punchTimer = null;
    this.deadline = null;
  }

  bindChannel(ch) {
    if (ch.label === 'ctl') {
      this.ctl = ch;
      // The control channel can open a moment before connectionState reports
      // "connected"; it is the real "usable" signal, so mark the session connected here.
      ch.onopen = () => {
        if (!this.connected) {
          this.connected = true;
          this.stopPunching();
        }
        this.emit('open');
      };
      ch.onclose = () => this.close(t('peer.left'));
      ch.onmessage = (e) => {
        try { this.emit('message', JSON.parse(e.data)); } catch {}
      };
    } else if (ch.label === 'file') {
      this.file = ch;
      ch.binaryType = 'arraybuffer';
      ch.bufferedAmountLowThreshold = 1024 * 1024;
      ch.onmessage = (e) => this.onFileMessage(e.data);
    }
  }

  // Host: capture screen, create offer, return invite code.
  // stream: an already captured screen (Tauri/WebKit must capture inside the click);
  // otherwise it is captured here (Electron picks the screen in its handler).
  async createInvite(stream) {
    this.stream = stream || await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: { ideal: 60, max: 60 } }, audio: false });
    const track = this.stream.getVideoTracks()[0];
    track.contentHint = 'detail';
    track.addEventListener('ended', () => this.close(t('share.stopped')));
    this.videoSender = this.pc.addTrack(track, this.stream);
    const { width = 1920, height = 1080 } = track.getSettings();
    const order = await preferCodecs(this.pc.getTransceivers().find((t) => t.sender === this.videoSender), width, height);
    this.log(`capture ${width}x${height}, codec order: ${order.map((c) => c.replace('video/', '')).join(' > ')}`);
    // Two-way voice: negotiated up front so the microphone can be switched on later
    // without a new code exchange (there is no signaling channel to renegotiate over).
    this.audioTx = this.pc.addTransceiver('audio', { direction: 'sendrecv' });

    this.bindChannel(this.pc.createDataChannel('ctl', { ordered: true }));
    this.bindChannel(this.pc.createDataChannel('file', { ordered: true }));

    await this.pc.setLocalDescription(await this.pc.createOffer());
    await waitForIce(this.pc);
    this.setQuality('auto');
    this.autoTimer = setInterval(() => this.autoTick(), 2000);
    return encodeCode({ v: 1, t: 'offer', sdp: this.pc.localDescription.sdp });
  }

  // Host: apply a quality preset to the outgoing screen video.
  setQuality(mode) {
    if (!QUALITY[mode] || !this.videoSender) return;
    this.quality = mode;
    this.level = AUTO_START;
    this.motion = false;
    this.applyEncoding();
  }

  // Host: the viewer's stage size in device pixels; never encode more than it shows.
  setView(w, h) {
    w = Math.round(Number(w)); h = Math.round(Number(h));
    if (!(w >= 160 && h >= 120 && w <= 16384 && h <= 16384)) return;
    this.view = { w, h };
    this.applyEncoding();
  }

  async applyEncoding() {
    const sender = this.videoSender;
    if (!sender || !sender.track || this.closed) return;
    const q = QUALITY[this.quality] || QUALITY.auto;
    const auto = this.quality === 'auto';
    const lv = auto ? AUTO_LEVELS[this.level] : { s: 1, fps: q.fps, br: 1 };
    const { width: sw = 1920, height: sh = 1080 } = sender.track.getSettings();
    const fit = this.view ? Math.max(1, sw / this.view.w, sh / this.view.h) : 1;
    const scale = Math.max(1, fit * q.extra * lv.s);
    const fps = Math.min(q.fps * (auto ? 2 : 1), lv.fps);
    const pixels = (sw / scale) * (sh / scale);
    const bitrate = Math.round(Math.min(q.maxBr * lv.br, Math.max(q.minBr, pixels * fps * q.bpp * lv.br)));
    // auto: static text stays sharp; continuous motion (video, scrolling) keeps its frame rate.
    const pref = auto ? (this.motion ? 'maintain-framerate' : 'maintain-resolution') : q.pref;
    const key = `${scale.toFixed(3)}|${fps}|${bitrate}|${pref}`;
    if (key === this.encKey) return;
    this.encKey = key;
    try {
      const p = sender.getParameters();
      p.degradationPreference = pref;
      if (!p.encodings || !p.encodings.length) p.encodings = [{}];
      Object.assign(p.encodings[0], { maxBitrate: bitrate, maxFramerate: fps, scaleResolutionDownBy: scale });
      await sender.setParameters(p);
      this.log(`encode ${Math.round(sw / scale)}x${Math.round(sh / scale)} @${fps} ≤${(bitrate / 1e6).toFixed(1)} Mbit/s ${pref}${auto ? ` (auto L${this.level})` : ''}`);
    } catch (e) { this.encKey = null; console.warn('setParameters', e); }
  }

  // Host, "auto" mode: step resolution/fps down when the encoder is CPU- or
  // bandwidth-limited, back up after a calm period with spare bandwidth.
  async autoTick() {
    if (this.closed || !this.connected || this.quality !== 'auto') return;
    let out = null; let pair = null;
    try {
      const rep = await this.pc.getStats();
      rep.forEach((s) => {
        if (s.type === 'outbound-rtp' && s.kind === 'video') out = s;
        if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') pair = s;
      });
    } catch { return; }
    if (!out) return;
    const now = performance.now();
    const prev = this.autoPrev;
    this.autoPrev = { frames: out.framesEncoded || 0, t: now };
    if (!prev) return;
    const fps = ((out.framesEncoded || 0) - prev.frames) / ((now - prev.t) / 1000);
    const cap = AUTO_LEVELS[this.level].fps;

    // Motion: the capturer only delivers frames on change, so a high encode rate means motion.
    const moving = fps >= Math.min(20, cap * 0.7);
    this.motionRun = moving ? (this.motionRun || 0) + 1 : 0;
    const motion = this.motion ? this.motionRun > 0 || fps >= 8 : this.motionRun >= 2;

    const limited = out.qualityLimitationReason === 'cpu' || out.qualityLimitationReason === 'bandwidth';
    this.bad = limited ? (this.bad || 0) + 1 : 0;
    this.good = limited ? 0 : (this.good || 0) + 1;
    let level = this.level;
    if (this.bad >= 2 && level < AUTO_LEVELS.length - 1) {
      level++;
      this.calmUntil = now + 20000;
    } else if (this.good >= 5 && level > 0 && now > (this.calmUntil || 0)) {
      const avail = pair && pair.availableOutgoingBitrate;
      const p = this.videoSender.getParameters();
      const max = p.encodings && p.encodings[0] && p.encodings[0].maxBitrate;
      if (!avail || !max || avail > max * 1.3) level--;
    }
    if (level !== this.level || motion !== this.motion) {
      if (level !== this.level) { this.bad = 0; this.good = 0; this.log(`auto: ${out.qualityLimitationReason || 'ok'} → L${level}`); }
      this.level = level;
      this.motion = motion;
      this.applyEncoding();
    }
  }

  // Both sides: switch the microphone on/off on the pre-negotiated audio transceiver.
  async setMic(on) {
    if (!this.audioTx) throw new Error(t('mic.noAudio'));
    if (on && !this.mic) {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      this.mic = stream.getAudioTracks()[0];
      await this.audioTx.sender.replaceTrack(this.mic);
    } else if (!on && this.mic) {
      await this.audioTx.sender.replaceTrack(null);
      this.mic.stop();
      this.mic = null;
    }
    this.send({ t: 'mic', on: !!this.mic });
    return !!this.mic;
  }

  // Host: apply the viewer's answer code.
  async acceptAnswer(code) {
    const obj = await decodeCode(code);
    if (obj.t !== 'answer') throw new Error('Not an answer code');
    await this.pc.setRemoteDescription({ type: 'answer', sdp: obj.sdp });
    this.startPunching(obj.sdp, HOST_CONNECT_MS);
    this.sas = await verificationCode(this.pc.localDescription.sdp, obj.sdp);
    return this.sas;
  }

  // Viewer: apply the host's invite code and produce an answer code.
  async answerInvite(code) {
    const obj = await decodeCode(code);
    if (obj.t !== 'offer') throw new Error('Not an invite code');
    await this.pc.setRemoteDescription({ type: 'offer', sdp: obj.sdp });
    this.audioTx = this.pc.getTransceivers().find((t) => t.receiver.track.kind === 'audio') || null;
    if (this.audioTx) this.audioTx.direction = 'sendrecv';
    await this.pc.setLocalDescription(await this.pc.createAnswer());
    await waitForIce(this.pc);
    this.startPunching(obj.sdp, VIEWER_WAIT_MS);
    this.sas = await verificationCode(obj.sdp, this.pc.localDescription.sdp);
    return { code: await encodeCode({ v: 1, t: 'answer', sdp: this.pc.localDescription.sdp }), sas: this.sas };
  }

  send(msg) {
    if (this.ctl && this.ctl.readyState === 'open') this.ctl.send(JSON.stringify(msg));
  }

  // --- file transfer --------------------------------------------------------
  sendFile(file, onProgress) {
    const run = async () => {
      const ch = this.file;
      if (!ch || ch.readyState !== 'open') throw new Error(t('file.noChannel'));
      const id = crypto.randomUUID();
      ch.send(JSON.stringify({ t: 'fstart', id, name: file.name, size: file.size }));
      let offset = 0;
      while (offset < file.size) {
        if (this.closed) throw new Error(t('file.closed'));
        if (ch.bufferedAmount > FILE_HIGH_WATER) {
          await new Promise((r) => ch.addEventListener('bufferedamountlow', r, { once: true }));
          continue;
        }
        const buf = await file.slice(offset, offset + FILE_CHUNK).arrayBuffer();
        ch.send(buf);
        offset += buf.byteLength;
        onProgress && onProgress(offset / file.size);
      }
      ch.send(JSON.stringify({ t: 'fend', id }));
      onProgress && onProgress(1);
    };
    const p = this.sendQueue.then(run);
    this.sendQueue = p.catch(() => {});
    return p;
  }

  onFileMessage(data) {
    // Messages arrive in order on a reliable channel; chain the async writes to keep it that way.
    this.recvChain = (this.recvChain || Promise.resolve()).then(async () => {
      if (typeof data === 'string') {
        const m = JSON.parse(data);
        if (m.t === 'fstart') {
          const path = await window.md.fileBegin(m.id, m.name, m.size);
          this.incoming = { id: m.id, name: m.name, size: m.size, got: 0, path };
          this.emit('file-progress', { ...this.incoming, progress: 0 });
        } else if (m.t === 'fend' && this.incoming && this.incoming.id === m.id) {
          const path = await window.md.fileEnd(m.id, true);
          this.emit('file-done', { id: m.id, name: this.incoming.name, path });
          this.incoming = null;
        }
      } else if (this.incoming) {
        await window.md.fileChunk(this.incoming.id, data);
        this.incoming.got += data.byteLength;
        this.emit('file-progress', { ...this.incoming, progress: this.incoming.size ? this.incoming.got / this.incoming.size : 1 });
      }
    }).catch((e) => console.error('file recv', e));
  }

  // --- stats ------------------------------------------------------------------
  async stats() {
    const report = await this.pc.getStats();
    let pair = null;
    const byId = new Map();
    let inbound = null;
    let outbound = null;
    report.forEach((s) => {
      byId.set(s.id, s);
      if (s.type === 'transport' && s.selectedCandidatePairId) pair = s.selectedCandidatePairId;
      if (s.type === 'inbound-rtp' && s.kind === 'video') inbound = s;
      if (s.type === 'outbound-rtp' && s.kind === 'video') outbound = s;
    });
    const cp = pair && byId.get(pair);
    const local = cp && byId.get(cp.localCandidateId);
    const remote = cp && byId.get(cp.remoteCandidateId);
    const rtp = inbound || outbound;
    let totalSent = 0;
    let totalRecv = 0;
    report.forEach((s) => {
      if (s.type === 'transport') { totalSent += s.bytesSent || 0; totalRecv += s.bytesReceived || 0; }
    });
    const codec = rtp && rtp.codecId && byId.get(rtp.codecId);
    const hw = rtp ? (inbound ? rtp.powerEfficientDecoder : rtp.powerEfficientEncoder) : undefined;
    const bytes = rtp ? (inbound ? rtp.bytesReceived : rtp.bytesSent) : 0;
    const now = performance.now();
    let kbps = 0;
    if (this.lastBytes !== undefined) kbps = ((bytes - this.lastBytes) * 8) / (now - this.lastTime);
    this.lastBytes = bytes;
    this.lastTime = now;
    return {
      route: local && remote ? `${local.candidateType} ↔ ${remote.candidateType}` : '—',
      lan: !!(local && remote && local.candidateType === 'host' && remote.candidateType === 'host'),
      relay: !!(local && remote && (local.candidateType === 'relay' || remote.candidateType === 'relay')),
      rtt: cp && cp.currentRoundTripTime ? Math.round(cp.currentRoundTripTime * 1000) : null,
      fps: rtp ? Math.round(rtp.framesPerSecond || 0) : 0,
      kbps: Math.round(kbps),
      size: rtp && rtp.frameWidth ? `${rtp.frameWidth}×${rtp.frameHeight}` : '',
      codec: codec ? codec.mimeType.replace('video/', '') : '',
      hw: hw === true,
      impl: rtp ? (inbound ? rtp.decoderImplementation : rtp.encoderImplementation) || '' : '',
      limit: outbound ? outbound.qualityLimitationReason || '' : '',
      totalMB: (totalSent + totalRecv) / 1048576,
    };
  }

  // Which kinds of addresses a code advertises; used to explain connection failures.
  static candidateSummary(sdp) {
    const types = new Set((sdp.match(/ typ (\w+)/g) || []).map((t) => t.slice(5)));
    return { host: types.has('host'), srflx: types.has('srflx'), relay: types.has('relay') };
  }

  close(reason = '') {
    if (this.closed) return;
    this.closed = true;
    this.reason = reason;
    this.log(`closed: ${reason || 'by user'}`);
    // Freeze the final pair statistics before the connection object is torn down.
    this.finalReport = this.logLines.length ? this.report() : null;
    this.stopPunching();
    clearTimeout(this.disconnectTimer);
    clearInterval(this.autoTimer);
    try { this.send({ t: 'bye' }); } catch {}
    if (this.incoming) window.md.fileEnd(this.incoming.id, false);
    if (this.stream) this.stream.getTracks().forEach((t) => t.stop());
    if (this.mic) { this.mic.stop(); this.mic = null; }
    try { this.pc.close(); } catch {}
    this.emit('closed');
  }
}

async function inspectCode(code) {
  return Session.candidateSummary((await decodeCode(code)).sdp);
}

window.RTC = { Session, encodeCode, decodeCode, codeFromFingerprints, CODE_PREFIX, stunServers, saveStunServers, DEFAULT_STUN, inspectCode, detectNat, turnConfig, saveTurnConfig, testTurn, prepareTurn, activeTurn };
})();
