#!/usr/bin/env node
/**
 * 生成标签页图标：`desktop-tauri/ui/favicon.ico`（16/32/48 三尺寸）与
 * `desktop-tauri/ui/favicon.svg`（现代浏览器优先取的高清版）。
 *
 * 为什么不直接复用 build/make-icon.mjs：那个脚本画的是**应用图标**（双向箭头，
 * 1024×1024，交给 `tauri icon` 派生各尺寸），本脚本画的是**标签页的字母标**，
 * 两者的形状与输出格式都不同。PNG 编码与 SDF 图元两套脚本各留一份 —— 构建脚本
 * 本就各自独立，抽取共享模块反而让「改一个图标」有连带弄坏另一个的风险。
 * （注释里写明出处，改 SDF 时记得两处对照。）
 *
 * 设计：圆角方块品牌蓝底 + 白色字母 A（AIapi 的首字母）。
 * 底色取令牌主色 #2563eb 而非 make-icon.mjs 里的 #007AFF —— 后者是迁移前的
 * 旧品牌色（2.9.1 已整体换成 #2563eb），新图标不该再把旧色带回来。
 *
 * 不依赖任何图形库：手写 SDF 光栅 + 3×3 超采样抗锯齿 + 手写 PNG / ICO 编码，
 * 与 make-icon.mjs 同一手法。
 *
 * 用法：node build/make-favicon.mjs
 */

import { deflateSync } from 'node:zlib';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const UI_DIR = join(HERE, '..', 'desktop-tauri', 'ui');
const ICO_OUT = join(UI_DIR, 'favicon.ico');
const SVG_OUT = join(UI_DIR, 'favicon.svg');

/** 品牌主色（tokens.css 的 --primary；见 ui-kit/styles/theme.css） */
const BRAND = '#2563eb';

// ─── PNG 编码（与 make-icon.mjs 同源）─────────────────────────

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buffer) {
  let c = 0xffffffff;
  for (let i = 0; i < buffer.length; i++) c = CRC_TABLE[(c ^ buffer[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const typeBuf = Buffer.from(type, 'latin1');
  const body = Buffer.concat([typeBuf, data]);
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(crc32(body), 8 + data.length);
  return out;
}

function encodePng(width, height, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;   // bit depth
  ihdr[9] = 6;   // color type: RGBA
  // 每行前加一个 filter byte（0 = None）
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0;
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ─── SDF 图元（与 make-icon.mjs 同源）────────────────────────

function segmentDistance(px, py, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;
  let t = lenSq === 0 ? 0 : ((px - ax) * dx + (py - ay) * dy) / lenSq;
  t = Math.max(0, Math.min(1, t));
  const cx = ax + t * dx;
  const cy = ay + t * dy;
  return Math.hypot(px - cx, py - cy);
}

function sdRoundedRect(px, py, halfW, halfH, radius) {
  const qx = Math.abs(px) - (halfW - radius);
  const qy = Math.abs(py) - (halfH - radius);
  const outside = Math.hypot(Math.max(qx, 0), Math.max(qy, 0));
  return outside + Math.min(Math.max(qx, qy), 0) - radius;
}

/** 胶囊体：线段加圆头端点 —— 字母 A 的三笔全靠它 */
function sdCapsule(px, py, ax, ay, bx, by, thickness) {
  return segmentDistance(px, py, ax, ay, bx, by) - thickness;
}

// ─── 形状定义 ──────────────────────────────────────────────

/** 圆角方块的圆角比例（与应用图标一致，两个图形轮廓同族） */
const RECT_RADIUS = 0.225;

/** 字母 A 的笔画：坐标归一化到 [-0.5, 0.5]，y 轴向下 */
const A_STROKES = (() => {
  const TIP_Y = -0.30;      // 尖顶
  const FOOT_Y = 0.27;      // 两脚落地
  const FOOT_X = 0.21;      // 两脚横向张开幅度
  const BAR_Y = 0.09;       // 横杠（偏下 2/3 处，标准 A 的位置）
  const BAR_X = 0.135;      // 横杠半长，比两脚窄，才像 A 而不是 Π
  return [
    [0, TIP_Y, -FOOT_X, FOOT_Y],  // 左腿
    [0, TIP_Y, FOOT_X, FOOT_Y],   // 右腿
    [-BAR_X, BAR_Y, BAR_X, BAR_Y], // 横杠
  ];
})();

/** 字母笔画粗细：16px 上约 1.15px、48px 上约 3.5px，小尺寸仍可辨认 */
const A_THICKNESS = 0.072;

/** 底与字母各自的有向距离场 */
function shapeAlpha(nx, ny) {
  const rect = sdRoundedRect(nx, ny, 0.5, 0.5, RECT_RADIUS);
  let letter = Infinity;
  for (const [ax, ay, bx, by] of A_STROKES) {
    letter = Math.min(letter, sdCapsule(nx, ny, ax, ay, bx, by, A_THICKNESS));
  }
  return { rect, letter };
}

// ─── 光栅化（3×3 超采样）───────────────────────────────────

const SS = 3;

/** #2563eb → 分量 */
const BRAND_RGB = [0x25, 0x63, 0xeb];

function rasterize(size) {
  const pixels = Buffer.alloc(size * size * 4);
  const lerp = (a, b, t) => a + (b - a) * t;

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let rectHits = 0;
      let letterHits = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const px = (x + (sx + 0.5) / SS) / size - 0.5;
          const py = (y + (sy + 0.5) / SS) / size - 0.5;
          const { rect, letter } = shapeAlpha(px, py);
          if (rect <= 0) rectHits++;
          if (letter <= 0) letterHits++;
        }
      }
      const total = SS * SS;
      const rectA = rectHits / total;
      const letterA = letterHits / total;
      if (rectA === 0) continue; // 圆角外的透明像素，保持 0

      let [r, g, b] = BRAND_RGB;
      if (letterA > 0) {
        r = lerp(r, 255, letterA);
        g = lerp(g, 255, letterA);
        b = lerp(b, 255, letterA);
      }
      const i = (y * size + x) * 4;
      pixels[i] = Math.round(r);
      pixels[i + 1] = Math.round(g);
      pixels[i + 2] = Math.round(b);
      pixels[i + 3] = Math.round(rectA * 255);
    }
  }
  return pixels;
}

