/**
 * One orb: the voice-assistant presence for the All for One desktop app.
 *
 * Original design ("etched glass lens"): a navy glass disc with thin contour lines that
 * drift like topographic ripples, a soft core light that wanders inside, and a rim light.
 * The disc edge is deformed by 8 angular harmonics, one per audio band, so speech visibly
 * "shapes" the silhouette. Colour moves blue (#1F6FD6) -> teal (#14B8A6) as the assistant
 * goes from hearing to speaking.
 *
 * Rendering: one full-canvas quad + one fragment shader (WebGL1, works on WebGL2 too).
 * If WebGL is unavailable a Canvas2D renderer draws a simplified version of the same frame.
 * No dependencies, no assets, no eval, no network.
 *
 * Build for the demo (classic script, opens from disk):
 *   npx --yes esbuild orb.ts --bundle --format=iife --global-name=OrbLib --outfile=orb.js
 */

export type OrbState = 'idle' | 'listening' | 'thinking' | 'speaking' | 'muted';
export interface Orb {
  setState(s: OrbState): void;
  setLevel(level: number /*0..1*/, fft?: Uint8Array): void;
  /** Light theme: a frosted blue glass body instead of the navy one. Dark (false) is unchanged. */
  setLight(on: boolean): void;
  destroy(): void;
}

// ---------------------------------------------------------------- state targets
// Every number here is lerped (about 250 ms) so state changes never jump.
interface Params {
  mix: number;       // 0 = blue, 1 = teal
  swirl: number;     // 0..1 twist of the contour field (thinking)
  spinSpeed: number; // rad/s of the extra rotation
  breath: number;    // 0..1 idle breathing amount
  glow: number;      // outer glow strength
  grey: number;      // 0..1 desaturate + dim (muted)
  gain: number;      // how much the live level / FFT is allowed to move the orb
  deform: number;    // how much the FFT bands deform the edge
  timeScale: number; // speed of the ambient motion
}

const TARGETS: Record<OrbState, Params> = {
  idle:      { mix: 0.0,  swirl: 0.0, spinSpeed: 0.05, breath: 1.0, glow: 0.45, grey: 0, gain: 0, deform: 0.0, timeScale: 1.0 },
  listening: { mix: 0.0,  swirl: 0.0, spinSpeed: 0.08, breath: 0.3, glow: 0.8,  grey: 0, gain: 1, deform: 0.35, timeScale: 1.2 },
  thinking:  { mix: 0.4,  swirl: 1.0, spinSpeed: 1.4,  breath: 0.2, glow: 0.7,  grey: 0, gain: 0, deform: 0.0, timeScale: 1.6 },
  speaking:  { mix: 1.0,  swirl: 0.2, spinSpeed: 0.15, breath: 0.2, glow: 0.95, grey: 0, gain: 1, deform: 1.0, timeScale: 1.3 },
  muted:     { mix: 0.0,  swirl: 0.0, spinSpeed: 0.0,  breath: 0.0, glow: 0.08, grey: 1, gain: 0, deform: 0.0, timeScale: 0.1 },
};

/** Everything a renderer needs for one frame. */
interface Frame {
  t: number; spin: number; level: number; bands: number[];
  mix: number; swirl: number; breath: number; glow: number; grey: number;
  light: number; // 0 = dark theme (the original look), 1 = light theme
  reduced: boolean;
}
interface Renderer { resize(w: number, h: number): void; draw(f: Frame): void; destroy(): void; }

const NAVY = [0.043, 0.106, 0.227];  // #0B1B3A
const BLUE = [0.122, 0.435, 0.839];  // #1F6FD6
const TEAL = [0.078, 0.722, 0.651];  // #14B8A6
const NBANDS = 8;
const FRAME_MS = 1000 / 60;

// ---------------------------------------------------------------- WebGL renderer
const VERT = `
attribute vec2 aPos;
void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }`;

