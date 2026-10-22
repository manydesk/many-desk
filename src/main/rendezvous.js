// Decentralized rendezvous over two independent public networks:
//   * Nostr relays (WebSocket, many independent operators)
//   * MQTT brokers (WebSocket, different companies and domains)
// A network or domain blocked on one side does not stop the handshake as long as
// any relay or broker gets through.
//
// Only the short connection handshake (an encrypted request, the compressed
// offer/answer codes) travels this way; screen, input, voice and files still flow
// peer-to-peer. Every message is a Schnorr-signed Nostr event made with a throwaway
// per-run key and verified here, so neither relays nor brokers can forge or alter it;
// the renderer pins the peer's key once a handshake starts.
const { EventEmitter } = require('events');
const crypto = require('crypto');
const { schnorr } = require('@noble/curves/secp256k1');
const { MqttConnection } = require('./mqtt');

const KIND = 21059; // ephemeral (20000-29999): relays forward but do not store
// Independent public relays verified to accept and forward ephemeral events.
const DEFAULT_RELAYS = [
  'wss://relay.damus.io',
  'wss://relay.primal.net',
  'wss://offchain.pub',
  'wss://relay.nostr.net',
  'wss://relay.snort.social',
  'wss://nostr-pub.wellorder.net',
  'wss://nostr.oxtr.dev',
  'wss://nostr.bitcoiner.social',
  'wss://relay.coinos.io',
  'wss://yabu.me',
  'wss://nostr.sathoarder.com',
];
// Public MQTT-over-WebSocket brokers (different operators from the relays above).
const DEFAULT_BROKERS = [
  'wss://broker.hivemq.com:8884/mqtt',
  'wss://broker.emqx.io:8084/mqtt',
  'wss://broker-cn.emqx.io:8084/mqtt',
  'wss://test.mosquitto.org:8081/mqtt',
];
const MQTT_PREFIX = 'manydesk/v1/';
// Computer clocks can be minutes apart; anything inside this window counts as current.
const CLOCK_SKEW_S = 600;

// Relays reject events whose timestamp is far from their own clock, and a computer's
// clock can be off by many minutes. So time is taken from the network: Cloudflare's
// trace endpoint (exact), else any HTTPS server's Date header (1 s precision).
let clockOffsetMs = 0;
const now = () => Date.now() + clockOffsetMs;

async function syncClock() {
  try {
    const t0 = Date.now();
    const res = await fetch('https://cloudflare.com/cdn-cgi/trace', { signal: AbortSignal.timeout(5000) });
    const ts = Number(((await res.text()).match(/^ts=([\d.]+)/m) || [])[1]);
    if (ts) {
      clockOffsetMs = Math.round(ts * 1000 - (t0 + Date.now()) / 2);
      return clockOffsetMs;
    }
  } catch {}
  for (const url of ['https://www.google.com', 'https://www.microsoft.com']) {
    try {
      const t0 = Date.now();
      const res = await fetch(url, { method: 'HEAD', signal: AbortSignal.timeout(5000) });
      const date = Date.parse(res.headers.get('date'));
      if (date) {
        clockOffsetMs = Math.round(date + 500 - (t0 + Date.now()) / 2);
        return clockOffsetMs;
      }
    } catch {}
  }
  return null;
}

const hex = (bytes) => Buffer.from(bytes).toString('hex');
const sha256 = (data) => crypto.createHash('sha256').update(data).digest();

function eventId(ev) {
  return hex(sha256(JSON.stringify([0, ev.pubkey, ev.created_at, ev.kind, ev.tags, ev.content])));
}

class Rendezvous extends EventEmitter {
  constructor(relays = DEFAULT_RELAYS, brokers = DEFAULT_BROKERS) {
    super();
    this.sk = schnorr.utils.randomPrivateKey();
    this.pubkey = hex(schnorr.getPublicKey(this.sk));
    this.relays = new Map(relays.map((url) => [url, { url, kind: 'nostr', ws: null, open: false, backoff: 1000, timer: null, eose: new Set() }]));
    this.brokers = new Map(brokers.map((url) => [url, { url, kind: 'mqtt', conn: null, open: false, backoff: 1000, timer: null }]));
    this.subs = new Map(); // topic -> { id, liveOnly }
    this.seen = new Set();
    this.acks = new Map(); // event id -> callback(accepted)
    this.stopped = false;
  }

