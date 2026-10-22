// MDP sessions: the same interface as the WebRTC Session in rtc.js, but transport,
// capture and encoding run natively (src-tauri/src/mdp, src-tauri/src/media). Used when
// both peers are current Tauri builds; anything older is reached over WebRTC.
(() => {
  if (!window.__TAURI__ || !window.RTC) return;
  const { invoke, Channel } = window.__TAURI__.core;
  const { Session, encodeCode, decodeCode, codeFromFingerprints, stunServers } = window.RTC;
  let seq = 0;

  const REASON = { bye: 'peer.left', lost: 'conn.lost', noroute: 'conn.noRoute' };
  const RATE = 48000;
  const FRAME = 960; // 20 ms

  // Voice over MDP: Opus (WebCodecs) in QUIC datagrams, both directions.
  class Voice {
    constructor(id) {
      this.id = id;
      this.ctx = null;
    }

    async context() {
      if (!this.ctx) {
        this.ctx = new AudioContext({ sampleRate: RATE, latencyHint: 'interactive' });
        await this.ctx.audioWorklet.addModule('audio-worklet.js');
      }
      if (this.ctx.state === 'suspended') this.ctx.resume().catch(() => {});
      return this.ctx;
    }

    // Remote voice → a MediaStream for the session's <audio> element (which handles
    // muting background tabs, like the WebRTC path).
    async output() {
      const ctx = await this.context();
      this.play = new AudioWorkletNode(ctx, 'md-play', { numberOfInputs: 0, outputChannelCount: [1] });
      const dest = ctx.createMediaStreamDestination();
      this.play.connect(dest);
      this.decoder = new AudioDecoder({
        output: (a) => {
          const pcm = new Float32Array(a.numberOfFrames);
          a.copyTo(pcm, { planeIndex: 0, format: 'f32-planar' });
          a.close();
          this.play.port.postMessage(pcm, [pcm.buffer]);
        },
        error: () => { this.decoder = null; },
      });
      this.decoder.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1 });
      return dest.stream;
    }

    receive(bytes) {
      if (!this.decoder || this.decoder.state !== 'configured') return;
      this.ts = (this.ts || 0) + 20000;
      try { this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: this.ts, data: bytes })); } catch {}
    }

    // Microphone → Opus → datagrams. Returns a handle with `enabled` / `stop()`.
    async startMic() {
      const media = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      const ctx = await this.context();
      const src = ctx.createMediaStreamSource(media);
      const cap = new AudioWorkletNode(ctx, 'md-capture', { numberOfOutputs: 0 });
      src.connect(cap);
      const id = this.id;
      const enc = new AudioEncoder({
        output: (chunk) => {
          const b = new Uint8Array(chunk.byteLength);
          chunk.copyTo(b);
          invoke('mdp_audio', b, { headers: { 'x-id': String(id) } }).catch(() => {});
        },
        error: () => {},
      });
      enc.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1, bitrate: 32000, opus: { frameDuration: 20000, usedtx: true } });
      const handle = {
        enabled: true,
        stop() {
          cap.port.onmessage = null;
          src.disconnect();
          media.getTracks().forEach((t) => t.stop());
          try { enc.close(); } catch {}
        },
      };
      let pend = new Float32Array(0);
      let ts = 0;
      cap.port.onmessage = (e) => {
        if (!handle.enabled || enc.state !== 'configured') return;
        const next = new Float32Array(pend.length + e.data.length);
        next.set(pend);
        next.set(e.data, pend.length);
        pend = next;
        while (pend.length >= FRAME) {
          const frame = pend.slice(0, FRAME);
          pend = pend.slice(FRAME);
          enc.encode(new AudioData({ format: 'f32-planar', sampleRate: RATE, numberOfFrames: FRAME, numberOfChannels: 1, timestamp: ts, data: frame }));
          ts += 20000;
        }
      };
      return handle;
    }

    close() {
      try { this.decoder && this.decoder.close(); } catch {}
      if (this.ctx) this.ctx.close().catch(() => {});
    }
  }

  // Looks like an RTCDataChannel to Session.sendFile (bufferedAmount + bufferedamountlow).
  class FileChannel extends EventTarget {
    constructor(id) {
      super();
      this.id = id;
      this.readyState = 'open';
      this.pending = 0;
      this.bufferedAmountLowThreshold = 1 << 20;
      this.chain = Promise.resolve();
    }

    get bufferedAmount() { return this.pending; }

    send(data) {
      let size;
      let call;
      if (typeof data === 'string') {
        size = data.length;
        call = () => invoke('mdp_send', { id: this.id, chan: 'file', text: data });
      } else {
        const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        size = bytes.byteLength;
        call = () => invoke('mdp_send_bin', bytes, { headers: { 'x-id': String(this.id) } });
      }
      this.pending += size;
      this.chain = this.chain.then(call).catch(() => {}).then(() => {
        this.pending -= size;
        if (this.pending <= this.bufferedAmountLowThreshold) this.dispatchEvent(new Event('bufferedamountlow'));
      });
    }
  }

  class NativeSession extends Session {
    // opts.screen: the monitor id to share (host)
    constructor(role, opts = {}) {
      super(role, true);
      this.native = true;
      this.mid = ++seq;
      this.screen = opts.screen;
      this.ctlChain = Promise.resolve();
      // Session's auto-quality loop and connection report read these.
      this.pc = { getStats: () => this.rtcStats(), close() {} };
    }

    async prepare() {
      const events = new Channel();
      events.onmessage = (m) => this.onEvent(m);
      const media = new Channel();
      media.onmessage = (m) => this.onMedia(m);
      const p = await invoke('mdp_prepare', { id: this.mid, host: this.role === 'host', stun: stunServers(), events, media });
      this.log(`MDP candidates: ${p.cands.join(', ') || 'none'}`);
      this.fp = p.fp;
      return p;
    }

    // Host: native capture of `opts.screen`; the code carries our addresses and certificate.
    async createInvite() {
      const p = await this.prepare();
      const size = await invoke('mdp_add_video', { id: this.mid, screen: this.screen ?? null });
      const id = this.mid;
      this.videoSender = {
        track: { getSettings: () => size },
        params: { encodings: [{}] },
        getParameters() { return JSON.parse(JSON.stringify(this.params)); },
        async setParameters(q) {
          this.params = q;
          const e = (q.encodings && q.encodings[0]) || {};
          await invoke('mdp_video_params', { id, scale: e.scaleResolutionDownBy || 1, fps: e.maxFramerate || 30, bitrate: e.maxBitrate || 4_000_000 });
        },
      };
      this.setQuality('auto');
      this.autoTimer = setInterval(() => this.autoTick(), 2000);
      return encodeCode({ v: 1, t: 'offer', mdp: { cands: p.cands, fp: p.fp, token: p.token } });
    }

    // Viewer: start punching right away; the host joins when our answer reaches it.
    async answerInvite(code) {
      const obj = await decodeCode(code);
      if (obj.t !== 'offer' || !obj.mdp) throw new Error('Not an invite code');
      const p = await this.prepare();
      await invoke('mdp_connect', { id: this.mid, peer: obj.mdp });
      this.sas = await codeFromFingerprints(obj.mdp.fp, p.fp);
      return { code: await encodeCode({ v: 1, t: 'answer', mdp: { cands: p.cands, fp: p.fp, token: '' } }), sas: this.sas };
    }

    async acceptAnswer(code) {
      const obj = await decodeCode(code);
      if (obj.t !== 'answer' || !obj.mdp) throw new Error('Not an answer code');
      await invoke('mdp_connect', { id: this.mid, peer: obj.mdp });
      this.sas = await codeFromFingerprints(this.fp, obj.mdp.fp);
      return this.sas;
    }

    onEvent(m) {
      if (m.e !== 'ctl' && m.e !== 'file') invoke('debug_log', { msg: `mdp ${this.role} ${JSON.stringify(m)}` }).catch(() => {});
      if (this.closed) return;
      if (m.e === 'open') {
        this.connected = true;
        this.log(`MDP connected via ${m.path}${m.lan ? ' (LAN)' : ''}`);
        this.file = new FileChannel(this.mid);
        this.voice = new Voice(this.mid);
        this.voice.output().then((stream) => this.emit('audio', stream)).catch(() => {});
        if (this.role === 'viewer') {
          this.remote = new window.RemoteVideo(() => invoke('mdp_keyframe', { id: this.mid }).catch(() => {}));
          this.emit('track', this.remote);
        }
        this.emit('open');
      } else if (m.e === 'ctl') {
        try { this.emit('message', JSON.parse(m.text)); } catch {}
      } else if (m.e === 'file') {
        this.onFileMessage(m.text);
      } else if (m.e === 'closed') {
        this.close(REASON[m.reason] ? t(REASON[m.reason]) : t('conn.failed', { e: m.reason }));
      }
    }

    onMedia(m) {
      const buf = new Uint8Array(m);
      if (buf[0] === 1) {
        const ts = new DataView(buf.buffer, buf.byteOffset + 2, 4).getUint32(0);
        if (this.remote) this.remote.push(buf[1] === 1, ts, buf.subarray(6));
      } else if (buf[0] === 2) {
        this.onFileMessage(buf.slice(2 + buf[1]).buffer);
      } else if (buf[0] === 3) {
        if (this.voice) this.voice.receive(buf.subarray(1));
      }
    }

    send(msg) {
      if (!this.connected) return;
      const text = JSON.stringify(msg);
      this.ctlChain = this.ctlChain.then(() => invoke('mdp_send', { id: this.mid, chan: 'ctl', text })).catch(() => {});
    }

    async setMic(on) {
      if (!this.voice) throw new Error(t('mic.noAudio'));
      if (on && !this.mic) this.mic = await this.voice.startMic();
      else if (!on && this.mic) { this.mic.stop(); this.mic = null; }
      this.send({ t: 'mic', on: !!this.mic });
      return !!this.mic;
    }

    // RTCStatsReport-shaped view for Session.autoTick / pairReport.
    async rtcStats() {
      const r = await invoke('mdp_stats', { id: this.mid }).catch(() => ({}));
      const map = new Map();
      map.set('pair', {
        id: 'pair', type: 'candidate-pair', nominated: true, state: r.rtt != null ? 'succeeded' : 'in-progress',
        currentRoundTripTime: (r.rtt || 0) / 1000,
        availableOutgoingBitrate: r.rtt ? (r.cwnd * 8) / (r.rtt / 1000) : undefined,
      });
      if (r.video) map.set('out', { id: 'out', type: 'outbound-rtp', kind: 'video', ...r.video });
      return map;
    }

    async stats() {
      const r = await invoke('mdp_stats', { id: this.mid });
      const v = r.video || {};
      const rv = this.remote;
      const host = this.role === 'host';
      const dir = (host ? r.sent : r.recv) || 0;
      const now = performance.now();
      let kbps = 0;
      if (this.lastBytes !== undefined) kbps = ((dir - this.lastBytes) * 8) / (now - this.lastTime);
      this.lastBytes = dir;
      this.lastTime = now;
      const w = host ? v.frameWidth : rv && rv.videoWidth;
      const h = host ? v.frameHeight : rv && rv.videoHeight;
      return {
        route: 'MDP',
        lan: !!r.lan,
        relay: false,
        rtt: r.rtt != null ? Math.round(r.rtt) : null,
        fps: Math.round(host ? v.framesPerSecond || 0 : rv ? rv.fps : 0),
        kbps: Math.round(kbps),
        size: w ? `${w}×${h}` : '',
        codec: 'H264',
        hw: false,
        impl: host ? v.encoderImplementation || '' : 'WebCodecs',
        limit: v.qualityLimitationReason || '',
        totalMB: ((r.sent || 0) + (r.recv || 0)) / 1048576,
      };
    }

    close(reason = '') {
      if (this.closed) return;
      super.close(reason); // sends 'bye' over the control stream first
      if (this.remote) this.remote.stop();
      if (this.voice) this.voice.close();
      this.ctlChain.finally(() => invoke('mdp_close', { id: this.mid }).catch(() => {}));
    }
  }

  window.RTC.NativeSession = NativeSession;
  // Is this invite/answer code for the native protocol?
  window.RTC.isNativeCode = async (code) => !!(await decodeCode(code).catch(() => ({}))).mdp;
})();