const FRAG = `
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform vec2 uRes;
uniform float uTime, uLevel, uMix, uSwirl, uSpin, uGrey, uBreath, uGlow, uLight;
uniform vec4 uB0, uB1;   // 8 audio bands -> 8 edge harmonics
const vec3 NAVY = vec3(0.043, 0.106, 0.227);
const vec3 BLUE = vec3(0.122, 0.435, 0.839);
const vec3 TEAL = vec3(0.078, 0.722, 0.651);

vec2 rot(vec2 v, float a){ float c = cos(a), s = sin(a); return vec2(c*v.x - s*v.y, s*v.x + c*v.y); }
float lobe(float a, float k, float ph, float amp){ return amp * cos(a*k + ph); }

void main(){
  float m = min(uRes.x, uRes.y);
  vec2 p = (gl_FragCoord.xy - 0.5*uRes) / (0.5*m);   // short side spans -1..1
  float r = length(p);
  float a = atan(p.y, p.x);
  float t = uTime;

  // silhouette: base radius + breathing + level swell + band-driven harmonics
  float R = 0.56 + uBreath*0.022*sin(t*1.2) + uLevel*0.07;
  float def = lobe(a,2.0, t*0.9,      uB0.x) + lobe(a,3.0,-t*1.1+1.0, uB0.y)
            + lobe(a,4.0, t*1.3+2.0,  uB0.z) + lobe(a,5.0,-t*1.5+0.5, uB0.w)
            + lobe(a,6.0, t*1.7+3.0,  uB1.x) + lobe(a,7.0,-t*1.9+1.5, uB1.y)
            + lobe(a,9.0, t*2.1+2.5,  uB1.z) + lobe(a,11.0,-t*2.3,    uB1.w);
  R += def * 0.03;

  float d = r - R;
  float px = 2.0 / m;
  float body = 1.0 - smoothstep(-px, px, d);
  float rho = r / R;
  vec2 q = p / R;

  // contour field: two warped sines, twisted harder toward the centre while thinking
  vec2 w = rot(q, uSpin + uSwirl*(1.2 - clamp(rho,0.0,1.2))*2.4);
  float f = sin(w.x*3.0 + sin(w.y*2.2 + t*0.45)*1.5)
          + sin(w.y*3.4 + sin(w.x*2.7 - t*0.35)*1.4)
          + uLevel*1.5*sin(rho*9.0 - t*3.0);
  float g = f*0.5 + rho*1.8 - t*0.05;
  float line = smoothstep(0.43, 0.5, abs(fract(g) - 0.5));

  vec3 acc  = mix(BLUE, TEAL, uMix);
  vec3 accL = mix(vec3(0.45,0.72,1.0), vec3(0.45,0.95,0.88), uMix);

  // wandering core light
  vec2 cOff = 0.18 * vec2(sin(t*0.4 + uSpin), cos(t*0.33));
  float core = exp(-length(q - cOff) * 2.2);
  float rim  = smoothstep(0.7, 1.0, rho);

  // light theme (uLight = 0 leaves the dark look exactly as it was): frosted blue glass with
  // deeper contour lines, so the orb is not a black ball on a pale background
  vec3 base = mix(NAVY, vec3(0.50, 0.68, 0.93), uLight);
  vec3 lineC = mix(accL, acc * 0.75, uLight);
  vec3 col = base * (0.9 + 0.3*(1.0 - rho));
  col += acc  * core * (0.55 + uLevel*0.6) * (1.0 - 0.35*uLight);
  col += lineC * line * mix(0.14 + 0.5*core + 0.25*rim, 0.45 + 0.3*core, uLight);
  col += accL * rim * rim * mix(0.55, 0.25, uLight);

  // muted: grey + dim
  float lum = dot(col, vec3(0.30, 0.59, 0.11));
  col = mix(col, vec3(lum*0.85), uGrey) * (1.0 - 0.35*uGrey);
  vec3 gacc = mix(acc, vec3(0.45), uGrey);

  // outer glow (premultiplied alpha)
  float glow = exp(-max(d, 0.0) * 7.0) * uGlow * (1.0 + uLevel*0.6) * (1.0 - body);
  vec3 rgb = col*body + gacc*glow*0.55;
  float al = clamp(body + glow*0.55, 0.0, 1.0);
  gl_FragColor = vec4(min(rgb, vec3(al)), al);
}`;

class GLRenderer implements Renderer {
  private gl: WebGLRenderingContext;
  private prog: WebGLProgram;
  private buf: WebGLBuffer;
  private u: Record<string, WebGLUniformLocation | null> = {};

