// Data-driven player core: loads data/*.json, builds the progress rail, runs scenes on the voiceover clock.
// Query params: ?autostart (begin immediately)  ?from=<scene id> (start there, for frame checks)  ?hold (stay on that scene)
(() => {
  const Q = new URLSearchParams(location.search);
  const $ = (s) => document.querySelector(s);
  const STEPS = { voice: [1], transcribe: [2], analysis: [3], ticket: [4], assign: [5], ack: [6], confirm: [7, 8], approve: [9], reply: [10], proof: [11], resolve: [12], kpi: [13] };
  const byStep = (tl) => Object.fromEntries(tl.events.map((e) => [e.step, e]));

  function buildRail(n) {
    const rail = $('#rail');
    for (let i = 1; i <= n; i++) {
      if (i > 1) rail.append(Object.assign(document.createElement('div'), { className: 'rl', id: 'rl' + i }));
      rail.append(Object.assign(document.createElement('div'), { className: 'rd', id: 'rd' + i, textContent: i }));
    }
  }

  function setRail(name, D) {
    const steps = STEPS[name];
    const cur = steps ? Math.max(...steps) : null;
    const done = name === 'title' ? 0 : cur ? cur - 1 : 13;
    for (let i = 1; i <= 13; i++) {
      $('#rd' + i).className = 'rd' + (i === cur ? ' cur' : i <= done ? ' done' : '');
      if (i > 1) $('#rl' + i).className = 'rl' + (name !== 'title' && i <= (cur || 13) ? ' done' : '');
    }
    $('#stepname').textContent = cur && D.steps[cur] ? `Step ${cur} · ${D.steps[cur].name}` : '';
  }

  function setCaption(text) {
    const c = $('#capText');
    c.classList.add('out');
    setTimeout(() => { c.textContent = text; c.classList.remove('out'); }, 260);
  }

  function makeCtx(el, sc, D) {
    const timers = [];
    let alive = true;
    const at = (ms, fn) => timers.push(setTimeout(() => alive && fn(), ms));
    const raf = (fn) => requestAnimationFrame((t) => alive && fn(t));
    const type = (target, text, startMs, durMs) => at(startMs, () => {
      const t0 = performance.now();
      const step = () => {
        const p = Math.min(1, (performance.now() - t0) / durMs);
        target.innerHTML = SCN.esc(text.slice(0, Math.round(p * text.length))) + '<span class="caret"></span>';
        if (p < 1) raf(step);
      };
      step();
    });
    const pop = (node, ms) => at(ms, () => node.classList.add('in'));
    return {
      el, D, at, raf, type, pop,
      clips: sc.clips.map((c) => ({ at: c.at * 1000, dur: c.dur * 1000, cut: c.cut * 1000 })), dur: sc.dur * 1000, audioStart: sc.audio_start - sc.start, audioDur: sc.audio_dur,
      stop() { alive = false; timers.forEach(clearTimeout); },
    };
  }

  async function main() {
    const load = (u) => fetch(u).then((r) => r.json());
    const [tl, ar, sc] = await Promise.all([load('data/timeline.json'), load('data/timeline.ar.json'), load('data/scenes.json')]);
    const D = { steps: byStep(tl), ar: byStep(ar), tl };
    buildRail(tl.events.length);
    window.__total = sc.total;
    window.__ready = true;

    // ?sync: flash the screen white for 200 ms before the demo clock starts, so render.mjs can find the exact start in the recording
    window.startDemo = () => {
      if (!Q.has('sync')) return begin();
      const f = document.createElement('div');
      f.style.cssText = 'position:fixed;inset:0;background:#fff;z-index:99';
      document.body.append(f);
      setTimeout(() => { begin(); f.remove(); }, 200);
    };
    const begin = () => {
      const stage = $('#stage');
      let idx = Math.max(0, sc.scenes.findIndex((s) => s.id === Q.get('from')));
      const t0 = Date.now() - sc.scenes[idx].start * 1000;
      window.__t0 = Date.now() - 0;
      let prev = null;
      const run = (i) => {
        const s = sc.scenes[i];
        const el = document.createElement('section');
        el.className = 'scene sc-' + s.scene;
        stage.append(el);
        const ctx = makeCtx(el, s, D);
        SCN.scenes[s.scene](ctx);
        setRail(s.scene, D);
        setCaption(s.caption);
        requestAnimationFrame(() => requestAnimationFrame(() => el.classList.add('show')));
        if (prev) { const p = prev; p.ctx.stop(); p.el.classList.add('gone'); setTimeout(() => p.el.remove(), 700); }
        prev = { el, ctx };
        const next = sc.scenes[i + 1];
        const endMs = (next ? next.start : sc.total) * 1000;
        setTimeout(() => (next ? (Q.has('hold') || run(i + 1)) : (window.__done = true)), Math.max(0, t0 + endMs - Date.now()));
      };
      run(idx);
    };
    if (Q.has('autostart') || Q.has('from')) window.startDemo();
  }
  main();
})();