  start() {
    for (const r of this.relays.values()) this.connectRelay(r);
    for (const b of this.brokers.values()) this.connectBroker(b);
    const sync = () => syncClock().then((off) => {
      if (off !== null && Math.abs(off) > 2000) console.log(`[rv] system clock is off by ${Math.round(off / 1000)} s — using network time`);
      this.emitStatus();
    });
    sync();
    this.clockTimer = setInterval(sync, 30 * 60 * 1000);
  }

  stop() {
    this.stopped = true;
    clearInterval(this.clockTimer);
    for (const r of this.relays.values()) {
      clearTimeout(r.timer);
      try { r.ws && r.ws.close(); } catch {}
    }
    for (const b of this.brokers.values()) {
      clearTimeout(b.timer);
      if (b.conn) b.conn.close();
    }
  }

  status() {
    const list = [...this.relays.values(), ...this.brokers.values()];
    return {
      connected: list.filter((x) => x.open).length,
      total: list.length,
      relays: list.map((x) => ({ url: x.url, kind: x.kind, open: x.open })),
      clockOffsetS: Math.round(clockOffsetMs / 1000),
    };
  }

  emitStatus() { this.emit('status', this.status()); }

  retry(x, connect) {
    if (this.stopped) return;
    clearTimeout(x.timer);
    x.timer = setTimeout(() => connect.call(this, x), x.backoff);
    x.backoff = Math.min(x.backoff * 2, 30000);
  }

  // ---- Nostr ----
  connectRelay(r) {
    if (this.stopped) return;
    let ws;
    try { ws = new WebSocket(r.url); } catch { return this.retry(r, this.connectRelay); }
    r.ws = ws;
    r.eose = new Set();
    ws.onopen = () => {
      r.open = true;
      r.backoff = 1000;
      for (const [topic, sub] of this.subs) this.sendReq(r, topic, sub);
      this.emitStatus();
    };
    ws.onmessage = (m) => this.onRelayMessage(r, m.data);
    ws.onclose = () => {
      const was = r.open;
      r.open = false;
      if (was) this.emitStatus();
      this.retry(r, this.connectRelay);
    };
    ws.onerror = () => {};
  }

  sendRelay(r, msg) {
    if (r.open && r.ws.readyState === WebSocket.OPEN) r.ws.send(JSON.stringify(msg));
  }

  sendReq(r, topic, sub) {
    // Ask for a generous window so a sender whose clock runs behind is still matched.
    // Old events that come back are told apart by EOSE, not by their timestamps.
    r.eose.delete(sub.id);
    this.sendRelay(r, ['REQ', sub.id, { kinds: [KIND], '#t': [topic], since: Math.floor(now() / 1000) - CLOCK_SKEW_S }]);
  }

  onRelayMessage(r, data) {
    let msg;
    try { msg = JSON.parse(data); } catch { return; }
    if (!Array.isArray(msg)) return;
    if (msg[0] === 'EVENT' && msg[2]) {
      const sub = [...this.subs.values()].find((s) => s.id === msg[1]);
      // Stored events (before EOSE) on a live-only topic are history, e.g. a request
      // from before a restart that some relay kept despite being ephemeral.
      if (sub && sub.liveOnly && !r.eose.has(sub.id)) return;
      this.onEvent(msg[2]);
    } else if (msg[0] === 'EOSE') {
      r.eose.add(msg[1]);
    } else if (msg[0] === 'OK') {
      const cb = this.acks.get(msg[1]);
      if (cb) cb(msg[2] === true);
    }
  }