  constructor(private canvas: HTMLCanvasElement, gl: WebGLRenderingContext) {
    this.gl = gl;
    const vs = this.compile(gl.VERTEX_SHADER, VERT);
    const fs = this.compile(gl.FRAGMENT_SHADER, FRAG);
    const prog = gl.createProgram()!;
    gl.attachShader(prog, vs); gl.attachShader(prog, fs); gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error('orb: link failed');
    gl.deleteShader(vs); gl.deleteShader(fs);
    this.prog = prog;
    gl.useProgram(prog);
    // one big triangle covers the viewport: cheaper than a quad, no seam
    this.buf = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, 'aPos');
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    for (const n of ['uRes', 'uTime', 'uLevel', 'uMix', 'uSwirl', 'uSpin', 'uGrey', 'uBreath', 'uGlow', 'uLight', 'uB0', 'uB1']) {
      this.u[n] = gl.getUniformLocation(prog, n);
    }
    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA); // premultiplied
    gl.clearColor(0, 0, 0, 0);
  }

  private compile(type: number, src: string): WebGLShader {
    const gl = this.gl;
    const s = gl.createShader(type)!;
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(s);
      gl.deleteShader(s);
      throw new Error('orb: shader compile failed: ' + log);
    }
    return s;
  }

  resize(w: number, h: number): void {
    this.canvas.width = w; this.canvas.height = h;
    this.gl.viewport(0, 0, w, h);
  }

  draw(f: Frame): void {
    const gl = this.gl, u = this.u, b = f.bands;
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.uniform2f(u.uRes, this.canvas.width, this.canvas.height);
    gl.uniform1f(u.uTime, f.t);
    gl.uniform1f(u.uLevel, f.level);
    gl.uniform1f(u.uMix, f.mix);
    gl.uniform1f(u.uSwirl, f.swirl);
    gl.uniform1f(u.uSpin, f.spin);
    gl.uniform1f(u.uGrey, f.grey);
    gl.uniform1f(u.uBreath, f.breath);
    gl.uniform1f(u.uGlow, f.glow);
    gl.uniform1f(u.uLight, f.light);
    gl.uniform4f(u.uB0, b[0], b[1], b[2], b[3]);
    gl.uniform4f(u.uB1, b[4], b[5], b[6], b[7]);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  destroy(): void {
    const gl = this.gl;
    gl.deleteBuffer(this.buf);
    gl.deleteProgram(this.prog);
    const ext = gl.getExtension('WEBGL_lose_context');
    if (ext) ext.loseContext();
  }
}

// ---------------------------------------------------------------- Canvas2D fallback
class Canvas2DRenderer implements Renderer {
  private w = 1; private h = 1;
  constructor(private canvas: HTMLCanvasElement, private ctx: CanvasRenderingContext2D) {}

  resize(w: number, h: number): void {
    this.canvas.width = w; this.canvas.height = h; this.w = w; this.h = h;
  }

  private css(c: number[], k: number, a: number): string {
    return 'rgba(' + Math.round(Math.min(1, c[0] * k) * 255) + ',' +
      Math.round(Math.min(1, c[1] * k) * 255) + ',' + Math.round(Math.min(1, c[2] * k) * 255) + ',' + a + ')';
  }

