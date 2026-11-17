// The Team home screen: who needs whom. All board text is rendered as text nodes.
import './team.css';
import { listen } from '@tauri-apps/api/event';
import { api, Ask, HubBrief, Person, TeamView } from '../ipc';
import { h, Kid } from './dom';
import { Lang, Strings, T } from './i18n';

type Theme = 'light' | 'dark';
interface Prefs {
  lang: Lang;
  theme: Theme;
  scope: 'mine' | 'team';
  view: 'cards' | 'graph';
  helpers: boolean;
  open: Record<string, boolean>;
}

const DEFAULTS: Prefs = { lang: 'en', theme: 'dark', scope: 'team', view: 'cards', helpers: false, open: {} };

// Prefs live in Rust (settings.json). This is only the in-memory copy; nothing is persisted here.
let prefs: Prefs = { ...DEFAULTS };
let data: TeamView | null = null;
let tabTitles = new Map<string, string>();
let host: HTMLElement;
let hooks: { onAsk: (prompt: string) => void; onAskSession: (sessionId: string) => void; onPrefs: () => void; onOpenTickets: () => void };
const NO_HUB: HubBrief = { status: 'connecting', needs: [], needs_count: 0, last_ok_ago_secs: null };
let toastTimer: number | undefined;
let toastEl: HTMLElement | null = null;
const busy = new Set<string>();

function save(): void {
  void api.setTeamPrefs(prefs).catch(() => {});
}

export const strings = (): Strings => T[prefs.lang];

/** Language, direction and theme apply to the whole window, not only this view. */
export function applyPrefs(): void {
  const de = document.documentElement;
  de.lang = prefs.lang;
  de.dir = prefs.lang === 'ar' ? 'rtl' : 'ltr';
  de.dataset.theme = prefs.theme;
}

// ---- names and sentences -----------------------------------------------------

const lower = (s: string) => s.toLowerCase();
const isMe = (name: string) => !!data && lower(name) === lower(data.me);

function person(name: string): Person {
  const found = data?.people.find((p) => lower(p.name) === lower(name));
  return (
    found ?? {
      name, name_ar: null, role: '', role_ar: null, kind: 'person', working_on: '', working_on_ar: null, status: 'active', gender: 'm',
    }
  );
}

const nm = (name: string) => (prefs.lang === 'ar' && person(name).name_ar) || name;
const what = (a: Ask) => (prefs.lang === 'ar' && a.what_ar) || a.what;
const mark = (t: string) => h('mark', null, t);

/** "X needs Y for Z"; "You need ..." / "... needs you ..." when it is about the signed-in person. */
function sentence(a: Ask): Kid[] {
  const l = strings();
  const me = isMe(a.who);
  if (prefs.lang === 'en') {
    return [mark(me ? l.needYou : nm(a.who)), me ? ' need ' : ' needs ', mark(isMe(a.whom) ? l.toYou : nm(a.whom)), ` for ${what(a)}`];
  }
  const verb = me || person(a.who).gender === 'f' ? l.needsF : l.needs;
  const target = isMe(a.whom) ? [mark(l.toYou)] : [`${l.toWord} `, mark(nm(a.whom))];
  return [mark(me ? l.needYou : nm(a.who)), ` ${verb} `, ...target, ` ${l.forWord} ${what(a)}`];
}

const askPrompt = (a: Ask) => `Tell me about: ${a.who} needs ${a.whom} for ${a.what}`;

// ---- actions ---------------------------------------------------------------------

/** Remove the toast for good: it must not come back on the next redraw (CR-M5). */
function dismissToast(): void {
  window.clearTimeout(toastTimer);
  toastEl?.remove();
  toastEl = null;
}

function toast(msg: string, undoId?: string): void {
  dismissToast();
  const l = strings();
  toastEl = h('div', { class: 'toast on', role: 'status', 'aria-live': 'polite' }, h('span', null, msg));
  if (undoId) {
    toastEl.append(
      h('button', { onclick: () => void undo(undoId) }, l.undo),
    );
  }
  host.append(toastEl);
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(dismissToast, 5000);
}

async function undo(id: string): Promise<void> {
  try {
    await api.teamSetDone(id, false);
    dismissToast();
    await reload();
  } catch (e) {
    toast(String(e));
  }
}

