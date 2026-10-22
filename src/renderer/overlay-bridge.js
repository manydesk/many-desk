// Tauri: provide the overlay API (Electron gets it from src/main/overlay-preload.js).
(() => {
  if (window.overlay || !window.__TAURI__) return;
  const { listen } = window.__TAURI__.event;
  const { invoke } = window.__TAURI__.core;
  window.overlay = {
    onCursor: (fn) => listen('cursor', (e) => fn(e.payload)),
    onForget: (fn) => listen('forget', (e) => fn(e.payload)),
  };
  // Tell the host its queued messages can be delivered now.
  setTimeout(() => invoke('overlay_ready'), 0);
})();
