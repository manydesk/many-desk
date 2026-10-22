// Linux (Tauri on WebKitGTK): the WebView has no WebRTC. This provides the parts of
// RTCPeerConnection that rtc.js uses, backed by webrtc-rs (src-tauri/src/linux/rtc.rs).
// Video is H.264 both ways: the viewer decodes with WebCodecs into a canvas; as host,
// the screen is captured and encoded natively, so getDisplayMedia returns a placeholder.
(() => {
  if (typeof window.RTCPeerConnection === 'function' || !window.__TAURI__) return;
  const { invoke, Channel } = window.__TAURI__.core;
  const tr = (key) => (window.t ? window.t(key) : key);
  let seq = 0;

  const fire = (target, type, init) => {
    const e = Object.assign(new Event(type), init || {});
    target.dispatchEvent(e);
    const h = target[`on${type}`];
    if (typeof h === 'function') h.call(target, e);
  };

  class NativeDataChannel extends EventTarget {
    constructor(pc, label) {
      super();
      this.pc = pc;
      this.label = label;
      this.readyState = 'connecting';
      this.binaryType = 'arraybuffer';
      this.bufferedAmountLowThreshold = 0;
      this.onopen = this.onclose = this.onmessage = this.onbufferedamountlow = null;
      this.pending = 0; // bytes handed to send() but not yet queued in Rust
      this.native = 0; // bytes Rust still has to transmit
      this.chain = Promise.resolve();
    }

    get bufferedAmount() { return this.pending + this.native; }

    open() {
      if (this.readyState === 'open') return;
      this.readyState = 'open';
      fire(this, 'open');
    }

    closed() {
      if (this.readyState === 'closed') return;
      this.readyState = 'closed';
      fire(this, 'close');
    }

    send(data) {
      if (this.readyState !== 'open') throw new DOMException('Data channel is not open', 'InvalidStateError');
      const id = this.pc.id;
      let call;
      let size;
      if (typeof data === 'string') {
        size = data.length;
        call = () => invoke('npc_dc_send', { id, label: this.label, text: data });
      } else {
        const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        size = bytes.byteLength;
        call = () => invoke('npc_dc_send_bin', bytes, { headers: { 'x-id': String(id), 'x-label': this.label } });
      }
      this.pending += size;
      this.chain = this.chain.then(call).then((outstanding) => {
        this.pending -= size;
        this.native = outstanding || 0;
        this.checkLow();
      }, () => { this.pending -= size; });
    }

    checkLow() {
      if (this.bufferedAmount <= this.bufferedAmountLowThreshold) fire(this, 'bufferedamountlow');
    }

    close() {
      invoke('npc_dc_close', { id: this.pc.id, label: this.label }).catch(() => {});
    }
  }

  class NativeSender {
    constructor(pc, track) {
      this.pc = pc;
      this.track = track;
      this.params = { encodings: [{}], degradationPreference: 'balanced' };
    }

    getParameters() { return JSON.parse(JSON.stringify(this.params)); }

    async setParameters(p) {
      this.params = p;
      const e = (p.encodings && p.encodings[0]) || {};
      await invoke('npc_video_params', {
        id: this.pc.id, scale: e.scaleResolutionDownBy || 1, fps: e.maxFramerate || 30, bitrate: e.maxBitrate || 4_000_000,
      });
    }

    async replaceTrack() { throw new Error(tr('mic.noAudio')); }
  }

  // Placeholder for the shared screen; the pixels never pass through the page.
  class NativeScreenTrack extends EventTarget {
    constructor(screen) {
      super();
      this.kind = 'video';
      this.native = true;
      this.contentHint = '';
      this.screen = screen;
      this.readyState = 'live';
    }

    getSettings() { return { width: this.screen.width, height: this.screen.height }; }
    stop() { this.readyState = 'ended'; }
  }

  class NativePeerConnection extends EventTarget {
    constructor(cfg = {}) {
      super();
      this.id = ++seq;
      this.connectionState = 'new';
      this.iceConnectionState = 'new';
      this.iceGatheringState = 'new';
      this.signalingState = 'stable';
      this.localDescription = null;
      this.remoteDescription = null;
      this.channels = new Map();
      this.transceivers = [];
      this.remote = null;
      this.closed = false;
      const events = new Channel();
      events.onmessage = (m) => this.onEvent(m);
      const media = new Channel();
      media.onmessage = (m) => this.onMedia(m);
      this.chain = invoke('npc_create', {
        id: this.id,
        config: { iceServers: cfg.iceServers || [], iceTransportPolicy: cfg.iceTransportPolicy || 'all' },
        events,
        media,
      });
    }

    // Calls run in order, like the browser's operations chain.
    call(cmd, args) {
      const p = this.chain.then(() => invoke(cmd, { id: this.id, ...args }));
      this.chain = p.catch(() => {});
      return p;
    }

    async refreshLocal() {
      const d = await invoke('npc_local', { id: this.id }).catch(() => null);
      if (d && d.sdp) this.localDescription = d;
    }

    async onEvent(m) {
      if (this.closed) return;
      switch (m.e) {
        case 'cand':
          await this.refreshLocal();
          fire(this, 'icecandidate', { candidate: { ...m.c, url: m.url }, url: m.url });
          break;
        case 'cand_err':
          fire(this, 'icecandidateerror', { errorCode: 0, errorText: m.text, url: '' });
          break;
        case 'gather':
          if (m.s === 'complete') await this.refreshLocal();
          this.iceGatheringState = m.s;
          if (m.s === 'complete') fire(this, 'icecandidate', { candidate: null });
          fire(this, 'icegatheringstatechange');
          break;
        case 'ice':
          this.iceConnectionState = m.s;
          fire(this, 'iceconnectionstatechange');
          break;
        case 'conn':
          this.connectionState = m.s;
          fire(this, 'connectionstatechange');
          break;
        case 'dc': {
          const dc = new NativeDataChannel(this, m.label);
          this.channels.set(m.label, dc);
          fire(this, 'datachannel', { channel: dc });
          // A channel the peer created is announced once it is already open.
          setTimeout(() => dc.open(), 0);
          break;
        }
        case 'dc_open': { const dc = this.channels.get(m.label); if (dc) dc.open(); break; }
        case 'dc_close': { const dc = this.channels.get(m.label); if (dc) dc.closed(); break; }
        case 'dc_low': { const dc = this.channels.get(m.label); if (dc) { dc.native = 0; dc.checkLow(); } break; }
        case 'dc_msg': { const dc = this.channels.get(m.label); if (dc) fire(dc, 'message', { data: m.text }); break; }
        case 'track':
          this.remote = new window.RemoteVideo(() => invoke('npc_pli', { id: this.id }).catch(() => {}));
          fire(this, 'track', { track: this.remote.track, streams: [this.remote], receiver: {} });
          break;
        default:
      }
    }

    onMedia(m) {
      const buf = m instanceof ArrayBuffer ? new Uint8Array(m) : new Uint8Array(m);
      if (buf[0] === 1) {
        const ts = new DataView(buf.buffer, buf.byteOffset + 2, 4).getUint32(0);
        if (this.remote) this.remote.push(buf[1] === 1, ts, buf.subarray(6));
      } else if (buf[0] === 2) {
        const len = buf[1];
        const label = new TextDecoder().decode(buf.subarray(2, 2 + len));
        const dc = this.channels.get(label);
        if (dc) fire(dc, 'message', { data: buf.slice(2 + len).buffer });
      }
    }

    createDataChannel(label) {
      const dc = new NativeDataChannel(this, label);
      this.channels.set(label, dc);
      this.call('npc_dc_create', { label }).catch(() => {});
      return dc;
    }

    addTrack(track) {
      const sender = new NativeSender(this, track);
      this.transceivers.push({ sender, receiver: { track: { kind: 'video' } }, direction: 'sendonly', setCodecPreferences() {} });
      this.call('npc_add_video', { screen: track.screen ? track.screen.id : null }).catch(() => {});
      return sender;
    }

    // Audio is negotiated (the peer expects the m-line) but not carried on Linux yet.
    addTransceiver(kind, init = {}) {
      const t = this.audioTransceiver(init.direction || 'sendrecv');
      this.call('npc_add_audio').catch(() => {});
      return t;
    }

    audioTransceiver(direction) {
      const t = { sender: new NativeSender(this, null), receiver: { track: { kind: 'audio' } }, direction, setCodecPreferences() {} };
      this.transceivers.push(t);
      return t;
    }

    getTransceivers() { return this.transceivers; }

    async createOffer() { return { type: 'offer', sdp: await this.call('npc_offer') }; }
    async createAnswer() { return { type: 'answer', sdp: await this.call('npc_answer') }; }

    async setLocalDescription(d) {
      if (!d) d = this.remoteDescription && this.remoteDescription.type === 'offer' ? await this.createAnswer() : await this.createOffer();
      await this.call('npc_set_local', { kind: d.type, sdp: d.sdp });
      this.localDescription = d;
      if (this.iceGatheringState === 'new') this.iceGatheringState = 'gathering';
      await this.refreshLocal();
    }

    async setRemoteDescription(d) {
      await this.call('npc_set_remote', { kind: d.type, sdp: d.sdp });
      this.remoteDescription = d;
      if (d.type === 'offer' && /^m=audio/m.test(d.sdp) && !this.transceivers.some((t) => t.receiver.track.kind === 'audio')) {
        this.audioTransceiver('recvonly');
      }
    }

    addIceCandidate(c) {
      if (!c || !c.candidate) return Promise.resolve();
      return this.call('npc_add_candidate', { candidate: c.candidate, mid: c.sdpMid == null ? null : String(c.sdpMid) });
    }

    async getStats() {
      const list = await this.call('npc_stats').catch(() => []);
      const map = new Map();
      let enc = null;
      for (const s of list) {
        if (s.type === 'x-encoder') { enc = s; continue; }
        if (s.id) map.set(s.id, s);
      }
      // webrtc-rs names candidates "RTCLocalIceCandidate_<id>" but pairs refer to "<id>",
      // and it does not list remote candidates: link what exists, stub the rest.
      for (const p of [...map.values()].filter((x) => x.type === 'candidate-pair')) {
        const local = [...map.values()].find((x) => x.type === 'local-candidate' && x.id.endsWith(p.localCandidateId));
        if (local) p.localCandidateId = local.id;
        if (!map.has(p.remoteCandidateId)) map.set(p.remoteCandidateId, { id: p.remoteCandidateId, type: 'remote-candidate', candidateType: '?' });
      }
      const codec = { id: 'x-codec', type: 'codec', mimeType: 'video/H264' };
      map.set(codec.id, codec);
      const find = (type) => [...map.values()].find((s) => s.type === type && (s.kind || 'video') === 'video');
      if (this.remote) {
        const r = this.remote;
        const inb = find('inbound-rtp') || { id: 'x-in', type: 'inbound-rtp', kind: 'video' };
        Object.assign(inb, {
          kind: 'video', codecId: codec.id, framesPerSecond: r.fps, frameWidth: r.videoWidth, frameHeight: r.videoHeight,
          framesDecoded: r.frames, decoderImplementation: 'WebCodecs', bytesReceived: inb.bytesReceived || r.bytes,
        });
        map.set(inb.id, inb);
      }
      if (enc) {
        const out = find('outbound-rtp') || { id: 'x-out', type: 'outbound-rtp', kind: 'video', bytesSent: 0 };
        Object.assign(out, {
          kind: 'video', codecId: codec.id, framesPerSecond: enc.framesPerSecond, frameWidth: enc.frameWidth, frameHeight: enc.frameHeight,
          framesEncoded: enc.framesEncoded, encoderImplementation: enc.encoderImplementation, powerEfficientEncoder: false,
          qualityLimitationReason: 'none',
        });
        map.set(out.id, out);
      }
      return map;
    }

    close() {
      if (this.closed) return;
      this.closed = true;
      if (this.remote) this.remote.stop();
      this.connectionState = 'closed';
      invoke('npc_close', { id: this.id }).catch(() => {});
    }
  }

  window.RTCPeerConnection = NativePeerConnection;
  window.RTCRtpReceiver = window.RTCRtpReceiver || { getCapabilities: () => null };

  // Host: the screen picked in the app's own list (md.selectSource), captured natively.
  let picked = null;
  window.__nativeScreens = {
    list: () => invoke('screens_list').catch(() => []),
    select: (screen) => { picked = screen; },
  };
  const nativeDisplayMedia = async () => {
    let screen = picked;
    if (!screen) screen = (await window.__nativeScreens.list())[0];
    if (!screen) throw new Error(tr('err.noScreen'));
    const track = new NativeScreenTrack(screen);
    invoke('host_set_display', { width: screen.width, height: screen.height }).catch(() => {});
    return { getVideoTracks: () => [track], getAudioTracks: () => [], getTracks: () => [track] };
  };
  // WebKit's own getDisplayMedia lives on the prototype and wants a user gesture.
  if (window.MediaDevices) {
    Object.defineProperty(MediaDevices.prototype, 'getDisplayMedia', { value: nativeDisplayMedia, configurable: true, writable: true });
  }
})();