async function markDone(a: Ask): Promise<void> {
  try {
    await api.teamSetDone(a.id, true);
    await reload();
    toast(strings().markedDone, a.id);
  } catch (e) {
    toast(String(e));
  }
}

async function remind(a: Ask): Promise<void> {
  if (busy.has(a.id)) return;
  busy.add(a.id);
  render();
  try {
    await api.teamRemind(a.id);
    toast(strings().remSent(nm(a.whom)));
  } catch (e) {
    toast(String(e));
  } finally {
    busy.delete(a.id);
    await reload();
  }
}

// ---- pieces ------------------------------------------------------------------------

function seg(label: string, items: [string, string, boolean, () => void][], extra: Record<string, string> = {}): HTMLElement {
  return h(
    'div',
    { class: 'seg', role: 'group', 'aria-label': label, ...extra },
    ...items.map(([key, text, on, fn]) => h('button', { 'data-k': key, 'aria-pressed': on, onclick: fn }, text)),
  );
}

function set<K extends keyof Prefs>(k: K, v: Prefs[K]): void {
  prefs[k] = v;
  save();
  applyPrefs();
  hooks.onPrefs();
  render();
}

function itemEl(a: Ask, withAsk = true): HTMLElement {
  const l = strings();
  const sent = a.reminded_recently;
  const meta = h(
    'div',
    { class: 'meta' },
    h('span', { class: `age${a.amber ? ' old' : ''}` }, l.ageF(a.age_hours)),
    a.long && h('span', { class: 'tag warn' }, l.long),
    h('span', { class: 'tag' }, l[a.kind]),
    a.ticket_id && h('span', { class: 'tag' }, a.ticket_id),
    a.auto_reminded && h('span', { class: 'tag' }, l.autoSent),
  );
  const acts = h(
    'div',
    { class: 'acts' },
    h('button', { class: 'btn primary', 'data-k': `done-${a.id}`, onclick: () => void markDone(a) }, l.done),
    h(
      'button',
      { class: `btn${sent ? ' done' : ''}`, 'data-k': `rem-${a.id}`, 'aria-disabled': sent || busy.has(a.id), onclick: () => !sent && void remind(a) },
      sent ? `✓ ${l.reminded}` : l.remind,
    ),
    withAsk && h('button', { class: 'btn ghost', onclick: () => hooks.onAsk(askPrompt(a)) }, l.askAbout),
  );
  return h('li', { class: `item${a.amber ? ' amber' : ''}`, 'data-id': a.id }, h('p', { class: 't' }, ...sentence(a)), meta, acts);
}

function listEl(asks: Ask[]): HTMLElement {
  return asks.length ? h('ul', { class: 'items' }, ...asks.map((a) => itemEl(a))) : h('p', { class: 'empty' }, strings().nothing);
}

/** Why the hub's side cannot be vouched for (empty when it answered). */
function hubStateText(hub: HubBrief): string {
  const l = strings();
  if (hub.status === 'connected') return '';
  return { connecting: l.hubChecking, token_missing: l.hubTokenMissing, offline: l.hubOffline }[hub.status as 'connecting' | 'token_missing' | 'offline'] ?? l.hubOther;
}

/** "Hub tickets need you: N", with ids and one-line summaries. One click opens the Tickets screen. */
function hubCard(hub: HubBrief): Kid {
  if (!hub.needs_count) return false;
  const l = strings();
  const rows = hub.needs.map((n) => h('li', { dir: 'auto' }, h('b', null, n.odoo_ticket_id ? `#${n.odoo_ticket_id}` : `#${n.ticket_id}`), n.customer_name ? ` ${n.customer_name}` : '', n.summary ? ` — ${n.summary}` : ''));
  const more = hub.needs_count - hub.needs.length;
  return h('button', { class: 'hubcard', type: 'button', 'data-k': 'hub-needs', onclick: () => hooks.onOpenTickets() },
    h('div', { class: 'hubcard-t' }, l.hubNeeds(hub.needs_count)),
    h('ul', null, ...rows),
    more > 0 && h('div', { class: 'hubcard-more' }, l.hubMore(more)),
    h('div', { class: 'hubcard-go' }, `${l.hubOpenTickets} →`));
}