// ─── ICO 打包（PNG-in-ICO）──────────────────────────────────

/**
 * 把多张 PNG 打进一个 .ico。
 *
 * 用 PNG 而非老式 BMP：ico 的 image data 段允许直接内嵌 PNG（Vista 起），
 * 省掉手写 BMP 头与逐行翻转 —— 浏览器、Windows 资源管理器都认。
 * 目录项的宽/高字节留 0 才表示 256，这里尺寸都小于 256，直接写字面值。
 */
function encodeIco(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);              // reserved
  header.writeUInt16LE(1, 2);              // type: 1 = icon
  header.writeUInt16LE(entries.length, 4); // count

  const dirSize = 16 * entries.length;
  let offset = 6 + dirSize;
  const dirs = [];
  const blobs = [];

  for (const { size, png } of entries) {
    const dir = Buffer.alloc(16);
    dir[0] = size;                          // width（0 才是 256）
    dir[1] = size;                          // height
    dir[2] = 0;                             // color count: 0 = 未使用
    dir[3] = 0;                             // reserved
    dir.writeUInt16LE(1, 4);                // planes
    dir.writeUInt16LE(32, 6);               // bit count
    dir.writeUInt32LE(png.length, 8);       // 图像数据字节数
    dir.writeUInt32LE(offset, 12);          // 图像数据偏移
    dirs.push(dir);
    blobs.push(png);
    offset += png.length;
  }

  return Buffer.concat([header, ...dirs, ...blobs]);
}

// ─── SVG 版（现代浏览器优先取，免去多尺寸位图的糊边）─────────

function svgMarkup() {
  const strokes = A_STROKES
    .map(([ax, ay, bx, by]) => {
      // 归一化坐标 [-0.5,0.5] → viewBox 0..100，y 轴向下不用翻转
      const t = (v) => ((v + 0.5) * 100).toFixed(2);
      return `<path d="M${t(ax)} ${t(ay)}L${t(bx)} ${t(by)}"/>`;
    })
    .join('');
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
  <rect width="100" height="100" rx="${(RECT_RADIUS * 100).toFixed(1)}" fill="${BRAND}"/>
  <g fill="none" stroke="#fff" stroke-width="${(A_THICKNESS * 200).toFixed(1)}"
     stroke-linecap="round" stroke-linejoin="round">${strokes}</g>
</svg>
`;
}

// ─── 输出 ──────────────────────────────────────────────────

mkdirSync(UI_DIR, { recursive: true });

const SIZES = [16, 32, 48];
const entries = SIZES.map((size) => ({ size, png: encodePng(size, size, rasterize(size)) }));
writeFileSync(ICO_OUT, encodeIco(entries));
writeFileSync(SVG_OUT, svgMarkup());

console.log(`[favicon] 已生成 ${ICO_OUT}（${SIZES.join('/')} 三尺寸）`);
console.log(`[favicon] 已生成 ${SVG_OUT}`);
