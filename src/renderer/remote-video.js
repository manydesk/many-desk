// A remote screen that arrives as H.264 access units (MDP, or the Linux WebRTC bridge):
// WebCodecs decodes them (hardware where the WebView has it) into a canvas drawn over
// the session's <video>, which keeps receiving the input events.
(() => {
  class RemoteVideo {
    // requestKey(): ask the sender for a keyframe (the decoder lost sync)
    constructor(requestKey) {
      this.requestKey = requestKey;
      this.native = true;
      this.videoWidth = 0;
      this.videoHeight = 0;
      this.canvas = null;
      this.decoder = null;
      this.bytes = 0;
      this.frames = 0;
      this.times = [];
      this.track = { kind: 'video', stop() {} };
    }

    getTracks() { return [this.track]; }
    getVideoTracks() { return [this.track]; }
    getAudioTracks() { return []; }

    attach(video) {
      let c = video.nextElementSibling;
      if (!c || c.tagName !== 'CANVAS') { c = document.createElement('canvas'); c.className = 'mirror'; video.after(c); }
      this.canvas = c;
      this.ctx = c.getContext('2d', { alpha: false });
      video.__native = this;
    }

    push(key, ts, data) {
      this.bytes += data.byteLength;
      if (!this.decoder || this.decoder.state === 'closed') {
        if (!key) { this.ask(); return; }
        this.decoder = new VideoDecoder({ output: (f) => this.draw(f), error: () => this.reset() });
        this.decoder.configure({ codec: 'avc1.42e01f', optimizeForLatency: true });
      }
      // Behind: skip delta frames rather than build up latency; the next keyframe resyncs.
      if (!key && this.decoder.decodeQueueSize > 2) { this.lagging = true; this.ask(); return; }
      if (this.lagging && !key) return;
      this.lagging = false;
      try {
        this.decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: Math.round((ts * 1000) / 90), data }));
      } catch { this.reset(); }
    }

    ask() {
      const now = performance.now();
      if (now - (this.asked || 0) < 300) return;
      this.asked = now;
      this.requestKey();
    }

    reset() {
      try { this.decoder && this.decoder.close(); } catch {}
      this.decoder = null;
      this.ask();
    }

    draw(f) {
      const w = f.displayWidth;
      const h = f.displayHeight;
      if (this.canvas) {
        if (this.canvas.width !== w || this.canvas.height !== h) { this.canvas.width = w; this.canvas.height = h; }
        this.ctx.drawImage(f, 0, 0);
      }
      f.close();
      this.videoWidth = w;
      this.videoHeight = h;
      this.frames += 1;
      const now = performance.now();
      this.times.push(now);
      while (this.times.length && now - this.times[0] > 2000) this.times.shift();
    }

    get fps() { return this.times.length / 2; }

    stop() {
      try { this.decoder && this.decoder.close(); } catch {}
      this.decoder = null;
    }
  }

  window.RemoteVideo = RemoteVideo;
})();