  // ---- MQTT ----
  connectBroker(b) {
    if (this.stopped) return;
    b.conn = new MqttConnection(b.url, {
      onOpen: () => {
        b.open = true;
        b.backoff = 1000;
        for (const topic of this.subs.keys()) b.conn.subscribe(MQTT_PREFIX + topic);
        this.emitStatus();
      },
      onClose: (was) => {
        b.open = false;
        if (was) this.emitStatus();
        this.retry(b, this.connectBroker);
      },
      onMessage: (topic, payload) => {
        if (!topic.startsWith(MQTT_PREFIX) || payload.length > 64 * 1024) return;
        try { this.onEvent(JSON.parse(payload)); } catch {}
      },
    });
    b.conn.connect();
  }

  // ---- subscriptions & publishing ----

  // liveOnly: for the long-lived ID topic — ignore anything a relay had stored.
  // Fresh random reply topics accept stored events too (a reply may arrive at a relay
  // a moment before our subscription does).
  subscribe(topic, liveOnly = false) {
    if (this.subs.has(topic)) return;
    const sub = { id: 'md' + crypto.randomBytes(6).toString('hex'), liveOnly };
    this.subs.set(topic, sub);
    for (const r of this.relays.values()) this.sendReq(r, topic, sub);
    for (const b of this.brokers.values()) if (b.open) b.conn.subscribe(MQTT_PREFIX + topic);
  }

  unsubscribe(topic) {
    const sub = this.subs.get(topic);
    if (!sub) return;
    this.subs.delete(topic);
    for (const r of this.relays.values()) this.sendRelay(r, ['CLOSE', sub.id]);
    for (const b of this.brokers.values()) if (b.open) b.conn.unsubscribe(MQTT_PREFIX + topic);
  }

  // Sends to every connected relay and broker; resolves with how many took it.
  // Relays confirm with OK; brokers (QoS 0) count once the message is written.
  publish(topic, content, timeoutMs = 5000) {
    const ev = { pubkey: this.pubkey, created_at: Math.floor(now() / 1000), kind: KIND, tags: [['t', topic]], content };
    ev.id = eventId(ev);
    ev.sig = hex(schnorr.sign(ev.id, this.sk));
    this.seen.add(ev.id); // ignore our own echo

    let brokersOk = 0;
    for (const b of this.brokers.values()) if (b.open && b.conn.publish(MQTT_PREFIX + topic, JSON.stringify(ev))) brokersOk++;

    const open = [...this.relays.values()].filter((r) => r.open);
    return new Promise((resolve) => {
      let relaysOk = 0;
      let pending = open.length;
      const finish = () => {
        this.acks.delete(ev.id);
        clearTimeout(timer);
        resolve({ ok: relaysOk + brokersOk, sent: open.length + brokersOk, relays: relaysOk, brokers: brokersOk });
      };
      const timer = setTimeout(finish, timeoutMs);
      this.acks.set(ev.id, (accepted) => {
        if (accepted) relaysOk++;
        if (--pending <= 0) finish();
      });
      if (!open.length) return finish();
      for (const r of open) this.sendRelay(r, ['EVENT', ev]);
    });
  }

  onEvent(ev) {
    if (!ev || ev.kind !== KIND || typeof ev.content !== 'string' || typeof ev.id !== 'string' || this.seen.has(ev.id)) return;
    if (ev.content.length > 64 * 1024) return;
    const topicTag = Array.isArray(ev.tags) && ev.tags.find((t) => t[0] === 't');
    const topic = topicTag && topicTag[1];
    if (!topic || !this.subs.has(topic)) return;
    // Never trust the transport: check the event hash and the author's signature ourselves.
    try {
      if (eventId(ev) !== ev.id || !schnorr.verify(ev.sig, ev.id, ev.pubkey)) return;
    } catch { return; }
    if (Math.abs(now() / 1000 - ev.created_at) > CLOCK_SKEW_S) return;
    this.seen.add(ev.id);
    if (this.seen.size > 5000) this.seen = new Set([...this.seen].slice(-2500));
    this.emit('message', { topic, content: ev.content, pubkey: ev.pubkey });
  }
}

module.exports = { Rendezvous, DEFAULT_RELAYS, DEFAULT_BROKERS, now };