/**
 * The top line. "All caught up" only when Rust says so: nothing on you from the team AND from the
 * hub, and the hub answered. Otherwise it says what is waiting, or that the hub could not be checked.
 */
function summary(d: TeamView): HTMLElement {
  const l = strings();
  const onMe = d.asks.filter((a) => isMe(a.whom));
  const old = onMe.filter((a) => a.amber).length;
  const hubText = hubStateText(d.hub);
  const headline = onMe.length ? l.big(onMe.length) : d.hub.needs_count ? l.hubNeeds(d.hub.needs_count) : d.caught_up ? l.allClear : hubText;
  return h(
    'section',
    { class: 'summary', 'aria-label': 'Summary' },
    h('div', { class: 'big' }, headline),
    old > 0 && h('p', null, h('span', { class: 'stale' }, l.stale(old))),
    hubText && headline !== hubText && h('p', null, h('span', { class: 'stale' }, hubText)),
    hubCard(d.hub),
  );
}

function tools(): HTMLElement {
  const l = strings();
  const kids: Kid[] = [seg('Scope', [['mine', l.mine, prefs.scope === 'mine', () => set('scope', 'mine')], ['team', l.team, prefs.scope === 'team', () => set('scope', 'team')]])];
  if (prefs.scope === 'team') {
    kids.push(seg('View', [['cards', l.cards, prefs.view === 'cards', () => set('view', 'cards')], ['graph', l.graph, prefs.view === 'graph', () => set('view', 'graph')]]));
    kids.push(
      h('button', { class: 'switch', role: 'switch', 'data-k': 'helpers', 'aria-checked': prefs.helpers, onclick: () => set('helpers', !prefs.helpers) }, h('i', { 'aria-hidden': 'true' }), l.helpers),
    );
  }
  kids.push(h('button', { class: 'btn primary sp', onclick: () => hooks.onAsk('Tell me about the team board: who needs whom right now, and what is waiting longest.') }, l.ask));
  return h('div', { class: 'tools' }, ...kids);
}

function mineView(asks: Ask[]): Kid[] {
  const l = strings();
  const onMe = asks.filter((a) => isMe(a.whom));
  const byMe = asks.filter((a) => isMe(a.who));
  return [
    h('h2', { class: 'sect big2' }, `${l.waitYou} (${onMe.length})`),
    listEl(onMe),
    h('h2', { class: 'sect big2 gap' }, `${l.waitOthers} (${byMe.length})`),
    listEl(byMe),
  ];
}

function cardEl(p: Person, asks: Ask[]): HTMLElement {
  const l = strings();
  const onMe = asks.filter((a) => lower(a.whom) === lower(p.name));
  const byMe = asks.filter((a) => lower(a.who) === lower(p.name));
  const hot = onMe.some((a) => a.amber);
  const tab = tabTitles.get(lower(p.name));
  const open = !!prefs.open[p.name];
  const role = (prefs.lang === 'ar' && p.role_ar) || p.role;
  const now = (prefs.lang === 'ar' && p.working_on_ar) || p.working_on;
  const card = h(
    'article',
    { class: `tcard ${p.kind === 'agent' ? 'agent' : 'person'}` },
    h(
      'div',
      { class: 'who' },
      h('div', { class: 'av', 'aria-hidden': 'true' }, Array.from(nm(p.name))[0] ?? '?'),
      h(
        'div',
        null,
        // A session's terminal TAB TITLE is what people know it by: title first, name second.
        h('h2', null, (tab ?? nm(p.name)) + (isMe(p.name) ? ` (${l.you})` : '')),
        tab && h('div', { class: 'role' }, nm(p.name)),
        h('div', { class: 'role' }, h('span', { class: `dot${p.status === 'waiting' ? ' wait' : ''}` }), ` ${role} · ${p.status === 'waiting' ? l.waiting : l.active}`),
      ),
    ),
    now && h('div', { class: 'now' }, l.working),
    now && h('p', { class: 'now-text', dir: 'auto' }, now),
    h('div', { class: 'chips' }, h('span', { class: `chip${hot ? ' hot' : ''}` }, `${onMe.length} ${l.onThem}`), h('span', { class: 'chip' }, `${byMe.length} ${l.onOthers}`)),
    h('button', { class: 'btn expand', 'data-k': `open-${p.name}`, 'aria-expanded': open, onclick: () => { if (open) delete prefs.open[p.name]; else prefs.open[p.name] = true; save(); render(); } }, open ? l.hide : l.show),
  );
  if (open) {
    card.append(
      h('div', { class: 'sect' }, l.waitYou),
      listEl(onMe),
      h('div', { class: 'sect' }, l.waitOthers),
      listEl(byMe),
      h('div', { class: 'foot' }, h('button', { class: 'btn ghost', onclick: () => hooks.onAsk(`Tell me about ${p.name}.`) }, l.askAbout)),
    );
  }
  return card;
}

