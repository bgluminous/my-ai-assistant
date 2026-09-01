// 生成一个 512x512 的应用图标源文件（app-icon.png），纯本地、无网络依赖。
// 之后用 `npx @tauri-apps/cli icon app-icon.png` 生成各平台图标。
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const W = 512;
const H = 512;
const buf = Buffer.alloc(W * H * 4);

function setPx(x, y, r, g, b, a) {
  const i = (y * W + x) * 4;
  buf[i] = r;
  buf[i + 1] = g;
  buf[i + 2] = b;
  buf[i + 3] = a;
}

function lerp(a, b, t) {
  return Math.round(a + (b - a) * t);
}

for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    const t = (x + y) / (W + H);
    // 背景：深色对角渐变
    let r = lerp(0x15, 0x22, t);
    let g = lerp(0x18, 0x2c, t);
    let b = lerp(0x20, 0x3a, t);

    // 中心圆形（品牌蓝渐变）
    const dx = x - 256;
    const dy = y - 256;
    const dist = Math.sqrt(dx * dx + dy * dy);
    if (dist < 168) {
      const tt = Math.min(1, dist / 168);
      r = lerp(0x6e, 0x3f, tt);
      g = lerp(0xa8, 0x64, tt);
      b = lerp(0xfe, 0xe0, tt);
    }
    // 内部高光圆
    const dist2 = Math.sqrt((x - 210) * (x - 210) + (y - 200) * (y - 200));
    if (dist2 < 46) {
      r = 0xff;
      g = 0xff;
      b = 0xff;
    }
    setPx(x, y, r, g, b, 255);
  }
}

const raw = Buffer.alloc(H * (1 + W * 4));
for (let y = 0; y < H; y++) {
  raw[y * (1 + W * 4)] = 0;
  buf.copy(raw, y * (1 + W * 4) + 1, y * W * 4, y * W * 4 + W * 4);
}
const idat = deflateSync(raw, { level: 9 });

const crcTable = (() => {
  const table = [];
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(data) {
  let c = 0xffffffff;
  for (const b of data) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0);
ihdr.writeUInt32BE(H, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type RGBA
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const png = Buffer.concat([
  signature,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

writeFileSync(new URL("../app-icon.png", import.meta.url), png);
console.log("wrote app-icon.png", png.length, "bytes");
