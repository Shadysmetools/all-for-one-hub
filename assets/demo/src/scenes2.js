// Scenes 2/2: approve .. arabic.
(() => {
  const { h, esc, clean, P } = SCN;
  const S = SCN.scenes;

  S.approve = (ctx) => {
    const sent = P(ctx, 9).sent;
    const owner = (ctx.D.steps[9].summary.match(/^(\w+)/) || [0, 'Owner'])[1];
    const ex = esc(clean(sent).split(/(?<=[.?!])\s/)[0]);
    const win = h('div', 'chatwin pop', `<div class="cw-head"><div class="av">1</div><div>Approvals<small>owner chat · approval bot</small></div></div>
      <div class="cw-body"><div class="cb pop" id="ap-msg"><b>Approval needed</b> · ticket #${P(ctx, 4).ticket_id}<br>CONFIRM draft for the client
      <p class="ex">“${ex}”</p><div class="btns"><div class="btn ok" id="ap-ok">✓ Approve</div><div class="btn">Edit</div><div class="btn">Reject</div></div></div>
      ${SCN.cursorSvg}</div>`);
    const wait = h('div', 'wait pop', `<div class="kick">Step 9 · Owner</div><div class="big" id="held">0 s</div><p>held for a human to review. The draft is never sent on its own.</p>`);
    ctx.el.append(win, wait);
    ctx.pop(win, 100); ctx.pop(wait, 500);
    const body = win.querySelector('.cw-body');
    ctx.pop(body.querySelector('#ap-msg'), 500);
    SCN.countUp(ctx, wait.querySelector('#held'), ctx.D.steps[9].offset_s, 1800, (v) => Math.round(v) + ' s', 600);
    const cur = body.querySelector('.cursor');
    ctx.at(1500, () => {
      const b = body.querySelector('#ap-ok').getBoundingClientRect(), r = body.getBoundingClientRect();
      cur.style.left = b.left - r.left + b.width * 0.55 + 'px'; cur.style.top = b.top - r.top + b.height * 0.4 + 'px';
    });
    ctx.at(2700, () => body.querySelector('#ap-ok').classList.add('press'));
    const done = h('div', 'cb sent pop', `✓ Approved by ${esc(owner)} · sent to the client`);
    body.append(done); ctx.pop(done, 3000);
  };

  S.reply = (ctx) => {
    const pl = P(ctx, 10);
    const ph = SCN.phone();
    SCN.bubble(ctx, ph.body, 'them clamp', esc(clean(P(ctx, 9).sent).slice(0, 96) + '…'), 0).classList.add('in');
    SCN.bubble(ctx, ph.body, 'me', esc(pl.reply) + '<small>✓✓</small>', 500);
    const stage = pl.stage.replace('_', ' ');
    const card = h('div', 'card side pop', `<div class="kick">Step 10 · Client</div><h2>Client confirms</h2>
      <div style="display:flex;gap:20px;align-items:center"><span class="chip">awaiting confirmation</span><span style="font-size:34px;color:var(--dim)">→</span><span class="chip teal pop" id="stg">${esc(stage)}</span></div>
      <div class="hint">The owner marks it confirmed, and work starts.</div>`);
    ctx.el.append(ph.el, card);
    ctx.pop(card, 300); ctx.pop(card.querySelector('#stg'), 1500);
  };

  S.proof = (ctx) => {
    const pl = P(ctx, 11);
    const win = h('div', 'screen pop', `<div class="sc-bar"><span class="dot"></span>REC · proof run${pl.simulated ? ' (simulated)' : ''}<span class="tc" id="tc">00:00</span></div>
      <div class="app"><h3>Inventory · Stock on hand</h3>
      <table><tr><th>Product</th><th>On hand</th><th>Incoming</th></tr><tr><td>Widget A</td><td class="q" id="qa">40</td><td>+10</td></tr><tr><td>Widget B</td><td class="q">18</td><td>—</td></tr></table>
      <span class="vbtn" id="vb">Validate delivery</span>${SCN.cursorSvg}<div class="rstamp" id="rs">${pl.verdict} ✓</div>
      <div class="sim">synthetic demo data</div></div><div class="prog"><i id="pb"></i></div>`);
    ctx.el.append(win); ctx.pop(win, 100);
    const cur = win.querySelector('.cursor');
    cur.style.left = '900px'; cur.style.top = '380px';
    ctx.at(600, () => { const b = win.querySelector('#vb').getBoundingClientRect(), r = win.querySelector('.app').getBoundingClientRect(); cur.style.left = b.left - r.left + 90 + 'px'; cur.style.top = b.top - r.top + 20 + 'px'; });
    ctx.at(1700, () => win.querySelector('#vb').classList.add('press'));
    ctx.at(2000, () => { const q = win.querySelector('#qa'); q.textContent = '42'; q.classList.add('bad'); });
    ctx.at(3300, () => win.querySelector('#rs').classList.add('in'));
    ctx.at(200, () => {
      const t0 = performance.now(), total = ctx.dur - 600;
      const f = () => { const p = Math.min(1, (performance.now() - t0) / total); win.querySelector('#pb').style.width = p * 100 + '%'; win.querySelector('#tc').textContent = '00:0' + Math.min(9, Math.floor(p * 9)); if (p < 1) ctx.raf(f); };
      f();
    });
  };

  S.resolve = (ctx) => {
    const text = clean(P(ctx, 12).sent.split('You can check')[0]);
    const ph = SCN.phone();
    const b = SCN.bubble(ctx, ph.body, 'them', esc(text), 300);
    b.style.maxWidth = '340px';
    const v = h('div', 'vthumb pop', '<div class="play"><i></i></div><small>proof video</small>');
    ph.body.append(v); ctx.pop(v, 1300);
    const owner = (ctx.D.steps[9].summary.match(/^(\w+)/) || [0, 'Owner'])[1];
    const card = h('div', 'card side pop', `<div class="kick">Step 12 · Owner</div><h2>RESOLVE approved and sent</h2>
      <div style="display:flex;flex-direction:column;gap:16px;align-items:flex-start"><span class="chip teal">stage: ${esc(P(ctx, 12).stage)}</span><span class="chip">approved by ${esc(owner)}</span><span class="chip flare">proof video attached</span></div>`);
    ctx.el.append(ph.el, card); ctx.pop(card, 700);
  };

  S.kpi = (ctx) => {
    const st = ctx.D.steps, k = P(ctx, 13).outcome;
    const ack = st[6].offset_s - st[1].offset_s, first = st[9].offset_s - st[1].offset_s, ttr = st[12].offset_s - st[1].offset_s;
    const judge = (P(ctx, 8).aggregate + k.judge_video_score) / 2;
    const dur = (v) => { const m = Math.round(v / 60); return v < 120 ? Math.round(v) + ' s' : Math.floor(m / 60) + ' h ' + String(m % 60).padStart(2, '0') + ' m'; };
    const defs = [['Ack time', ack, dur, 'voice note → instant ack'], ['First response', first, dur, 'voice note → owner approval'], ['Time to resolve', ttr, dur, 'voice note → resolved'], ['Judge average', judge, (v) => v.toFixed(2), `draft ${P(ctx, 8).aggregate.toFixed(2)} · video ${k.judge_video_score.toFixed(2)}`]];
    const tiles = h('div', 'tiles');
    const nodes = defs.map(([l, , , sub]) => h('div', 'card tile pop', `<small>${l}</small><b>0</b><span>${sub}</span>`));
    tiles.append(...nodes);
    const ppl = h('div', 'card people pop', '<h3>Per person <small>resolved this week · judge average · sample team data</small></h3>');
    const rows = [['Alex', 6, '0.86'], ['Maya', 8, judge.toFixed(2)], ['Sam', 5, '0.91']];
    rows.forEach(([n, v, j]) => ppl.append(h('div', 'prow', `<span>${n}</span><div class="pbar"><i data-w="${v * 10}"></i></div><span class="n">${v}</span><span class="j">judge ${j}</span>`)));
    const wrap = h('div', 'kpi'); wrap.append(tiles, ppl); ctx.el.append(wrap);
    nodes.forEach((n, i) => { ctx.pop(n, 150 + i * 250); SCN.countUp(ctx, n.querySelector('b'), defs[i][1], 1300, defs[i][2], 400 + i * 250); });
    ctx.pop(ppl, 1300);
    ctx.at(1700, () => ppl.querySelectorAll('.pbar i').forEach((i) => (i.style.width = i.dataset.w + '%')));
  };

  S.arabic = (ctx) => {
    const text = clean(ctx.D.ar[6].payload.text);
    const ph = SCN.phone('محادثة العميل');
    const b = SCN.bubble(ctx, ph.body, 'them rtl', esc(text) + '<small style="text-align:left">instant ack</small>', 500);
    b.style.maxWidth = '340px';
    const card = h('div', 'card side pop', `<div class="kick">Same flow · <span style="direction:ltr">--lang ar</span></div>
      <h2 dir="rtl" style="font-size:64px">العربية</h2><span class="chip teal big">right-to-left replies</span>
      <div class="hint">Replies go out in the client's language.</div>`);
    ctx.el.append(ph.el, card); ctx.pop(card, 300);
  };
})();
