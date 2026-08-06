// 纯 Node 生成 1024x1024 PNG 应用图标（无第三方依赖）：天空蓝 -> 靛蓝渐变圆球 + 高光
const zlib = require("zlib");
const fs = require("fs");
const path = require("path");

const SIZE = 1024;
const buf = Buffer.alloc(SIZE * SIZE * 4);

function lerp(a, b, t) {
  return a + (b - a) * t;
}

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    const i = (y * SIZE + x) * 4;
    const cx = x - SIZE / 2;
    const cy = y - SIZE / 2;
    const r = Math.sqrt(cx * cx + cy * cy);
    const R = SIZE * 0.42;
    if (r < R) {
      const t = r / R;
      // 渐变：sky-400 (56,189,248) -> indigo-600 (79,70,229)
      const rr = lerp(56, 79, t);
      const gg = lerp(189, 70, t);
      const bb = lerp(248, 229, t);
      // 左上角高光
      const hx = x - SIZE * 0.36;
      const hy = y - SIZE * 0.32;
      const hr = Math.sqrt(hx * hx + hy * hy);
      const add = Math.max(0, 1 - hr / (SIZE * 0.22)) * 70;
      // 边缘抗锯齿
      let a = 255;
      if (r > R - 3) a = Math.max(0, (255 * (R - r)) / 3);
      buf[i] = Math.min(255, Math.round(rr + add));
      buf[i + 1] = Math.min(255, Math.round(gg + add));
      buf[i + 2] = Math.min(255, Math.round(bb + add));
      buf[i + 3] = Math.round(a);
    } else {
      buf[i + 3] = 0;
    }
  }
}

function crc32(b) {
  let table = crc32.table;
  if (!table) {
    table = crc32.table = [];
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      table[n] = c >>> 0;
    }
  }
  let c = 0xffffffff;
  for (const byte of b) c = table[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  buf.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
}
const idat = zlib.deflateSync(raw, { level: 9 });
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA

const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

const outDir = path.join(__dirname, "..", "icons");
fs.mkdirSync(outDir, { recursive: true });
const out = path.join(outDir, "app-icon.png");
fs.writeFileSync(out, png);
console.log("icon written:", out, png.length, "bytes");
