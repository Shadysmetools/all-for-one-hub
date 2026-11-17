// Scenes 1/2: helpers + title .. confirm. Every scene is data-driven from the timeline (ctx.D.steps[n].payload).
const SCN = (window.SCN = { scenes: {} });
const h = (t, c, html) => { const e = document.createElement(t); if (c) e.className = c; if (html != null) e.innerHTML = html; return e; };
SCN.h = h;
SCN.esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
SCN.clean = (s) => String(s).replace(/�/g, '—').replace(/https?:\/\/\S+/g, '').replace(/\s+/g, ' ').trim();
const { esc, clean } = SCN;
const P = (ctx, n) => ctx.D.steps[n].payload;
SCN.P = P;
SCN.waveHtml = (n) => Array.from({ length: n }, (_, i) => `<s style="height:${10 + Math.round(30 * Math.abs(Math.sin(i * 0.7) * Math.cos(i * 0.31)))}px"></s>`).join('');
SCN.mic = '<svg viewBox="0 0 24 24"><path d="M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11z"/></svg>';
SCN.cursorSvg = '<svg class="cursor" viewBox="0 0 24 24"><path d="M5 3l14 8-6 2 4 7-3 1.5-4-7-5 4z" fill="#fff" stroke="#050A18" stroke-width="1.2" stroke-linejoin="round"/></svg>';

SCN.phone = (sub = 'client chat') => {
  const el = h('div', 'phone', `<div class="ph-head"><div class="av">N</div><div><b>Northwind Supplies Support</b><small>${sub}</small></div></div><div class="ph-body"></div><div class="ph-input">Message</div>`);
  return { el, body: el.querySelector('.ph-body') };
};
SCN.bubble = (ctx, body, cls, html, ms) => {
  const b = h('div', `b ${cls} pop`, html);
  body.append(b);
  ctx.pop(b, ms);
  return b;
};
SCN.voiceBubble = (ctx, body, ms, played) => {
  const b = SCN.bubble(ctx, body, 'me', `<div class="vn"><div class="play"><i></i></div><div class="wave">${SCN.waveHtml(30)}</div></div><small>0:04</small>`, ms);
  const bars = [...b.querySelectorAll('.wave s')];
  if (played) bars.forEach((x) => x.classList.add('on'));
  return { b, bars };
};
SCN.countUp = (ctx, el, to, ms, fmt, startMs) => ctx.at(startMs, () => {
  const t0 = performance.now();
  const f = () => {
    const p = Math.min(1, (performance.now() - t0) / ms), e = 1 - Math.pow(1 - p, 3);
    el.textContent = fmt(to * e);
    if (p < 1) ctx.raf(f);
  };
  f();
});
const S = SCN.scenes;

S.title = (ctx) => {
  ctx.el.classList.add('titlec');
  const logo = h('img', 'pop'); logo.src = 'assets/brand/logo.svg';
  const t = h('h1', 'pop', 'All <em>for</em> One');
  const p = h('p', 'pop', 'WhatsApp → helpdesk, with AI that <b>drafts</b> and humans who <b>approve</b>');
  ctx.el.append(logo, t, p);
  ctx.pop(logo, 100); ctx.pop(t, 600); ctx.pop(p, 1300);
};

S.voice = (ctx) => {
  const pl = P(ctx, 1);
  const ph = SCN.phone();
  const { bars } = SCN.voiceBubble(ctx, ph.body, 500, false);
  const card = h('div', 'card side pop', `<div class="kick">Step 1 · Client</div><h2>Voice note arrives</h2>
    <dl class="kv"><dt>From</dt><dd>${esc(pl.from)}</dd><dt>Kind</dt><dd>${esc(pl.kind)}</dd><dt>Size</dt><dd>${pl.audio.bytes.toLocaleString('en-US')} bytes</dd><dt>Clip</dt><dd>${pl.audio.synthetic ? 'synthetic (demo)' : 'recorded'}</dd></dl>`);
  ctx.el.append(ph.el, card);
  ctx.pop(card, 900);
  ctx.at(1000, () => { const t0 = performance.now(); const f = () => { const p = Math.min(1, (performance.now() - t0) / 1800); bars.forEach((x, i) => x.classList.toggle('on', i / bars.length < p)); if (p < 1) ctx.raf(f); }; f(); });
};

