// Renders the brand PNGs. The wordmark is live text in Playfair Display, which GitHub will not load,
// so each SVG is inlined in an HTML page that pulls the font from Google Fonts (render time only) and is screenshotted in Edge.
//   PW_DIR=<dir containing node_modules/playwright> node render-brand.mjs
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const { chromium } = createRequire(join(process.env.PW_DIR || HERE, 'x.js'))('playwright');
const FONT = '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Playfair+Display:ital,wght@0,800;1,400&family=JetBrains+Mono:wght@500&display=swap">';
const svg = (f) => readFileSync(join(HERE, f), 'utf8');
const sized = (s, w, h) => s.replace(/<svg /, `<svg style="display:block;width:${w}px;height:${h}px" `).replace(/ width="\d+" height="\d+"/, '');

// [source svg, output png, css width, css height, scale, transparent]
const JOBS = [
  ['logo-lockup.svg', 'logo-lockup.png', 600, 120, 2, true],
  ['logo-lockup-light.svg', 'logo-lockup-light.png', 600, 120, 2, true],
  ['hero-dark.svg', 'hero-dark.png', 1200, 360, 2, false],
  ['hero-light.svg', 'hero-light.png', 1200, 360, 2, false],
  ['logo.svg', 'logo-512.png', 512, 512, 1, true],
  ['logo.svg', 'logo-256.png', 256, 256, 1, true],
  ['logo.svg', 'logo-64.png', 64, 64, 1, true],
  ['app-icon.svg', 'app-icon-1024.png', 1024, 1024, 1, false],
];

const browser = await chromium.launch({ channel: 'msedge' });
try {
  for (const [src, out, w, h, scale, transparent] of JOBS) {
    const page = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: scale });
    await page.setContent(`<!doctype html><meta charset="utf-8">${FONT}<body style="margin:0;background:transparent">${sized(svg(src), w, h)}</body>`, { waitUntil: 'networkidle' });
    await page.evaluate(() => document.fonts.load('800 40px "Playfair Display"').then(() => document.fonts.ready));
    await page.screenshot({ path: join(HERE, out), omitBackground: transparent });
    await page.close();
    console.log(out);
  }
  const page = await browser.newPage({ viewport: { width: 1280, height: 640 } });
  await page.goto(pathToFileURL(join(HERE, 'social-preview.html')).href, { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: join(HERE, 'social-preview.png') });
  console.log('social-preview.png');
} finally {
  await browser.close();
}
