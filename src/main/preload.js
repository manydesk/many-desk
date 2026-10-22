const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('md', {
  platform: process.platform,

  permStatus: () => ipcRenderer.invoke('perm:status'),
  requestAccessibility: () => ipcRenderer.invoke('perm:request-accessibility'),
  requestMicrophone: () => ipcRenderer.invoke('perm:microphone'),
  openSettings: (which) => ipcRenderer.invoke('perm:open-settings', which),

  listScreens: () => ipcRenderer.invoke('screens:list'),
  selectSource: (sourceId, displayId) => ipcRenderer.invoke('host:select-source', sourceId, displayId),
  hostSession: (opts) => ipcRenderer.invoke('host:session', opts),
  inject: (msg, who) => ipcRenderer.send('input:event', msg, who),
  forgetPointer: (id) => ipcRenderer.send('input:forget', id),

  watchClipboard: (on) => ipcRenderer.invoke('clipboard:watch', on),
  setClipboard: (text) => ipcRenderer.invoke('clipboard:set', text),
  onClipboard: (fn) => {
    ipcRenderer.removeAllListeners('clipboard:changed');
    ipcRenderer.on('clipboard:changed', (_e, t) => fn(t));
  },

  captureMenu: (on) => ipcRenderer.invoke('menu:capture', on),
  setFullscreen: (on) => ipcRenderer.invoke('win:set-fullscreen', on),
  onFullscreen: (fn) => {
    ipcRenderer.removeAllListeners('win:fullscreen');
    ipcRenderer.on('win:fullscreen', (_e, on) => fn(on));
  },

  appInfo: () => ipcRenderer.invoke('app:info'),
  rvSubscribe: (topic, lookback) => ipcRenderer.invoke('rv:subscribe', topic, lookback),
  rvUnsubscribe: (topic) => ipcRenderer.invoke('rv:unsubscribe', topic),
  rvPublish: (topic, content) => ipcRenderer.invoke('rv:publish', topic, content),
  rvStatus: () => ipcRenderer.invoke('rv:status'),
  cloudflareTurn: (keyId, token) => ipcRenderer.invoke('turn:cloudflare', keyId, token),
  onRv: (fn) => {
    ipcRenderer.removeAllListeners('rv:message');
    ipcRenderer.on('rv:message', (_e, m) => fn(m));
  },
  onRvStatus: (fn) => {
    ipcRenderer.removeAllListeners('rv:status');
    ipcRenderer.on('rv:status', (_e, s) => fn(s));
  },

  fileBegin: (id, name, size) => ipcRenderer.invoke('file:begin', id, name, size),
  fileChunk: (id, buf) => ipcRenderer.invoke('file:chunk', id, buf),
  fileEnd: (id, ok) => ipcRenderer.invoke('file:end', id, ok),
  reveal: (p) => ipcRenderer.invoke('file:reveal', p),
});