S.transcribe = (ctx) => {
  const pl = P(ctx, 2);
  const ph = SCN.phone();
  SCN.voiceBubble(ctx, ph.body, 0, true).b.classList.add('in');
  const tx = h('div', 'tx', '');
  const card = h('div', 'card side pop', `<div class="kick">Step 2 · Hub</div><h2>Live transcription</h2>`);
  card.append(tx, h('div', 'hint', `speech-to-text: ${esc(pl.provider)}`));
  ctx.el.append(ph.el, card);
  ctx.pop(card, 200);
  ctx.type(tx, pl.transcript, 700, Math.min(2000, ctx.dur - 1300));
};

S.analysis = (ctx) => {
  const pl = P(ctx, 3);
  const col = h('div', 'col');
  const q = h('div', 'quote pop', esc(P(ctx, 2).transcript));
  const chips = h('div', 'chips');
  const items = [['Nature', pl.nature, ''], ['Type', pl.ticket_type, 'flare'], ['Module', pl.module, ''], ['Track', pl.track, 'teal']];
  const nodes = items.map(([k, v, c]) => h('div', `chip ${c} pop`, `<small>${k}</small><b>${esc(v)}</b>`));
  chips.append(...nodes);
  col.append(q, chips);
  ctx.el.append(col);
  ctx.pop(q, 100);
  nodes.forEach((n, i) => ctx.pop(n, 700 + i * 450));
};

S.ticket = (ctx) => {
  const pl = P(ctx, 4);
  const mail = '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 7l9 7 9-7M4 20L20 4"/></svg>';
  const shield = '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/><path d="M8.5 12l2.5 2.5 4.5-5"/></svg>';
  const card = h('div', 'card ticket pop', `<div class="stamp">CREATED</div><div class="kick">Step 4 · Helpdesk</div>
    <div class="tk-head"><span class="tk-id">#${pl.ticket_id}</span><span class="chip">${esc(P(ctx, 3).ticket_type)} · ${esc(P(ctx, 3).module)}</span></div>
    <div class="tk-title">${esc(P(ctx, 2).transcript)}</div>
    <dl class="kv"><dt>Team</dt><dd>${pl.team_id}</dd><dt>Stage</dt><dd>${pl.stage_id} (silent)</dd></dl>`);
  const badges = h('div', 'badges');
  const b1 = h('span', 'chip flare big pop', `${mail} mail suppressed`);
  const b2 = h('span', 'chip teal big pop', `${shield} guarded write`);
  badges.append(b1, b2); card.append(badges);
  ctx.el.append(card);
  ctx.pop(card, 100); ctx.pop(b1, 1200); ctx.pop(b2, 1700);
};

S.assign = (ctx) => {
  const pl = P(ctx, 5);
  const reasons = (pl.rationale.split('because:')[1] || '').split(';').map((x) => x.trim()).filter(Boolean);
  const card = h('div', 'card assign pop', `<div class="kick">Step 5 · Assignment</div><div class="who"><div class="av xl">${esc(pl.owner[0])}</div><h2>Assigned to ${esc(pl.owner)}</h2></div><div class="kick" style="color:var(--dim)">Why</div>`);
  const ul = h('ul', 'reasons');
  const lis = reasons.map((r) => h('li', 'pop', `<span class="ck">✓</span><span>${esc(r[0].toUpperCase() + r.slice(1))}</span>`));
  ul.append(...lis); card.append(ul);
  const team = h('div', 'team pop');
  ['Alex', 'Maya', 'Sam'].forEach((n) => team.append(h('div', 'tm' + (n === pl.owner ? ' sel' : ''), `<div class="av">${n[0]}</div>${n}`)));
  ctx.el.append(card, team);
  ctx.pop(card, 100); ctx.pop(team, 400);
  lis.forEach((l, i) => ctx.pop(l, 900 + i * 600));
};

