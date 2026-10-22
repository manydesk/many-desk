// Minimal MQTT 3.1.1 client over WebSocket (QoS 0 only), used as a second, independent
// rendezvous network next to Nostr. Public brokers forward live messages to current
// subscribers and keep nothing (no retain), so handshake messages are not stored.
// Payloads are the same signed events as on Nostr and are verified by the caller.
const crypto = require('crypto');

function encodeLength(n) {
  const out = [];
  do {
    let b = n % 128;
    n = Math.floor(n / 128);
    if (n > 0) b |= 0x80;
    out.push(b);
  } while (n > 0);
  return Buffer.from(out);
}

function str(s) {
  const b = Buffer.from(s, 'utf8');
  const len = Buffer.alloc(2);
  len.writeUInt16BE(b.length);
  return Buffer.concat([len, b]);
}

function packet(type, body) {
  return Buffer.concat([Buffer.from([type]), encodeLength(body.length), body]);
}

class MqttConnection {
  constructor(url, { onOpen, onClose, onMessage }) {
    this.url = url;
    this.onOpen = onOpen;
    this.onClose = onClose;
    this.onMessage = onMessage;
    this.ws = null;
    this.ready = false;
    this.buf = Buffer.alloc(0);
    this.packetId = 1;
    this.ping = null;
  }

  connect() {
    let ws;
    try { ws = new WebSocket(this.url, ['mqtt']); } catch { return this.onClose(); }
    ws.binaryType = 'arraybuffer';
    this.ws = ws;
    ws.onopen = () => {
      const clientId = 'md' + crypto.randomBytes(8).toString('hex');
      const varHeader = Buffer.concat([str('MQTT'), Buffer.from([4, 0x02, 0, 60])]); // v3.1.1, clean session, 60 s keep-alive
      ws.send(packet(0x10, Buffer.concat([varHeader, str(clientId)])));
    };
    ws.onmessage = (m) => this.onData(Buffer.from(m.data));
    ws.onclose = () => {
      clearInterval(this.ping);
      const was = this.ready;
      this.ready = false;
      this.onClose(was);
    };
    ws.onerror = () => {};
  }

  close() {
    clearInterval(this.ping);
    try { this.ws && this.ws.close(); } catch {}
  }

  send(buf) {
    if (this.ready && this.ws.readyState === WebSocket.OPEN) this.ws.send(buf);
  }

  subscribe(topic) {
    const id = Buffer.alloc(2);
    id.writeUInt16BE(this.nextId());
    this.send(packet(0x82, Buffer.concat([id, str(topic), Buffer.from([0])])));
  }

  unsubscribe(topic) {
    const id = Buffer.alloc(2);
    id.writeUInt16BE(this.nextId());
    this.send(packet(0xa2, Buffer.concat([id, str(topic)])));
  }

  publish(topic, payload) {
    if (!this.ready) return false;
    this.send(packet(0x30, Buffer.concat([str(topic), Buffer.from(payload, 'utf8')])));
    return true;
  }

  nextId() {
    this.packetId = (this.packetId % 65535) + 1;
    return this.packetId;
  }

  // WebSocket frames may carry partial or several MQTT packets.
  onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    for (;;) {
      if (this.buf.length < 2) return;
      let len = 0;
      let mult = 1;
      let i = 1;
      for (;; i++) {
        if (i >= this.buf.length) return;
        const b = this.buf[i];
        len += (b & 0x7f) * mult;
        mult *= 128;
        if (!(b & 0x80)) break;
        if (i > 4) return this.close();
      }
      const start = i + 1;
      if (this.buf.length < start + len) return;
      const type = this.buf[0];
      const body = this.buf.subarray(start, start + len);
      this.buf = this.buf.subarray(start + len);
      this.onPacket(type, body);
    }
  }

  onPacket(type, body) {
    const kind = type >> 4;
    if (kind === 2) { // CONNACK
      if (body[1] !== 0) return this.close();
      this.ready = true;
      this.ping = setInterval(() => this.send(Buffer.from([0xc0, 0])), 45000);
      this.onOpen();
    } else if (kind === 3) { // PUBLISH
      const qos = (type >> 1) & 3;
      const tlen = body.readUInt16BE(0);
      const topic = body.subarray(2, 2 + tlen).toString('utf8');
      const payload = body.subarray(2 + tlen + (qos ? 2 : 0)).toString('utf8');
      this.onMessage(topic, payload);
    }
  }
}

module.exports = { MqttConnection };
