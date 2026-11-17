// The Tickets screen: live All for One tickets and approvals. Rendering only.
// Rust fetches, polls (15 s in view, 30 s hidden), caches and decides; this file draws what it is told.
// All hub text goes in as text nodes (never HTML) with dir="auto", so Arabic reads right to left.
import './tickets.css';
import { listen } from '@tauri-apps/api/event';
import { api, HubApproval, HubPulse, HubSnapshot, HubTicket, HubTicketDetail } from '../ipc';
import { h, Kid } from '../team/dom';
import { STEP_ORDER, TS, TStrings } from './strings';

let host: HTMLElement;
let tabBtn: HTMLElement;
let snap: HubSnapshot | null = null;
let detail: { id: string; data: HubTicketDetail | null; error: string } | null = null;
let confirming: string | null = null; // "<ticket id>|<fingerprint>" while "Open this page?" shows
let note = '';
// "Updated Ns ago": Rust says how old the last good answer was when it last polled (a pulse after
// every poll); this only counts the seconds since then so the label never looks frozen.
let okAgoBase: number | null = null;
let okAgoAt = 0;

const L = (): TStrings => TS[document.documentElement.lang === 'ar' ? 'ar' : 'en'];
const key = (a: HubApproval) => `${a.ticket_id}|${a.fingerprint}`;
/** Text that may be Arabic or English: the browser picks the direction per element. */
const t = (cls: string, text: string, tag: 'span' | 'div' | 'b' = 'span') => h(tag, { class: cls, dir: 'auto' }, text || L().unknown);

// ---- the nav tab and its badge -------------------------------------------------

export function renderTab(): void {
  const s = L();
  const needs = snap?.needs_count ?? 0;
  const offline = !!snap && snap.status !== 'connected' && snap.status !== 'connecting';
  const badge = offline ? '!' : needs > 0 ? String(needs) : '';
  tabBtn.replaceChildren(s.tab);
  if (badge) tabBtn.append(h('span', { class: 'nav-badge' + (offline ? ' off' : ''), 'aria-hidden': 'true' }, badge));
  tabBtn.title = snap ? s.badgeTip(needs, snap.tickets.filter(isOpen).length) : s.tab;
  tabBtn.setAttribute('aria-label', badge && !offline ? `${s.tab} (${needs})` : s.tab);
}

const isOpen = (tk: HubTicket) => tk.lifecycle !== 'closed';

// ---- the live step strip ----------------------------------------------------------

/** New -> Needs approval -> ... -> Closed, with the current step marked. "Held" is shown after them when it is the step. */
function strip(tk: HubTicket, s: TStrings): Kid {
  const at = STEP_ORDER.indexOf(tk.lifecycle);
  const items = STEP_ORDER.map((st, i) => {
    // earlier steps count as done only when the hub itself sent the step; a derived step proves nothing before it
    const cls = st === tk.lifecycle ? 'cur' : !tk.lifecycle_derived && at >= 0 && i < at ? 'past' : '';
    return h('li', { class: `step ${cls}${st === 'needs_approval' && cls === 'cur' ? ' warn' : ''}`.trim(), 'aria-current': st === tk.lifecycle ? 'step' : undefined }, s.steps[st]);
  });
  if (tk.lifecycle === 'held') items.push(h('li', { class: 'step cur held', 'aria-current': 'step' }, s.steps.held));
  return h('ol', { class: 'strip', 'aria-label': s.stepsLabel }, ...items);
}

/** What happened last: ack, who approved, how the last message went, and how long ago the step began. */
function lifeFacts(tk: HubTicket, s: TStrings): Kid {
  const d = tk.last_delivery;
  const bits: Kid[] = [];
  if (tk.acked || (d?.kind === 'ack' && d.status !== 'failed')) bits.push(h('span', { class: 'tag ok' }, s.ackSent));
  if (tk.approved_by) bits.push(h('span', { class: 'tag', dir: 'auto' }, s.approvedBy(tk.approved_by)));
  if (d) bits.push(h('span', { class: 'tag' + (d.status === 'failed' ? ' warn' : ''), dir: 'auto' }, s.delivery(d.kind, d.status, s.ago(d.ago_secs))));
  if (tk.lifecycle_ago_secs !== null) bits.push(h('small', { class: 'since' }, s.since(s.ago(tk.lifecycle_ago_secs))));
  return bits.length ? h('div', { class: 'life' }, ...bits) : null;
}

/** The connection state pill and "Updated Ns ago". The pill is Live only when the last poll worked. */
function okAgoNow(): number | null {
  return okAgoBase === null ? null : okAgoBase + Math.floor((performance.now() - okAgoAt) / 1000);
}

function connection(s: TStrings): Kid {
  const st = snap?.status ?? 'connecting';
  const age = okAgoNow();
  const stale = age !== null && age > 45;
  return h('span', { class: 'conn' }, h('span', { class: `pill ${st === 'connected' ? 'live' : st === 'connecting' ? '' : 'bad'}`, role: 'status' }, s.conn[st]),
    h('span', { class: 'stamp' + (stale || st !== 'connected' ? ' stale' : ''), id: 'hub-stamp' }, s.updatedAgo(age)));
}

