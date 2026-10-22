// "Connect by ID" handshake on top of the main-process Nostr transport.
//
//   viewer ──req (to topic(ID), key from ID)──────────────▶ host   shows Accept / Reject
//   viewer ◀──offer code + host reply topic (session key)── host   on Accept
//   viewer ──answer code (session key)────────────────────▶ host   → P2P connection
//
// The ID only locates the host and wraps the first message. Everything after it uses
// a random per-request key, and each side pins the other's signing key, so someone
// who merely knows (or guesses) the ID cannot read the reply or slip in an answer.
// The host still has to press Accept, and both sides show the 6-digit SAS.
(() => {
  const api = window.md;
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const idKeys = new Map();

  const toHex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
  const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf)));
  const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

  function normalizeId(s) {
    return String(s || '').replace(/\D/g, '');
  }

  function formatId(id) {
    return normalizeId(id).replace(/(\d{3})(?=\d)/g, '$1 ');
  }

  function randomId() {
    const n = crypto.getRandomValues(new Uint32Array(1))[0] % 900000000;
    return String(100000000 + n);
  }

  function myId() {
    let id = '';
    try { id = localStorage.getItem('myId') || ''; } catch {}
    if (!/^\d{9}$/.test(id)) {
      id = randomId();
      try { localStorage.setItem('myId', id); } catch {}
    }
    return id;
  }

  function newId() {
    const id = randomId();
    try { localStorage.setItem('myId', id); } catch {}
    return id;
  }

  async function topicForId(id) {
    return toHex(await crypto.subtle.digest('SHA-256', enc.encode(`manydesk-topic-v1:${normalizeId(id)}`)));
  }

  function randomTopic() {
    return toHex(crypto.getRandomValues(new Uint8Array(32)));
  }

  async function keyForId(id) {
    id = normalizeId(id);
    if (!idKeys.has(id)) {
      const base = await crypto.subtle.importKey('raw', enc.encode(id), 'PBKDF2', false, ['deriveKey']);
      idKeys.set(id, await crypto.subtle.deriveKey(
        { name: 'PBKDF2', hash: 'SHA-256', salt: enc.encode('manydesk-rv-v1'), iterations: 200000 },
        base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'],
      ));
    }
    return idKeys.get(id);
  }

  async function newSessionKey() {
    const raw = crypto.getRandomValues(new Uint8Array(32));
    return { raw: b64(raw), key: await importSessionKey(b64(raw)) };
  }

  function importSessionKey(rawB64) {
    return crypto.subtle.importKey('raw', unb64(rawB64), 'AES-GCM', false, ['encrypt', 'decrypt']);
  }

  async function seal(key, obj) {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, enc.encode(JSON.stringify(obj)));
    const out = new Uint8Array(12 + ct.byteLength);
    out.set(iv);
    out.set(new Uint8Array(ct), 12);
    return b64(out);
  }

  async function open(key, text) {
    try {
      const bytes = unb64(text);
      const pt = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: bytes.slice(0, 12) }, key, bytes.slice(12));
      const obj = JSON.parse(dec.decode(pt));
      if (!obj || typeof obj.t !== 'string') return null;
      // No timestamp check here: the sender's clock may be wrong. Freshness and replays
      // are handled in the transport (network-corrected time, EOSE, event-id dedupe).
      return obj;
    } catch {
      return null; // not for us, or tampered
    }
  }

  async function send(topic, key, obj) {
    return api.rvPublish(topic, await seal(key, { ...obj, ts: Date.now() }));
  }

  window.RV = {
    normalizeId, formatId, myId, newId, topicForId, randomTopic, keyForId,
    newSessionKey, importSessionKey, seal, open, send,
  };
})();