/** Projects (from the status notes) and sessions: shown in every scope and view, whatever the helpers switch says. */
const projectItems = (): Person[] => (data?.people ?? []).filter((p) => p.kind === 'project' || p.kind === 'session');

function projectCard(p: Person): HTMLElement {
  const l = strings();
  const waiting = p.status === 'waiting';
  return h(
    'article',
    { class: 'tcard agent' },
    h(
      'div',
      { class: 'who' },
      h('div', { class: 'av', 'aria-hidden': 'true' }, Array.from(nm(p.name))[0] ?? '?'),
      h('div', null, h('h2', { dir: 'auto' }, nm(p.name)), h('div', { class: 'role' }, h('span', { class: `dot${waiting ? ' wait' : ''}` }), ` ${p.kind === 'session' ? `${l.sessionRole} · ` : ''}${waiting ? l.waiting : l.active}`)),
    ),
    p.working_on && h('p', { class: 'now-text', dir: 'auto' }, p.working_on),
    // a session is asked about through the focus path in Rust: no text of it goes into a message
    p.kind === 'session' && p.session_id && h('div', { class: 'foot' }, h('button', { class: 'btn ghost', onclick: () => hooks.onAskSession(p.session_id ?? '') }, l.askAbout)),
  );
}

function projectsSection(): Kid {
  const items = projectItems();
  if (!items.length) return false;
  const l = strings();
  return h('section', { class: 'panel' }, h('h2', null, l.projects), h('div', { class: 'grid' }, ...items.map(projectCard)));
}

function visiblePeople(): Person[] {
  const all = data?.people ?? [];
  const list = prefs.helpers ? all : all.filter((p) => p.kind === 'person');
  return [...list.filter((p) => p.kind === 'person'), ...list.filter((p) => p.kind === 'agent')];
}

const cardsView = (asks: Ask[]) => h('div', { class: 'grid' }, ...visiblePeople().map((p) => cardEl(p, asks)));

function arrow(): SVGElement {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  s.setAttribute('class', 'arr');
  s.setAttribute('viewBox', '0 0 34 16');
  s.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  p.setAttribute('d', 'M0 8h30M23 2l7 6-7 6');
  p.setAttribute('fill', 'none');
  p.setAttribute('stroke', 'currentColor');
  p.setAttribute('stroke-width', '2.2');
  p.setAttribute('stroke-linecap', 'round');
  p.setAttribute('stroke-linejoin', 'round');
  s.append(p);
  return s;
}