S.ack = (ctx) => {
  const pl = P(ctx, 6);
  const ph = SCN.phone();
  SCN.voiceBubble(ctx, ph.body, 0, true).b.classList.add('in');
  const typing = SCN.bubble(ctx, ph.body, 'them', '<div class="typing"><i></i><i></i><i></i></div>', 300);
  ctx.at(1300, () => {
    typing.innerHTML = esc(clean(pl.text)) + '<small>instant ack</small>';
    typing.style.textAlign = 'left';
  });
  const tag = h('div', 'ack-tag pop', `<div class="kick">Step 6 · Hub</div><h2>The <span>only</span> automatic message</h2><p>${P(ctx, 13).automatic_messages} automatic message in the whole run. Everything else waits for a human.</p>`);
  ctx.el.append(ph.el, tag);
  ctx.pop(tag, 1500);
};

S.confirm = (ctx) => {
  const pl = P(ctx, 7), j = P(ctx, 8);
  const thr = parseFloat((ctx.D.steps[8].summary.match(/threshold ([\d.]+)/) || [0, 0.7])[1]);
  const draft = h('div', 'card draft pop', `<div class="kick">Step 7 · CONFIRM draft</div><span class="chip flare">held for owner approval</span><p>${esc(clean(pl.draft))}</p>`);
  const R = 160, pt = (v, r) => `${(200 - r * Math.cos(Math.PI * v)).toFixed(1)},${(200 - r * Math.sin(Math.PI * v)).toFixed(1)}`;
  const arc = 'M40 200 A160 160 0 0 1 360 200';
  const gauge = h('div', 'gauge pop', `<div class="gwrap"><svg viewBox="0 0 400 240"><defs><linearGradient id="gg" x1="0" x2="1"><stop offset="0" stop-color="#4DA3FF"/><stop offset="1" stop-color="#3FE0C5"/></linearGradient></defs>
    <path d="${arc}" fill="none" stroke="#122046" stroke-width="26" stroke-linecap="round" pathLength="100"/>
    <path id="garc" d="${arc}" fill="none" stroke="url(#gg)" stroke-width="26" stroke-linecap="round" pathLength="100" stroke-dasharray="0 100" style="transition:stroke-dasharray 1.4s cubic-bezier(.2,.8,.2,1)"/>
    <line x1="${pt(thr, R - 24).split(',')[0]}" y1="${pt(thr, R - 24).split(',')[1]}" x2="${pt(thr, R + 26).split(',')[0]}" y2="${pt(thr, R + 26).split(',')[1]}" stroke="#FFB547" stroke-width="4"/>
    <text x="${pt(thr, R + 44).split(',')[0]}" y="${pt(thr, R + 44).split(',')[1]}" fill="#FFB547" font-size="15" font-family="Consolas,monospace" text-anchor="middle">${thr.toFixed(2)}</text></svg>
    <div class="g-val" id="gval">0.00</div></div>
    <div class="g-sub">judge score · threshold ${thr.toFixed(2)}</div>`);
  const chip = h('span', 'chip teal big pop', j.flag ? 'flagged for review' : '✓ above threshold · no flag');
  const mini = h('div', 'mini');
  Object.entries(j.scores).forEach(([k, v]) => mini.append(h('div', 'mrow', `<span>${k}</span><div class="mbar"><i data-w="${v * 100}"></i></div><span>${v.toFixed(2)}</span>`)));
  gauge.append(chip, mini);
  ctx.el.append(draft, gauge);
  ctx.pop(draft, 100); ctx.pop(gauge, 1800);
  ctx.at(2300, () => ctx.el.querySelector('#garc').setAttribute('stroke-dasharray', `${j.aggregate * 100} 100`));
  SCN.countUp(ctx, ctx.el.querySelector('#gval'), j.aggregate, 1400, (v) => v.toFixed(2), 2300);
  ctx.pop(chip, 3900);
  ctx.at(3000, () => ctx.el.querySelectorAll('.mbar i').forEach((i) => (i.style.width = i.dataset.w + '%')));
};