function tickStamp(): void {
  const el = document.getElementById('hub-stamp');
  if (!el || host.offsetParent === null) return;
  const age = okAgoNow();
  el.textContent = L().updatedAgo(age);
  el.classList.toggle('stale', snap?.status !== 'connected' || (age !== null && age > 45));
}

// ---- pieces -----------------------------------------------------------------------

function banner(s: TStrings): Kid {
  if (!snap || snap.status === 'connected') return null;
  const text = { connecting: s.connecting, offline: s.offline, token_missing: s.tokenMissing, rejected: s.rejected, not_enabled: s.notEnabled, busy: s.busy }[snap.status];
  const showStale = snap.status !== 'connecting' && snap.last_ok_ago_secs !== null && (snap.tickets.length > 0 || snap.approvals.length > 0);
  return h('div', { class: 'hub-banner' + (snap.status === 'connecting' ? '' : ' bad'), role: 'status' }, h('b', null, text), showStale ? h('span', null, ' ' + s.staleNote(s.ago(snap.last_ok_ago_secs))) : null);
}

function field(label: string, value: string): Kid {
  return h('div', { class: 'f' }, h('small', null, label), t('v', value));
}

function expiry(a: HubApproval, s: TStrings): Kid {
  if (a.expires_in_secs === null) return null;
  const gone = a.expires_in_secs <= 0;
  const text = gone ? s.expired : `${s.expires} ${s.inTime(a.expires_in_secs)}`;
  return h('span', { class: 'tag' + (gone || a.expires_in_secs < 3600 ? ' warn' : '') }, text);
}

function openBlock(a: HubApproval, s: TStrings): Kid {
  if (!a.open_url) return h('span', { class: 'note' }, s.cannotOpen);
  if (confirming !== key(a)) {
    return h('button', { class: 'btn primary', type: 'button', onclick: () => { confirming = key(a); note = ''; render(); } }, s.approveBtn);
  }
  const go = () => {
    confirming = null;
    void api.hubOpenApproval(a.ticket_id, a.fingerprint).catch((e) => { note = String(e); }).finally(render);
  };
  return h('div', { class: 'confirm', role: 'group' },
    h('span', { dir: 'auto' }, s.confirmOpen(a.open_url)),
    h('button', { class: 'btn primary', type: 'button', onclick: go }, s.open),
    h('button', { class: 'btn', type: 'button', onclick: () => { confirming = null; render(); } }, s.cancel));
}

function approvalCard(a: HubApproval, s: TStrings, withSummary: boolean): Kid {
  return h('li', { class: 'card approval' },
    h('div', { class: 'head' }, t('who', a.customer_name, 'b'), a.odoo_ticket_id ? h('span', { class: 'tag' }, `#${a.odoo_ticket_id}`) : null, a.kind_label ? t('tag', a.kind_label) : null, expiry(a, s)),
    h('div', { class: 'facts' }, field(s.owner, a.owner_name), field(s.approvers, a.approvers), h('div', { class: 'f' }, h('small', null, s.fingerprint), h('code', { class: 'fp', dir: 'ltr' }, a.fingerprint))),
    h('small', { class: 'lbl' }, s.draft),
    h('div', { class: 'draft', dir: 'auto' }, a.draft || s.noDraft),
    h('div', { class: 'acts' }, openBlock(a, s),
      withSummary ? h('button', { class: 'btn ghost', type: 'button', onclick: () => void openDetail(a.ticket_id) }, s.summary) : null));
}

function ticketRow(tk: HubTicket, s: TStrings): Kid {
  return h('li', null, h('button', { class: 'row', type: 'button', onclick: () => void openDetail(tk.id) },
    h('div', { class: 'r1' }, t('who', tk.customer_name, 'b'), tk.odoo_ticket_id ? h('span', { class: 'tag' }, `#${tk.odoo_ticket_id}`) : null,
      tk.lifecycle === 'needs_approval' ? h('span', { class: 'tag warn' }, s.needsApprovalTag) : null),
    strip(tk, s),
    lifeFacts(tk, s),
    h('div', { class: 'r2' }, field(s.owner, tk.owner_name), field(s.status, tk.status), field(s.waitingOn, tk.waiting_on), field(s.updated, s.ago(tk.updated_ago_secs)))));
}

// ---- the two views ---------------------------------------------------------------------