  draw(f: Frame): void {
    const ctx = this.ctx, cx = this.w / 2, cy = this.h / 2, m = Math.min(this.w, this.h) / 2;
    ctx.clearRect(0, 0, this.w, this.h);
    const mixc = [0, 1, 2].map(i => BLUE[i] + (TEAL[i] - BLUE[i]) * f.mix);
    const grey = [0.45, 0.45, 0.45];
    const acc = [0, 1, 2].map(i => mixc[i] + (grey[i] - mixc[i]) * f.grey);
    const dim = 1 - 0.35 * f.grey;
    const R = m * (0.56 + f.breath * 0.022 * Math.sin(f.t * 1.2) + f.level * 0.07);

    // glow
    const gr = ctx.createRadialGradient(cx, cy, R * 0.8, cx, cy, R * 1.6);
    gr.addColorStop(0, this.css(acc, 1, 0.5 * f.glow * (1 + f.level * 0.6)));
    gr.addColorStop(1, this.css(acc, 1, 0));
    ctx.fillStyle = gr; ctx.fillRect(0, 0, this.w, this.h);

    // deformed silhouette
    const lobes = [2, 3, 4, 5, 6, 7, 9, 11];
    ctx.beginPath();
    for (let i = 0; i <= 120; i++) {
      const a = (i / 120) * Math.PI * 2;
      let d = 0;
      for (let k = 0; k < NBANDS; k++) {
        d += f.bands[k] * Math.cos(a * lobes[k] + f.t * (k % 2 ? -1 : 1) * (0.9 + k * 0.2) + k);
      }
      const rr = R + d * 0.03 * m;
      const x = cx + Math.cos(a) * rr, y = cy + Math.sin(a) * rr;
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    }
    ctx.closePath();
    const ox = Math.sin(f.t * 0.4 + f.spin) * 0.18 * R, oy = Math.cos(f.t * 0.33) * 0.18 * R;
    const body = ctx.createRadialGradient(cx + ox, cy + oy, 0, cx, cy, R * 1.05);
    body.addColorStop(0, this.css(acc, 0.9 * dim, 1));
    const base = [0, 1, 2].map(i => NAVY[i] + ([0.5, 0.68, 0.93][i] - NAVY[i]) * f.light);
    body.addColorStop(0.55, this.css(base, 1.4 * dim, 1));
    body.addColorStop(1, this.css(base, dim, 1));
    ctx.fillStyle = body; ctx.fill();
    ctx.save(); ctx.clip();

    // a few contour rings (skipped for reduced motion: they are the "moving" part)
    ctx.lineWidth = Math.max(1, m * 0.008);
    for (let i = 1; i <= 4; i++) {
      const rr = R * (i / 4.6) * (1 + 0.04 * Math.sin(f.t * 0.45 + i + f.swirl * f.spin));
      ctx.strokeStyle = this.css(acc, 1.3, 0.22 * dim);
      ctx.beginPath(); ctx.ellipse(cx, cy, rr, rr * (0.9 + 0.1 * Math.sin(f.t * 0.3 + i)), f.spin + i, 0, Math.PI * 2);
      ctx.stroke();
    }
    ctx.restore();
    // rim light
    ctx.lineWidth = Math.max(1, m * 0.012);
    ctx.strokeStyle = this.css(acc, 1.4, 0.55 * dim);
    ctx.stroke();
  }

  destroy(): void { /* nothing to free */ }
}

// ---------------------------------------------------------------- helpers
function lerp(a: number, b: number, k: number): number { return a + (b - a) * k; }

/** Group an FFT (Uint8Array, 0..255) into 8 log-spaced voice bands, 0..1. */
function fftToBands(fft: Uint8Array, out: number[]): void {
  const n = Math.max(NBANDS + 1, Math.floor(fft.length * 0.7)); // skip the near-silent top
  let lo = 1;
  for (let i = 0; i < NBANDS; i++) {
    let hi = Math.max(lo + 1, Math.round(Math.pow(n, (i + 1) / NBANDS)));
    hi = Math.min(hi, fft.length);
    let sum = 0;
    for (let j = lo; j < hi; j++) sum += fft[j];
    out[i] = Math.min(1, (sum / Math.max(1, hi - lo) / 255) * 1.6);
    lo = hi;
  }
}

