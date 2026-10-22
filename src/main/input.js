// Bridge between remote-control messages and the native input helper process.
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const { app, screen } = require('electron');

const isWin = process.platform === 'win32';
const keymap = isWin ? require('./keymap-win') : require('./keymap');

function helperPath() {
  const name = isWin ? 'manydesk-input.exe' : 'manydesk-input';
  const packaged = path.join(process.resourcesPath, 'bin', name);
  if (app.isPackaged && fs.existsSync(packaged)) return packaged;
  return path.join(__dirname, '..', '..', 'build', isWin ? 'bin-win' : 'bin', name);
}

class InputInjector {
  constructor() {
    this.proc = null;
    this.displayId = null;
  }

  start() {
    if (this.proc) return;
    const bin = helperPath();
    if (!fs.existsSync(bin)) {
      console.error('[input] helper missing at', bin, '- run `npm run build:helper`');
      return;
    }
    this.proc = spawn(bin, [], { stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true });
    this.proc.stdout.on('data', (d) => console.log('[input]', d.toString().trim()));
    this.proc.on('exit', () => { this.proc = null; });
  }

  stop() {
    if (!this.proc) return;
    this.send({ t: 'release' });
    this.proc.stdin.end();
    this.proc = null;
  }

  setDisplay(displayId) {
    this.displayId = displayId ? String(displayId) : null;
  }

  display() {
    const all = screen.getAllDisplays();
    return all.find((d) => String(d.id) === this.displayId) || screen.getPrimaryDisplay();
  }

  send(cmd) {
    if (this.proc && this.proc.stdin.writable) this.proc.stdin.write(JSON.stringify(cmd) + '\n');
  }

  // msg comes from the remote viewer; x/y are normalized (0..1) within the shared display.
  // Returns the pointer position in display-local DIPs for move/down/up (for the overlay).
  handle(msg) {
    if (!msg || typeof msg.t !== 'string') return null;
    const { bounds } = this.display();
    const local = () => ({
      x: Math.min(1, Math.max(0, Number(msg.x) || 0)) * bounds.width,
      y: Math.min(1, Math.max(0, Number(msg.y) || 0)) * bounds.height,
    });
    const toPoint = (l) => {
      const p = { x: bounds.x + l.x, y: bounds.y + l.y };
      // macOS CGEvent uses points (= Electron DIPs); Windows SetCursorPos needs physical pixels.
      return isWin ? screen.dipToScreenPoint(p) : p;
    };
    switch (msg.t) {
      case 'move': {
        const l = local();
        this.send({ t: 'move', ...toPoint(l) });
        return l;
      }
      case 'down':
      case 'up': {
        const l = local();
        this.send({ t: msg.t, b: [0, 1, 2].includes(msg.b) ? msg.b : 0, c: Number(msg.c) || 1, ...toPoint(l) });
        return l;
      }
      case 'scroll':
        this.send({ t: 'scroll', dx: clamp(msg.dx), dy: clamp(msg.dy) });
        break;
      case 'key': {
        const k = keymap[msg.code];
        if (k === undefined) break;
        if (isWin) this.send({ t: 'key', k: k[0], e: !!k[1], d: !!msg.d });
        else this.send({ t: 'key', k, d: !!msg.d });
        break;
      }
      case 'release':
        this.send({ t: 'release' });
        break;
    }
    return null;
  }
}

function clamp(v) {
  const n = Number(v) || 0;
  return Math.max(-2000, Math.min(2000, n));
}

module.exports = { InputInjector };
