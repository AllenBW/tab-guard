// Draws Tab Guard's icons as PNGs with no dependencies:
//   src-tauri/icons/source.png  1024px app icon (feed to `tauri icon`)
//   src-tauri/icons/tray.png    44px menu-bar template icon (black + alpha)
// The mark is a shield with a browser tab cut out of it.
import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

function png(width, height, rgba) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0;
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// Shapes live in a -1..1 box.
const shieldPoly = (() => {
  const pts = [];
  const top = -0.78, side = 0.62, shoulder = 0.08, tip = 0.86;
  pts.push([-side, top + 0.08], [0, top - 0.04], [side, top + 0.08], [side, shoulder]);
  // Right side curves down to the tip, then back up the left (quadratic bezier).
  const bez = (p0, c, p1, t) => [
    (1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * c[0] + t * t * p1[0],
    (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * c[1] + t * t * p1[1],
  ];
  for (let i = 1; i <= 24; i++) pts.push(bez([side, shoulder], [side, 0.55], [0, tip], i / 24));
  for (let i = 1; i <= 24; i++) pts.push(bez([0, tip], [-side, 0.55], [-side, shoulder], i / 24));
  return pts;
})();

function inPoly(x, y, poly) {
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [xi, yi] = poly[i], [xj, yj] = poly[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

function inRoundRect(x, y, x0, y0, x1, y1, r) {
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r && x >= x0 && x <= x1 && y >= y0 && y <= y1;
}

// A browser tab: a wide body with a smaller tab on its top-left edge.
function inTab(x, y, s = 1) {
  const body = inRoundRect(x, y, -0.36 * s, -0.22 * s, 0.36 * s, 0.3 * s, 0.07 * s);
  const tab = inRoundRect(x, y, -0.36 * s, -0.36 * s, -0.02 * s, -0.12 * s, 0.06 * s);
  return body || tab;
}

// Is (x,y) on the mark, given the mark's scale inside the image?
function markCoverage(x, y, scale) {
  const mx = x / scale, my = y / scale;
  return inPoly(mx, my, shieldPoly) && !inTab(mx, my - 0.02);
}

function render(size, shade) {
  const buf = Buffer.alloc(size * size * 4);
  const ss = 4;
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let acc = [0, 0, 0, 0];
      for (let sy = 0; sy < ss; sy++) {
        for (let sx = 0; sx < ss; sx++) {
          const x = ((px + (sx + 0.5) / ss) / size) * 2 - 1;
          const y = ((py + (sy + 0.5) / ss) / size) * 2 - 1;
          // Average in premultiplied space so edges don't fringe.
          const c = shade(x, y);
          for (let k = 0; k < 3; k++) acc[k] += c[k] * c[3];
          acc[3] += c[3];
        }
      }
      const i = (py * size + px) * 4;
      for (let k = 0; k < 3; k++) buf[i + k] = acc[3] > 0 ? Math.round(acc[k] / acc[3]) : 0;
      buf[i + 3] = Math.round(acc[3] / (ss * ss));
    }
  }
  return buf;
}

const mix = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);

// App icon: macOS-style rounded square, deep teal gradient, white mark.
const appIcon = render(1024, (x, y) => {
  // Apple's grid: content square is ~80% of the canvas.
  if (!inRoundRect(x, y, -0.8, -0.8, 0.8, 0.8, 0.36)) return [0, 0, 0, 0];
  const bg = mix([20, 128, 120], [10, 60, 70], (y + 0.8) / 1.6);
  if (markCoverage(x, y + 0.02, 0.62)) return [255, 255, 255, 255];
  return [...bg, 255];
});
writeFileSync("src-tauri/icons/source.png", png(1024, 1024, appIcon));

// Menu-bar template icon: black mark on transparent, a little padding.
const tray = render(44, (x, y) => (markCoverage(x, y, 0.98) ? [0, 0, 0, 255] : [0, 0, 0, 0]));
writeFileSync("src-tauri/icons/tray.png", png(44, 44, tray));
console.log("wrote src-tauri/icons/source.png and src-tauri/icons/tray.png");
