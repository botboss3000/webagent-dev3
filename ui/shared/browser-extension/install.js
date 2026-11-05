import {zip} from './zip.js';

// Everything the unpacked extension needs; the install page and zip helper stay behind.
const FILES = ['manifest.json', 'background.js', 'link.js', 'overlay.js', 'popup.html', 'popup.js'];

const button = document.getElementById('download'), status = document.getElementById('status');
button.addEventListener('click', async () => {
  button.disabled = true;
  status.textContent = 'Preparing…';
  try {
    const files = await Promise.all(FILES.map(async name => {
      const response = await fetch(new URL(name, import.meta.url), {cache: 'no-store'});
      if (!response.ok) throw new Error(name + ': ' + response.status);
      return {name: 'webagent-browser-extension/' + name, bytes: new Uint8Array(await response.arrayBuffer())};
    }));
    const link = document.createElement('a');
    link.href = URL.createObjectURL(new Blob([zip(files)], {type: 'application/zip'}));
    link.download = 'webagent-browser-extension.zip';
    link.click();
    setTimeout(() => URL.revokeObjectURL(link.href), 10000);
    status.textContent = 'Downloaded.';
  } catch (error) {
    status.textContent = 'Download failed: ' + error.message;
  } finally {
    button.disabled = false;
  }
});
