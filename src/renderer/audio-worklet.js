// Audio worklets for native (MDP) voice: 48 kHz mono.
//  md-capture: forwards microphone samples to the page (encoded there with Opus).
//  md-play:    plays decoded samples from a small jitter buffer.

class Capture extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) this.port.postMessage(ch.slice(0));
    return true;
  }
}

// Starts once ~60 ms are buffered; above ~200 ms it drops the oldest audio, so the
// conversation stays close to real time even after a network hiccup.
class Play extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(48000);
    this.read = 0;
    this.size = 0;
    this.started = false;
    this.port.onmessage = (e) => this.push(e.data);
  }

  push(samples) {
    const cap = this.buf.length;
    for (let i = 0; i < samples.length; i++) {
      this.buf[(this.read + this.size) % cap] = samples[i];
      if (this.size < cap) this.size++;
      else this.read = (this.read + 1) % cap;
    }
    const max = 9600; // 200 ms
    if (this.size > max) {
      this.read = (this.read + this.size - max) % cap;
      this.size = max;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0][0];
    if (!this.started && this.size >= 2880) this.started = true; // 60 ms
    if (!this.started) return true;
    const cap = this.buf.length;
    for (let i = 0; i < out.length; i++) {
      if (this.size > 0) {
        out[i] = this.buf[this.read];
        this.read = (this.read + 1) % cap;
        this.size--;
      } else {
        out[i] = 0;
        this.started = false; // ran dry: refill before playing again
      }
    }
    return true;
  }
}

registerProcessor('md-capture', Capture);
registerProcessor('md-play', Play);
