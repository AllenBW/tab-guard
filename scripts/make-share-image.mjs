// Draws website/share.jpg, the 1200×630 link-preview card (og:image / twitter:image):
// the icon, name and lede over the top of the real window screenshot (window-light.png).
// Run after replacing that screenshot: `node scripts/make-share-image.mjs`.
// Renders in Playwright's WebKit (`pnpm exec playwright install webkit` once).
import { readFileSync } from "node:fs";
import { webkit } from "@playwright/test";

const site = new URL("../website/", import.meta.url);
const dataUrl = (name) => `data:image/png;base64,${readFileSync(new URL(name, site)).toString("base64")}`;

// Colours are the page's light theme (website/index.html :root).
const html = `<!doctype html>
<html><head><meta charset="utf-8" /><style>
  * { box-sizing: border-box; }
  html, body { margin: 0; width: 1200px; height: 630px; overflow: hidden; }
  body { background: #f6f7f8; color: #1c2024; font-family: -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif; }
  header { position: absolute; left: 64px; top: 48px; right: 64px; display: flex; gap: 24px; align-items: center; }
  header img { width: 96px; height: 96px; }
  h1 { margin: 0; font-size: 56px; letter-spacing: -0.02em; line-height: 1.05; }
  p { margin: 6px 0 0; font-size: 26px; color: #5f676d; }
  .shot { position: absolute; left: 64px; top: 192px; width: 1072px; border-radius: 14px; overflow: hidden;
    border: 1px solid #e1e4e8; box-shadow: 0 10px 30px rgba(28, 32, 36, 0.10); background: #fff; }
  .shot img { display: block; width: 100%; }
</style></head><body>
  <header>
    <img src="${dataUrl("icon.png")}" alt="" />
    <div><h1>Tab Guard</h1><p>Closes browser tabs when they use too much memory.</p></div>
  </header>
  <div class="shot"><img src="${dataUrl("window-light.png")}" alt="" /></div>
</body></html>`;

const browser = await webkit.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 630 } });
await page.setContent(html, { waitUntil: "load" });
await page.screenshot({ path: new URL("share.jpg", site).pathname, type: "jpeg", quality: 82 });
await browser.close();
console.log("wrote website/share.jpg");