function listView(s: TStrings): Kid[] {
  const sn = snap;
  if (!sn) return [h('p', { class: 'empty' }, s.loading)];
  const openList = sn.tickets.filter(isOpen);
  const closedList = sn.tickets.filter((tk) => !isOpen(tk));
  return [
    banner(s),
    h('h2', { class: 'sect' }, s.needsApproval, sn.approvals.length ? h('span', { class: 'count' }, String(sn.approvals.length)) : null),
    sn.approvals.length ? h('ul', { class: 'cards', 'data-tour': 'tickets-approvals' }, ...sn.approvals.map((a) => approvalCard(a, s, true))) : h('p', { class: 'empty' }, s.nothingToApprove),
    note ? h('p', { class: 'err', role: 'alert', dir: 'auto' }, note) : null,
    h('h2', { class: 'sect' }, s.openTickets, h('span', { class: 'count' }, String(openList.length))),
    openList.length ? h('ul', { class: 'rows', 'data-tour': 'tickets-list' }, ...openList.map((tk) => ticketRow(tk, s))) : h('p', { class: 'empty' }, s.noTickets),
    closedList.length ? h('h2', { class: 'sect' }, s.recentlyClosed, h('span', { class: 'count' }, String(closedList.length))) : null,
    closedList.length ? h('ul', { class: 'rows' }, ...closedList.map((tk) => ticketRow(tk, s))) : null,
  ];
}

function detailView(s: TStrings, d: NonNullable<typeof detail>): Kid[] {
  const back = h('button', { class: 'btn ghost', type: 'button', onclick: () => { detail = null; confirming = null; note = ''; render(); } }, '← ' + s.back);
  if (d.error) return [back, h('p', { class: 'err', role: 'alert', dir: 'auto' }, d.error)];
  if (!d.data) return [back, h('p', { class: 'empty' }, s.loading)];
  const tk = d.data.ticket;
  const p = d.data.pending;
  return [
    back,
    h('div', { class: 'card detail' },
      h('div', { class: 'head' }, t('who', tk.customer_name, 'b'), tk.odoo_ticket_id ? h('span', { class: 'tag' }, `#${tk.odoo_ticket_id}`) : null),
      strip(tk, s),
      lifeFacts(tk, s),
      h('div', { class: 'facts' }, field(s.owner, tk.owner_name), field(s.status, tk.status), field(s.waitingOn, tk.waiting_on), field(s.updated, s.ago(tk.updated_ago_secs))),
      h('small', { class: 'lbl' }, s.summary),
      h('div', { class: 'draft', dir: 'auto' }, tk.summary || s.noSummary)),
    p ? h('ul', { class: 'cards' }, approvalCard(p, s, false)) : null,
    note ? h('p', { class: 'err', role: 'alert', dir: 'auto' }, note) : null,
    h('h2', { class: 'sect' }, s.timeline),
    d.data.timeline.length
      ? h('ol', { class: 'timeline' }, ...d.data.timeline.map((e) => h('li', null, t('k', e.label || e.kind, 'b'), ' ', t('a', e.actor), ' ', h('small', null, s.ago(e.ago_secs)))))
      : h('p', { class: 'empty' }, s.noTimeline),
  ];
}

function render(): void {
  const s = L();
  renderTab();
  const refresh = h('button', { class: 'btn', type: 'button', onclick: () => void refreshNow() }, s.refresh);
  const top = h('div', { class: 'thead' }, h('h1', null, s.title), h('span', { class: 'note' }, s.readOnly), connection(s), refresh);
  const scroll = host.scrollTop;
  host.replaceChildren(top, h('main', null, ...(detail ? detailView(s, detail) : listView(s))));
  host.scrollTop = scroll;
}

async function openDetail(id: string): Promise<void> {
  detail = { id, data: null, error: '' };
  note = '';
  render();
  try {
    const data = await api.hubTicket(id);
    if (detail?.id === id) detail.data = data;
  } catch (e) {
    if (detail?.id === id) detail.error = String(e);
  }
  render();
}

async function refreshNow(): Promise<void> {
  try {
    snap = await api.hubRefresh();
    stamp(snap);
  } catch (e) {
    note = String(e);
  }
  if (detail) await openDetail(detail.id);
  else render();
}

function stamp(x: { last_ok_ago_secs: number | null }): void {
  okAgoBase = x.last_ok_ago_secs;
  okAgoAt = performance.now();
}

/** The tab was opened: show what Rust has right now (it polls on its own). */
export function showTickets(): void {
  void api.hubSnapshot().then((s) => { snap = s; stamp(s); render(); }).catch(() => render());
}

export async function initTickets(el: HTMLElement, tab: HTMLElement): Promise<void> {
  host = el;
  tabBtn = tab;
  render();
  // Rust pushes after every poll; the badge stays right even while another tab is showing.
  await listen<HubSnapshot>('hub-update', (e) => {
    snap = e.payload;
    stamp(snap);
    if (confirming && !snap.approvals.some((a) => key(a) === confirming)) confirming = null;
    render();
  });
  // A poll that found nothing new sends only this: the label and the state stay honest.
  await listen<HubPulse>('hub-pulse', (e) => {
    stamp(e.payload);
    if (snap && snap.status !== e.payload.status) { snap.status = e.payload.status; render(); }
    else tickStamp();
  });
  window.setInterval(tickStamp, 1000); // only moves the seconds of one label, and only while this screen shows
  showTickets();
}
