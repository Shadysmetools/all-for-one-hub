// Coming next (desktop console + One), the hands-free One demo inside the Desktop window, and the end card.
(() => {
  const { h, esc } = SCN;
  const S = SCN.scenes;
  const eye = '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/></svg>';
  const killIco = '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M5.6 5.6l12.8 12.8"/></svg>';

  S.next = (ctx) => {
    const L = h('div', 'card pop', `<div class="kick">Coming next</div><h2>All for One Desktop</h2><p>Admin console</p>
      <div class="pills"><span class="chip">Approvals</span><span class="chip">Connectors</span><span class="chip">KPIs</span><span class="chip flare">Kill switch</span></div>
      <div class="mini-win"><div class="mt"><img src="assets/brand/logo.svg" width="18" alt=""> All for One Desktop</div><div class="ml"><div></div><div></div></div></div>`);
    const R = h('div', 'card pop', `<div class="kick">Coming next</div><h2>One · AI teammate</h2><p>Team voice assistant, powered by Claude</p>
      <div class="swave" id="nw"></div><div class="q">“Hey One, <em>what's waiting for my approval?</em>”</div>`);
    const w = R.querySelector('#nw');
    for (let i = 0; i < 40; i++) w.append(document.createElement('s'));
    const wrap = h('div', 'nxt'); wrap.append(L, R); ctx.el.append(wrap);
    ctx.pop(L, 100); ctx.pop(R, 600);
    const bars = [...w.children];
    const f = (t) => { bars.forEach((b, i) => (b.style.height = 8 + 30 * Math.abs(Math.sin(t / 260 + i * 0.5) * Math.cos(t / 530 + i * 0.2)) + 'px')); ctx.raf(f); };
    ctx.at(700, () => ctx.raf(f));
  };

  const rows = [['#20001', 'Northwind Supplies', 'Inventory stock fix · CONFIRM', '0.90', true], ['#20002', 'Acme Traders', 'Invoice question · CONFIRM', '0.86'], ['#20003', 'Harbor & Co.', 'Access request · RESOLVE', '0.91']];
  const words = (el, text, startMs, durMs, ctx, onDone) => {
    const w = text.split(' ');
    w.forEach((_, i) => ctx.at(startMs + (durMs * i) / w.length, () => { el.textContent = w.slice(0, i + 1).join(' '); }));
    if (onDone) ctx.at(startMs + durMs, onDone);
  };

  S.voiceai = (ctx) => {
    const [c1, c2, c3] = ctx.clips;
    const d = h('div', 'dwin pop', `<div class="dt"><img src="assets/brand/logo.svg" alt="">All for One Desktop<div class="wc"><i>—</i><i>□</i><i>✕</i></div></div>
    <div class="db"><div class="sb"><div class="act">Approvals <span class="bd">3</span></div><div>Tickets</div><div>KPIs</div><div>Connectors</div><div>Audit</div><div>Settings</div></div>
    <div class="main"><h3>Approvals queue <small>3 waiting</small></h3>
      ${rows.map((r, i) => `<div class="qrow" id="q${i}"><span class="mono">${r[0]}</span><span>${esc(r[1])}<small>${esc(r[2])}${r[4] ? ' · <b style="color:var(--flare)">URGENT</b>' : ''}</small></span><span class="j">${r[3]}</span></div>`).join('')}
      <div class="actcard pop" id="act"><div>Draft #20001 opened · approving needs your tap</div><span><i style="background:var(--pulse);color:#04201B;font-weight:700">Approve</i><i>Edit</i></span></div>
      <div class="foot">Phase 3 · presence &amp; speaker recognition run on your PC, opt-in, nothing stored</div></div>
    <div class="voice"><div class="vh"><div class="orb" id="orb"></div><div>One · AI teammate<small>powered by Claude</small></div>
      <div class="ctl"><span class="cam" title="camera on">${eye}<em>on-device</em></span><span class="mute" id="mute">${SCN.mic}</span><span class="kill">${killIco}</span></div></div>
      <div class="stat"><span class="ldot"></span><b id="st">Listening</b> · hands-free</div>
      <div class="lvl" id="lvl"></div>
      <div class="tags"><span class="chip flare pop" id="pres">Maya is here · on-device</span><span class="chip">wake word: Hey One</span><span class="chip">auto turn detection</span></div>
      <div class="tbox" id="tbox"></div></div></div>`);
    ctx.el.append(d); ctx.pop(d, 50);
    const $ = (s) => d.querySelector(s), orb = $('#orb'), st = $('#st'), tbox = $('#tbox'), lvl = $('#lvl');
    for (let i = 0; i < 24; i++) lvl.append(document.createElement('s'));
    const bars = [...lvl.children];
    let level = 0.1;
    const loop = (t) => { bars.forEach((b, i) => { const v = level * (0.35 + 0.65 * Math.abs(Math.sin(t / 140 + i * 0.9))); b.style.height = 5 + v * 54 + 'px'; b.classList.toggle('on', v > 0.25); }); ctx.raf(loop); };
    ctx.raf(loop);
    const mode = (m) => { orb.className = 'orb ' + m; st.textContent = m === 'speak' ? 'One is speaking' : m === 'think' ? 'Understanding' : 'Listening'; level = m === 'speak' ? 0.08 : 0.1; };
    mode('listen');
    const say = (cls, who) => { const b = h('div', 'tb ' + cls); if (who) b.innerHTML = `<span class="spk">${who}</span><span class="tt"></span>`; else b.innerHTML = '<span class="tt"></span>'; tbox.append(b); return b; };
    ctx.pop($('#pres'), 150);
    const hl = (i, on) => $('#q' + i).classList.toggle('hl', on);
    // One 1: proactive greeting
    ctx.at(c1.at, () => { mode('speak'); hl(0, true); const b = say('a'); words(b.querySelector('.tt'), "Morning, Maya — three approvals are waiting, one is urgent.", 0, c1.dur * 0.92, ctx); });
    ctx.at(c1.at + c1.dur, () => { mode('listen'); hl(0, false); });
    // user 1 (wake word)
    const u1 = c1.at + c1.dur + 250;
    ctx.at(u1, () => { level = 0.9; const b = say('u', 'Maya · on-device speaker ID'); words(b.querySelector('.tt'), 'Hey One, open the urgent one.', 0, 1500, ctx, () => { level = 0.1; mode('think'); }); });
    // One 2 (interrupted)
    ctx.at(c2.at, () => { mode('speak'); hl(0, true); const b = say('a int2'); words(b.querySelector('.tt'), "It's Northwind's stock fix. The draft scored 0.9 — want me to read it?", 0, (c2.dur + c2.cut) * 0.9, ctx); b.id = 'o2'; });
    const cutAt = c2.at + c2.dur;
    ctx.at(cutAt, () => { mode('listen'); level = 0.9; const o = $('#o2'); if (o) { o.classList.add('int'); o.insertAdjacentHTML('beforeend', '<small>— interrupted (barge-in)</small>'); } });
    ctx.at(cutAt + 100, () => { const b = say('u', 'Maya · on-device speaker ID'); words(b.querySelector('.tt'), 'Just approve it.', 0, 900, ctx, () => { level = 0.1; mode('think'); }); });
    // One 3 (confirmation rule)
    ctx.at(c3.at, () => { mode('speak'); const b = say('a'); words(b.querySelector('.tt'), "Approving needs your tap — I've opened it for you.", 0, c3.dur * 0.92, ctx); ctx.pop($('#act'), 600); });
    ctx.at(c3.at + c3.dur, () => mode('listen'));
  };

  S.end = (ctx) => {
    ctx.el.classList.add('endc');
    const logo = h('img', 'pop'); logo.src = 'assets/brand/logo.svg';
    const t = h('h1', 'pop', 'All <em>for</em> One');
    const l = h('div', 'lic pop', 'Open source · Apache-2.0');
    const s = h('div', 'star pop', '<span>★</span> Star us on GitHub');
    ctx.el.append(logo, t, l, s);
    [logo, t, l, s].forEach((n, i) => ctx.pop(n, 100 + i * 450));
  };
})();
