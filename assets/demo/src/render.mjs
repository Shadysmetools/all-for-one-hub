// Reproduces ../../demo.mp4, ../../demo.gif and ../poster.png from player.html + data/scenes.json + voiceover/*.mp3.
//   node render.mjs            record + encode demo.mp4, then gif + poster
//   node render.mjs --reencode  re-mux/encode from out/raw.webm (audio/lead tweaks, no re-recording)
//   node render.mjs --shots    one PNG per scene into out/shots (layout check, no recording)
// Needs: Node, Playwright (PW_DIR=<dir containing node_modules/playwright> if not installed here),
// the system Edge (channel msedge), and ffmpeg on PATH. Voice clips are cached in voiceover/ (see README).
import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { readFileSync, writeFileSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, extname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, '..');
const TMP = join(HERE, 'out');
const req = createRequire(join(process.env.PW_DIR || HERE, 'x.js'));
const { chromium } = req('playwright');
const scenes = JSON.parse(readFileSync(join(HERE, 'data/scenes.json'), 'utf8'));
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };

const server = createServer((rq, rs) => {
  const p = join(HERE, decodeURIComponent(rq.url.split('?')[0]));
  if (!p.startsWith(HERE) || !existsSync(p)) { rs.writeHead(404).end(); return; }
  rs.writeHead(200, { 'Content-Type': MIME[extname(p)] || 'application/octet-stream' }).end(readFileSync(p));
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/player.html`;

const ff = (args) => { const r = spawnSync('ffmpeg', ['-y', '-hide_banner', '-loglevel', 'error', ...args], { stdio: 'inherit' }); if (r.status) throw new Error('ffmpeg failed'); };

async function shots(browser) {
  mkdirSync(join(TMP, 'shots'), { recursive: true });
  const only = process.argv.find((a) => a.startsWith('--only='))?.slice(7);
  for (const s of scenes.scenes.filter((x) => !only || only.split(',').includes(x.id))) {
    const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
    await page.goto(`${base}?from=${s.id}&hold`);
    await page.waitForTimeout(Math.min(s.dur * 1000 * 0.88, 14000));
    await page.screenshot({ path: join(TMP, 'shots', `${s.id}.png`) });
    await page.close();
  }
}

async function record(browser) {
  rmSync(join(TMP, 'rec'), { recursive: true, force: true });
  const ctx = await browser.newContext({ viewport: { width: 1920, height: 1080 }, recordVideo: { dir: join(TMP, 'rec'), size: { width: 1920, height: 1080 } } });
  const page = await ctx.newPage();
  await page.goto(base + '?sync');
  await page.waitForFunction(() => window.__ready, null, { timeout: 30000 });
  await page.waitForTimeout(800);
  await page.evaluate(() => window.startDemo());
  await page.waitForFunction(() => window.__done, null, { timeout: (scenes.total + 30) * 1000 });
  await page.waitForTimeout(600);
  await ctx.close();
  const raw = join(TMP, 'raw.webm');
  renameSync(join(TMP, 'rec', readdirSync(join(TMP, 'rec'))[0]), raw);
  const lead = detectLead(raw);
  writeFileSync(join(TMP, 'lead.txt'), String(lead));
  return { raw, lead };
}

// The player flashes the screen white for 200 ms right before its clock starts (?sync). Find the end of that flash in the recording.
function detectLead(raw) {
  const r = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-t', '12', '-i', raw, '-an', '-vf', 'signalstats,metadata=mode=print:file=-', '-f', 'null', '-'], { encoding: 'utf8', maxBuffer: 1 << 28 });
  let t = 0, lastBright = null, next = null;
  for (const line of r.stdout.split(/\r?\n/)) {
    const m = line.match(/pts_time:([\d.]+)/), y = line.match(/YAVG=([\d.]+)/);
    if (m) t = parseFloat(m[1]);
    if (y && parseFloat(y[1]) > 150) { lastBright = t; next = null; }
    else if (y && lastBright !== null && next === null) next = t;
  }
  // the last bright-to-dark change is the end of the flash (the blank page before load is white too)
  if (lastBright === null) throw new Error('sync flash not found in the recording');
  return next ?? lastBright + 0.04;
}

function audioFilter() {
  const inputs = [], parts = [];
  let n = 1;
  for (const s of scenes.scenes) for (const c of s.clips) {
    inputs.push('-i', join(HERE, 'voiceover', c.file));
    const ms = Math.round(c.start * 1000);
    const fade = c.cut ? `,afade=t=out:st=${(c.dur - 0.08).toFixed(3)}:d=0.08` : '';
    parts.push(`[${n}:a]atrim=0:${c.dur},asetpts=PTS-STARTPTS${fade},adelay=${ms}|${ms}[v${n}]`);
    n++;
  }
  const T = scenes.total;
  const pad = `sine=f=110:r=44100,volume=0.5[p1];sine=f=165:r=44100,volume=0.35[p2];sine=f=220.5:r=44100,volume=0.25[p3];` +
    `[p1][p2][p3]amix=inputs=3:normalize=0,lowpass=f=600,tremolo=f=0.15:d=0.4,volume=0.05,afade=t=in:d=2,afade=t=out:st=${T - 3}:d=3[pad]`;
  const vs = parts.map((_, i) => `[v${i + 1}]`).join('');
  const graph = `${parts.join(';')};${pad};${vs}amix=inputs=${parts.length}:normalize=0:dropout_transition=0,volume=1.0[vo];[vo][pad]amix=inputs=2:normalize=0,alimiter=limit=0.95[aout]`;
  return { inputs, graph };
}

function encode(raw, lead) {
  const { inputs, graph } = audioFilter();
  const crf = process.env.CRF || '21';
  lead += Number(process.env.LEAD_ADJUST ?? 0);
  ff(['-ss', lead.toFixed(3), '-i', raw, ...inputs, '-filter_complex', graph, '-map', '0:v', '-map', '[aout]',
    '-t', scenes.total.toFixed(3), '-r', '30', '-c:v', 'libx264', '-preset', 'slow', '-crf', crf, '-pix_fmt', 'yuv420p',
    '-c:a', 'aac', '-b:a', '128k', '-movflags', '+faststart', join(OUT, '..', 'demo.mp4')]);
}

function gif() {
  // ~25 s highlight: the approval + judge + proof + KPI stretch (start/length overridable via GIF_START / GIF_LEN)
  const start = process.env.GIF_START || '28.4', len = process.env.GIF_LEN || '22';
  const pal = join(TMP, 'pal.png');
  const vf = 'fps=12,scale=960:-1:flags=lanczos';
  ff(['-ss', start, '-t', len, '-i', join(OUT, '..', 'demo.mp4'), '-vf', `${vf},palettegen=max_colors=64:stats_mode=diff`, pal]);
  ff(['-ss', start, '-t', len, '-i', join(OUT, '..', 'demo.mp4'), '-i', pal, '-lavfi', `${vf}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle`, join(OUT, '..', 'demo.gif')]);
  ff(['-ss', process.env.POSTER_AT || '31', '-i', join(OUT, '..', 'demo.mp4'), '-frames:v', '1', join(OUT, 'poster.png')]);
}

mkdirSync(TMP, { recursive: true });
const browser = await chromium.launch({ channel: 'msedge', args: ['--force-color-profile=srgb'] });
try {
  if (process.argv.includes('--shots')) await shots(browser);
  else if (process.argv.includes('--reencode')) { encode(join(TMP, 'raw.webm'), Number(readFileSync(join(TMP, 'lead.txt'), 'utf8'))); gif(); }
  else {
    const { raw, lead } = await record(browser);
    await browser.close();
    encode(raw, lead);
    gif();
    console.log('lead(s):', lead, 'done');
  }
} finally {
  await browser.close().catch(() => {});
  server.close();
}