function graphView(asks: Ask[]): Kid[] {
  const l = strings();
  const counts = new Map<string, number>();
  for (const a of asks) {
    const k = person(a.whom).name;
    if (!prefs.helpers && person(a.whom).kind === 'agent') continue;
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const rows = [...counts.entries()].sort((a, b) => b[1] - a[1]);
  const max = rows[0]?.[1] ?? 1;
  const bars = rows.map(([n, c]) =>
    h('div', { class: 'bar' }, h('span', null, nm(n)), h('div', { class: 'track' }, h('div', { class: 'fill', style: `width:${Math.round((c / max) * 100)}%` })), h('b', null, c)),
  );
  const pill = (n: string) => h('span', { class: `pill${person(n).kind === 'agent' ? ' ag' : ''}` }, nm(n));
  const links = [...asks]
    .sort((a, b) => b.age_hours - a.age_hours)
    .map((a) => {
      const row = itemEl(a, false);
      return h('div', { class: 'link', 'data-id': a.id }, pill(a.who), arrow(), pill(a.whom), h('div', { class: 'what' }, ...Array.from(row.querySelectorAll('.t,.meta')), ), row.querySelector('.acts'));
    });
  return [
    h('section', { class: 'panel' }, h('h2', null, l.who), h('p', { class: 'sub' }, l.whoSub), h('div', { class: 'bars' }, ...bars)),
    h('section', { class: 'panel' }, h('h2', null, l.list), h('p', { class: 'sub' }, l.listSub), ...links),
  ];
}

function emptyState(d: TeamView): HTMLElement {
  const l = strings();
  const missing = d.status === 'missing';
  return h(
    'section',
    { class: 'panel empty-state' },
    h('h2', null, missing ? l.emptyTitle : l.errTitle),
    // plain words only: no file path or parser message on screen (it goes to the console)
    h('p', { class: 'sub' }, missing ? l.emptyBody : l.errBody),
  );
}

function header(): HTMLElement {
  const l = strings();
  return h(
    'header',
    { class: 'thead' },
    h('div', { class: 'brand' }, h('div', { class: 'mini-orb', 'aria-hidden': 'true' }), h('div', null, h('h1', null, l.title), h('small', null, l.sub))),
    seg('Language', [['en', 'English', prefs.lang === 'en', () => set('lang', 'en')], ['ar', 'العربية', prefs.lang === 'ar', () => set('lang', 'ar')]]),
    seg('Theme', [['light', l.light, prefs.theme === 'light', () => set('theme', 'light')], ['dark', l.dark, prefs.theme === 'dark', () => set('theme', 'dark')]]),
  );
}

function body(d: TeamView): Kid[] {
  if (d.status !== 'ok') return [hubCard(d.hub), emptyState(d)];
  const l = strings();
  // empty means nothing is SHOWN: projects and sessions always count, people only as the prefs show them
  if (!d.asks.length && !visiblePeople().length && !projectItems().length) {
    return [hubCard(d.hub), h('section', { class: 'panel empty-state' }, h('h2', null, l.emptyBoard), h('p', { class: 'sub' }, l.emptyBoardSub))];
  }
  const main = prefs.scope === 'mine' ? mineView(d.asks) : prefs.view === 'cards' ? [cardsView(d.asks)] : graphView(d.asks);
  return [summary(d), tools(), projectsSection(), ...main, d.dropped > 0 && h('p', { class: 'note' }, l.skipped(d.dropped))];
}

function render(): void {
  const focus = (document.activeElement as HTMLElement | null)?.dataset?.k;
  const top = host.scrollTop;
  host.replaceChildren(header(), h('main', null, ...(data ? body(data) : [h('p', { class: 'empty' }, strings().loading)])));
  if (toastEl) host.append(toastEl);
  host.scrollTop = top;
  if (focus) host.querySelector<HTMLElement>(`[data-k="${CSS.escape(focus)}"]`)?.focus();
}

export async function reload(): Promise<void> {
  try {
    data = await api.teamLoad();
  } catch (e) {
    data = { me: '', status: 'error', path: '', message: String(e), people: [], asks: [], dropped: 0, hub: NO_HUB, caught_up: false };
  }
  await loadTabTitles();
  render();
}

/** Session name (lower-case) to its terminal tab title. Best effort: cards fall back to the name. */
async function loadTabTitles(): Promise<void> {
  try {
    const tabs = await api.listSessions();
    tabTitles = new Map(tabs.filter((t) => t.name && t.tab_title).map((t) => [t.name!.toLowerCase(), t.tab_title]));
  } catch {
    tabTitles = new Map();
  }
}

export async function initTeam(el: HTMLElement, h2: typeof hooks): Promise<void> {
  host = el;
  hooks = h2;
  try {
    prefs = { ...DEFAULTS, ...(await api.getTeamPrefs()) };
  } catch {
    prefs = { ...DEFAULTS };
  }
  applyPrefs();
  render();
  void reload();
  // Rust says when to refresh (once a minute); the webview has no timer for it.
  void listen('team-tick', () => host.offsetParent !== null && void reload());
  // The hub's side (tickets waiting for a person, or the hub being unreachable) changes this screen too.
  void listen('hub-update', () => host.offsetParent !== null && void reload());
}
