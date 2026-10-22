// Host-side overlay: one labelled marker per remote viewer at the point where that
// viewer is moving / clicking, so the host always sees who is doing what.
const markers = new Map();

function marker(c) {
  let m = markers.get(c.id);
  if (!m) {
    const el = document.createElement('div');
    el.className = 'who';
    el.innerHTML = '<div class="ring"></div><div class="tag"><span class="name"></span><small></small><span class="act"></span></div>';
    document.body.appendChild(el);
    m = { el, idle: null, gone: null };
    markers.set(c.id, m);
  }
  m.el.style.setProperty('--c', c.color);
  m.el.querySelector('.name').textContent = c.name;
  m.el.querySelector('small').textContent = c.tag || '';
  return m;
}

window.overlay.onCursor((c) => {
  const m = marker(c);
  if (typeof c.x === 'number') m.el.style.transform = `translate(${Math.round(c.x)}px, ${Math.round(c.y)}px)`;
  // Action words come translated from the host app (its UI language).
  const act = (c.acts && c.acts[c.kind]) || '';
  m.el.querySelector('.act').textContent = act ? `· ${act}` : '';
  if (c.kind === 'down') {
    const r = document.createElement('div');
    r.className = 'ripple';
    m.el.appendChild(r);
    setTimeout(() => r.remove(), 650);
  }
  m.el.classList.remove('idle', 'gone');
  clearTimeout(m.idle);
  clearTimeout(m.gone);
  // Fade when the viewer stops, disappear after a while; it comes back on the next action.
  m.idle = setTimeout(() => { m.el.classList.add('idle'); m.el.querySelector('.act').textContent = ''; }, 2500);
  m.gone = setTimeout(() => m.el.classList.add('gone'), 15000);
});

window.overlay.onForget((id) => {
  const m = markers.get(id);
  if (!m) return;
  m.el.remove();
  markers.delete(id);
});
