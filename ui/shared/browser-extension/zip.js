// Minimal ZIP writer (stored, no compression) for the extension download, so the server
// needs no archive step: the install page fetches the files and zips them in the browser.
// 1980-01-01, the earliest valid ZIP date; a zero date confuses some unzip tools.
const DOS_DATE = (1 << 5) | 1;
const table = Array.from({length: 256}, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

export function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = table[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/** `files`: [{name, bytes: Uint8Array}]. Returns the archive as a Uint8Array. */
export function zip(files) {
  const encoder = new TextEncoder();
  const parts = [], central = [];
  let offset = 0;
  for (const {name, bytes} of files) {
    const path = encoder.encode(name), crc = crc32(bytes);
    const header = new DataView(new ArrayBuffer(30));
    header.setUint32(0, 0x04034b50, true); header.setUint16(4, 20, true); header.setUint16(12, DOS_DATE, true);
    header.setUint32(14, crc, true); header.setUint32(18, bytes.length, true);
    header.setUint32(22, bytes.length, true); header.setUint16(26, path.length, true);
    parts.push(new Uint8Array(header.buffer), path, bytes);
    const entry = new DataView(new ArrayBuffer(46));
    entry.setUint32(0, 0x02014b50, true); entry.setUint16(4, 20, true); entry.setUint16(6, 20, true);
    entry.setUint16(14, DOS_DATE, true);
    entry.setUint32(16, crc, true); entry.setUint32(20, bytes.length, true);
    entry.setUint32(24, bytes.length, true); entry.setUint16(28, path.length, true);
    entry.setUint32(42, offset, true);
    central.push(new Uint8Array(entry.buffer), path);
    offset += 30 + path.length + bytes.length;
  }
  const size = central.reduce((sum, part) => sum + part.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true); end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true); end.setUint32(12, size, true); end.setUint32(16, offset, true);
  const all = [...parts, ...central, new Uint8Array(end.buffer)];
  const out = new Uint8Array(all.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of all) { out.set(part, at); at += part.length; }
  return out;
}