// ---------------------------------------------------------------- the orb
export function createOrb(canvas: HTMLCanvasElement): Orb {
  const reducedMq = window.matchMedia ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;
  let reduced = !!(reducedMq && reducedMq.matches);
  const onReduced = () => { reduced = !!(reducedMq && reducedMq.matches); };
  if (reducedMq && reducedMq.addEventListener) reducedMq.addEventListener('change', onReduced);

  let renderer: Renderer;
  let destroyed = false;

  function makeRenderer(): Renderer {
    const attrs = { alpha: true, premultipliedAlpha: true, antialias: false, depth: false, stencil: false, powerPreference: 'low-power' as const };
    try {
      const gl = (canvas.getContext('webgl', attrs) || canvas.getContext('experimental-webgl', attrs)) as WebGLRenderingContext | null;
      if (gl) return new GLRenderer(canvas, gl);
    } catch (e) { /* fall through to 2D */ }
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('orb: no WebGL and no Canvas2D');
    return new Canvas2DRenderer(canvas, ctx);
  }
  renderer = makeRenderer();

  // animated values (lerped toward TARGETS[state])
  let state: OrbState = 'idle';
  const cur: Params = { ...TARGETS.idle };
  let t = 0, spin = 0, level = 0;
  const bands: number[] = new Array(NBANDS).fill(0);
  const rawBands: number[] = new Array(NBANDS).fill(0);
  let rawLevel = 0, haveFft = false;
  let lightTarget = 0, light = 0;

  // ---- sizing: backing store = CSS size * min(dpr, 2)
  function resize(): void {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
    if (w !== canvas.width || h !== canvas.height) renderer.resize(w, h);
  }
  const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(resize) : null;
  if (ro) ro.observe(canvas); else window.addEventListener('resize', resize);
  resize();

  // ---- WebGL context loss (GPU reset, sleep/wake)
  const onLost = (e: Event) => { e.preventDefault(); };
  const onRestored = () => {
    try { renderer = makeRenderer(); canvas.width = 0; resize(); } catch (e) { /* stay blank */ }
  };
  canvas.addEventListener('webglcontextlost', onLost);
  canvas.addEventListener('webglcontextrestored', onRestored);

  // ---- loop: <= 60 fps, paused while the document is hidden
  let raf = 0, last = 0, lastDraw = 0;

  function frame(ts: number): void {
    raf = 0;
    if (destroyed || document.hidden) return;
    raf = requestAnimationFrame(frame);
    const minGap = reduced ? 50 : FRAME_MS - 1.5; // reduced motion: 20 fps is plenty
    if (ts - lastDraw < minGap) return;
    const dt = Math.min(0.1, last ? (ts - last) / 1000 : 0.016);
    last = ts; lastDraw = ts;

    // ~250 ms to settle: time constant 80 ms
    const k = 1 - Math.exp(-dt / 0.08);
    const tg = TARGETS[state];
    (Object.keys(tg) as (keyof Params)[]).forEach(key => { cur[key] = lerp(cur[key], tg[key], k); });

    light = lerp(light, lightTarget, k);
    t += dt * (reduced ? 0 : cur.timeScale);
    spin += dt * (reduced ? 0 : cur.spinSpeed);

    // live signal: fast attack, slower release; ignored when gain ~ 0 (idle/thinking/muted)
    const atk = 1 - Math.exp(-dt / 0.04), rel = 1 - Math.exp(-dt / 0.14);
    const lvTarget = rawLevel * cur.gain;
    level = lerp(level, lvTarget, lvTarget > level ? atk : rel);
    for (let i = 0; i < NBANDS; i++) {
      // no FFT supplied: synthesise plausible bands from the level so the edge still moves
      const src = haveFft ? rawBands[i] : rawLevel * (0.55 + 0.45 * Math.sin(t * 3 + i * 1.7));
      const tgt = src * cur.deform * (reduced ? 0 : 1);
      bands[i] = lerp(bands[i], tgt, tgt > bands[i] ? atk : rel);
    }

    renderer.draw({
      t, spin, level: reduced ? level * 0.4 : level, bands,
      mix: cur.mix, swirl: reduced ? 0 : cur.swirl, breath: reduced ? 0 : cur.breath,
      glow: cur.glow, grey: cur.grey, light, reduced,
    });
  }

  function start(): void { if (!raf && !destroyed && !document.hidden) { last = 0; raf = requestAnimationFrame(frame); } }
  function stop(): void { if (raf) { cancelAnimationFrame(raf); raf = 0; } }
  const onVis = () => { if (document.hidden) stop(); else start(); };
  document.addEventListener('visibilitychange', onVis);
  start();

  return {
    setState(s: OrbState): void { if (TARGETS[s]) state = s; },
    setLight(on: boolean): void { lightTarget = on ? 1 : 0; },
    setLevel(l: number, fft?: Uint8Array): void {
      rawLevel = l > 0 ? (l < 1 ? l : 1) : 0;
      if (fft && fft.length) { fftToBands(fft, rawBands); haveFft = true; } else haveFft = false;
    },
    destroy(): void {
      if (destroyed) return;
      destroyed = true; stop();
      document.removeEventListener('visibilitychange', onVis);
      canvas.removeEventListener('webglcontextlost', onLost);
      canvas.removeEventListener('webglcontextrestored', onRestored);
      if (ro) ro.disconnect(); else window.removeEventListener('resize', resize);
      if (reducedMq && reducedMq.removeEventListener) reducedMq.removeEventListener('change', onReduced);
      renderer.destroy();
    },
  };
}
