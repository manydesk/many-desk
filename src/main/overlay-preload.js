const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('overlay', {
  onCursor: (fn) => ipcRenderer.on('cursor', (_e, c) => fn(c)),
  onForget: (fn) => ipcRenderer.on('forget', (_e, id) => fn(id)),
});
