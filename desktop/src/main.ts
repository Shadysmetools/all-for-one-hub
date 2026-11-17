import './style.css';
import { Mic } from './audio/capture';
import { Player } from './audio/player';
import { api, Card, Latency, MicEvent, MuteOutcome, SessionRoute, SessionTab, TurnEvent, Voice } from './ipc';
import { renderMarkdown } from './md';
import { routeText } from './route';
import { createOrb, OrbState } from './orb/orb';
import { initTeam, reload as reloadTeam, strings } from './team/team';
import { initTickets, renderTab as renderTicketsTab, showTickets } from './tickets/tickets';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const orb = createOrb($<HTMLCanvasElement>('orb'));
// The orb follows the theme the Team screen sets on <html data-theme>.
const syncOrbTheme = () => orb.setLight(document.documentElement.dataset.theme === 'light');
new MutationObserver(syncOrbTheme).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
syncOrbTheme();
const player = new Player();

let muted = true; // the mic is OFF at every launch (Rust starts muted and says the same)
let micLive = false; // audio is being listened to right now (Rust truth, polled): the red dot
let pausedReason = ''; // set when a call switched the mic off; cleared on the next unmute
let turnId = 0; // bumped on every new turn / cancel so stale events and audio are dropped
let turnActive = false;
let rustTurn = 0; // number of the Rust-started turn being shown; 0 = none
let transcribing = false;
let micLevel = 0;
let assistantEl: HTMLElement | null = null;

/**
 * What the user sees for an error: plain words and what to do, never a path, an id, JSON or a
 * developer term. Messages that are already plain pass through; the raw text goes to the console.
 */
export function friendly(raw: string): string {
  const t = String(raw ?? '').replace(/^Error:\s*/, '');
  if (!t) return '';
  const techy = /[A-Za-z]:[\\/]|\/[\w.-]+\/|[{}[\]]|\bjson\b|os error|stderr|exit (code|status)|ONE_CLAUDE_BIN|\bCLI\b|README|ElevenLabs|\bHTTP\b|\b\d{3}\b|[0-9a-f]{8}-[0-9a-f]{4}|NotAllowedError|NotFoundError|NotReadableError|undefined|null/i;
  if (!techy.test(t) && t.length <= 160) return t;
  console.warn('[one]', t);
  if (/microphone|getUserMedia|NotAllowedError|NotFoundError|NotReadableError|audio input/i.test(t)) return "I can't reach your microphone. Check that it is plugged in and that Windows lets apps use it.";
  if (/ONE_CLAUDE_BIN|claude CLI|could not find the claude/i.test(t)) return "One can't start on this computer yet: Claude Code needs to be installed.";
  if (/elevenlabs|tts|voice|speech|audio/i.test(t)) return "I can't speak right now, so I'll answer in text. Check your sound and your internet.";
  if (/network|timed? ?out|connect|dns|offline|unreachable/i.test(t)) return "I can't reach the internet right now. Check your connection and try again.";
  if (/history/i.test(t)) return "I couldn't load our earlier chat. New messages still work.";
  return 'Something went wrong. Please try again.';
}

const setStatus = (t: string) => ($('status').textContent = friendly(t));

/** A red (or, for `ok`, green) toast that dismisses itself after ~8 s. Text only, never HTML. */
function toast(message: string, ok = false, note = false): void {
  const el = document.createElement('div');
  el.className = note ? 'toast note' : ok ? 'toast ok' : 'toast';
  el.textContent = note ? message : friendly(message);
  const box = $('toasts');
  while (box.children.length >= 3) box.firstElementChild?.remove();
  box.appendChild(el);
  setTimeout(() => el.remove(), 8000);
}

const openLink = (url: string) => void api.openLink(url).catch((e) => toast(String(e)));
let linksOff = false; // a locked conversation (another session's text in play): links are plain text
const refreshLock = async () => {
  try {
    linksOff = await api.lockState();
  } catch {
    linksOff = true; // when in doubt, no live links
  }
};

/** A link is opened only after the user has seen where it goes. Text only. */
function confirmLink(url: string): void {
  let host = '';
  try {
    host = new URL(url).hostname;
  } catch {
    return toast('That link cannot be opened.');
  }
  const el = document.createElement('div');
  el.className = 'toast ok confirm';
  const msg = document.createElement('span');
  msg.textContent = `Open ${host} in your browser?`;
  el.append(
    msg,
    button('Open', () => {
      el.remove();
      openLink(url);
    }),
    button('Cancel', () => el.remove()),
  );
  $('toasts').appendChild(el);
  setTimeout(() => el.remove(), 15000);
}

let paintTimer: number | undefined;
/** While a reply streams, redraw at most about 10 times a second. */
function schedulePaint(el: HTMLElement): void {
  if (paintTimer) return;
  paintTimer = window.setTimeout(() => {
    paintTimer = undefined;
    const box = $('transcript');
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    paintOne(el, el.dataset.raw ?? '');
    if (atBottom) box.scrollTop = box.scrollHeight; // follow the reply unless the user scrolled up
  }, 100);
}
function flushPaint(): void {
  if (paintTimer) clearTimeout(paintTimer);
  paintTimer = undefined;
  if (!assistantEl) return;
  const box = $('transcript');
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
  paintOne(assistantEl, assistantEl.dataset.raw ?? '');
  if (atBottom) box.scrollTop = box.scrollHeight;
}

/** Draw One's text as markdown (DOM nodes only). The raw text is kept on the element for streaming. */
function paintOne(el: HTMLElement, raw: string): void {
  el.dataset.raw = raw;
  renderMarkdown(el, raw, confirmLink, { linksOff });
}

/** One chat bubble in the thread, before the typing indicator and the approval cards. */
function addLine(who: 'you' | 'one', text: string): HTMLElement {
  const el = document.createElement('div');
  el.className = `bubble ${who}`;
  el.dir = 'auto'; // an Arabic reply in the English UI (or the reverse) keeps its own direction
  if (who === 'one') paintOne(el, text);
  else el.textContent = text;
  const box = $('transcript');
  box.insertBefore(el, $('typing'));
  box.scrollTop = box.scrollHeight;
  return el;
}

// ---- turns -----------------------------------------------------------------

/** When the running turn last showed any sign of life (the "thinking" fallback timeout uses it). */
let lastTurnEventAt = 0;
/** Rust's watchdog ends a stuck turn by 10 minutes; this is only the last-resort net for the UI. */
const THINKING_GIVE_UP_MS = 11 * 60 * 1000;
const THINKING_SLOW_MS = 30 * 1000;

function onTurnEvent(id: number, e: TurnEvent): void {
  if (id !== turnId) return;
  lastTurnEventAt = Date.now();
  if (e.type === 'delta') {
    assistantEl ??= addLine('one', '');
    assistantEl.dataset.raw = (assistantEl.dataset.raw ?? '') + e.text;
    schedulePaint(assistantEl);
    $('transcript').scrollTop = $('transcript').scrollHeight;
  } else if (e.type === 'cards') {
    flushPaint();
    showCards(e.cards);
  } else if (e.type === 'latency') {
    showLatency(e.latency);
  } else if (e.type === 'notice') {
    toast(e.message, false, true); // calm, not red
  } else if (e.type === 'error') {
    setStatus(e.message);
    toast(e.message);
    if (e.message.startsWith('One could not reach Claude')) addLine('one', e.message).classList.add('interrupted');
  } else if (e.type === 'done') {
    flushPaint();
    void refreshLock(); // the turn may have tainted the conversation
    if (e.interrupted) markInterrupted();
    turnActive = false;
  }
}

/** "Replied in 1.2 s": end of speech (or send) to the first sound of the reply. */
function showLatency(l: Latency): void {
  const el = $('latency');
  const s = (ms: number) => (ms / 1000).toFixed(1);
  el.textContent = `Replied in ${s(l.total_ms)} s`;
  el.title = `Hearing you ${s(l.stt_ms)} s, thinking ${s(l.claude_ms)} s, voice ${s(l.voice_ms)} s. Target: under 1.5 s.`;
  el.classList.toggle('slow', !l.under_target);
}

/** The live line of what the user is saying. It is replaced by the final text (or removed). */
let partialEl: HTMLElement | null = null;
function showPartial(text: string): void {
  partialEl ??= addLine('you', '');
  partialEl.classList.add('partial');
  partialEl.textContent = text;
  $('transcript').scrollTop = $('transcript').scrollHeight;
}
function clearPartial(): void {
  partialEl?.remove();
  partialEl = null;
}

function markInterrupted(): void {
  flushPaint();
  if (assistantEl) assistantEl.classList.add('interrupted');
  if (assistantEl) assistantEl.append(' [interrupted]');
  assistantEl = null;
}

function startTurn(start: (e: (e: TurnEvent) => void, a: (b: ArrayBuffer) => void) => Promise<void>): void {
  void refreshLock();
  const id = ++turnId;
  rustTurn = 0; // a typed / approved turn: its own channels carry its events and audio
  turnActive = true;
  lastTurnEventAt = Date.now();
  assistantEl = null;
  setStatus('');
  start(
    (e) => onTurnEvent(id, e),
    (b) => id === turnId && player.push(b),
  ).catch((err) => {
    if (id === turnId) {
      setStatus(String(err));
      turnActive = false;
    }
  });
}

/** Drop the local view of the turn and stop the audio. Rust has already cancelled it (barge-in, mute). */
function stopLocal(): void {
  const wasActive = turnActive;
  turnId++; // drop anything still in flight
  rustTurn = 0; // and the audio / events of a Rust-started turn
  player.stop();
  if (wasActive) markInterrupted();
  turnActive = false;
  $('cards').replaceChildren();
}

/** Stop button. Always tells Rust: it also drops speech that is still being transcribed. */
function stop(): void {
  void api.cancel();
  stopLocal();
  clearPartial();
  transcribing = false; // Rust dropped any speech that was still being transcribed
}

/** Rust runs the VAD, the STT and the turn itself; we only show what it reports. */
function onMicEvent(e: MicEvent): void {
  if (e.type === 'level') micLevel = e.level;
  else if (e.type === 'partial') {
    setTab('voice');
    showPartial(e.text);
  } else if (e.type === 'barge_in') stopLocal();
  else if (e.type === 'paused') {
    clearPartial();
    onPaused(e.reason);
  }
  else if (e.type === 'blocked') onBlocked(e.reason);
  else if (e.type === 'transcribing') {
    transcribing = e.on;
    if (!e.on) clearPartial(); // the final text (if any) follows as a transcript
  }
  else if (e.type === 'error') {
    setStatus(e.message);
    toast(e.message);
  }
  else if (e.type === 'transcript') {
    setTab('voice'); // the answer shows (and is spoken) on the One tab
    clearPartial();
    addLine('you', e.text);
    $('cards').replaceChildren();
    if (turnActive) markInterrupted(); // Rust cancelled the running turn when it started this one
    turnId++;
    rustTurn = e.turn;
    turnActive = true;
    lastTurnEventAt = Date.now();
    assistantEl = null;
    setStatus('');
  } else if (e.type === 'turn' && e.turn === rustTurn) onTurnEvent(turnId, e.event);
}

/** The mute hotkey was refused because a call is on: say so and offer "Unmute anyway". */
function onBlocked(reason: string): void {
  pausedReason = reason;
  callBlocked = true;
  unmutePending = false;
  paintMic();
}

/** A call was detected: Rust already muted the mic and stopped the turn. Unmute stays manual. */
function onPaused(reason: string): void {
  pausedReason = reason;
  callBlocked = false;
  stopLocal();
  void syncMic();
}

/** Audio of a turn Rust started: dropped unless it belongs to the turn we are showing. */
function onRustAudio(b: ArrayBuffer): void {
  if (rustTurn !== 0) player.push(b);
}

// ---- tabs: Team is home, One is the voice panel --------------------------------

let currentTab: 'team' | 'tickets' | 'voice' = 'team';

/** Tell Rust whether a screen that shows the hub is on screen: it polls every 15 s then, else every 30 s. */
function syncHubView(): void {
  void api.hubSetInView((currentTab === 'team' || currentTab === 'tickets') && !document.hidden).catch(() => {});
}

function setTab(tab: 'team' | 'tickets' | 'voice'): void {
  currentTab = tab;
  syncHubView();
  if (tab === 'voice') void applyRoute();
  document.body.classList.toggle('tab-team', tab === 'team');
  document.body.classList.toggle('tab-tickets', tab === 'tickets');
  $('tab-tickets').setAttribute('aria-pressed', String(tab === 'tickets'));
  $('tab-team').setAttribute('aria-pressed', String(tab === 'team'));
  $('tab-voice').setAttribute('aria-pressed', String(tab === 'voice'));
  if (tab === 'team') void reloadTeam();
  if (tab === 'tickets') showTickets();
}

function syncTabLabels(): void {
  const l = strings();
  $('tab-team').textContent = l.tabTeam;
  $('tab-voice').textContent = l.tabOne;
  renderTicketsTab();
  paintRoute();
  // the chat screen speaks the same language as the board
  $('btn-send').textContent = l.chatSend;
  $<HTMLTextAreaElement>('msg').placeholder = l.chatPlaceholder;
  $('composer-hint').textContent = l.chatHint;
  $('btn-stop').textContent = l.stop;
  $('btn-test-voice').textContent = l.testVoice;
  $('more-label').textContent = l.more;
  $('session-label').textContent = l.askSession;
  $('shrink-label').textContent = l.shrink;
  $('btn-mute').textContent = muted ? l.talk : l.micOnStop;
  const w = document.querySelector<HTMLElement>('#transcript .welcome');
  if (w) paintOne(w, l.greeting);
}

/** "Ask One about this": same run-turn path as speaking; the prompt is the board's safe text. */
function askOne(prompt: string): void {
  setTab('voice');
  // No api.cancel() here: run_turn cancels the running turn itself, and a late cancel IPC
  // could kill the new one. startTurn bumps turnId, so the old turn's events are dropped.
  if (turnActive) markInterrupted();
  player.stop();
  addLine('you', prompt);
  $('cards').replaceChildren();
  startTurn((e, a) => api.runTurn(prompt, e, a));
}

/** "Ask about" on a session card: through the focus path (extract, no write tools, deny-only approvals), never as a typed message that carries that session's text. */
async function askSession(sessionId: string): Promise<void> {
  try {
    await api.focusSession(sessionId); // Rust checks the id is one of this project's sessions
    const sel = $<HTMLSelectElement>('session');
    if (![...sel.options].some((o) => o.value === sessionId)) await fillSessions(); // a session started after launch
    userPicked = true; // an explicit "Ask about X" stays on X: the default does not take it back
    sel.value = sessionId; // the picker shows that One is now reading that session (read-only)
    await refreshLock();
    askOne('What is this session working on right now, and is it waiting for anything?');
  } catch (e) {
    toast(String(e));
  }
}

// ---- approval cards (click required; voice alone never approves) -----------

function showCards(cards: Card[]): void {
  const box = $('cards');
  box.replaceChildren();
  for (const c of cards) {
    const el = document.createElement('div');
    el.className = 'card';
    const h = document.createElement('b');
    h.textContent = c.sentence; // "One wants to ... Allow?" in plain words (text only)
    const tool = document.createElement('i');
    tool.className = 'why';
    tool.textContent = `Uses: ${c.tool_name}`;
    el.append(h, tool);
    // H1: the full command, untruncated, whitespace preserved (pre-wrap), scrolls if long.
    const s = document.createElement('pre');
    s.className = 'cmd';
    s.textContent = c.command ?? '';
    if (c.command !== null) el.append(s);
    if (c.warning) {
      const w = document.createElement('div');
      w.className = 'warn';
      w.textContent = 'Warning: this runs shell operators (; & | > < $( or a backtick). Read it all before you approve.';
      el.append(w);
    }
    // Allow and Deny are both always visible. Allow is live only when the displayed text is exactly
    // the stored command; otherwise it is disabled and the reason is shown.
    const same = c.command === null || s.textContent === c.command;
    const live = c.approvable && same;
    const allow = button('Allow', () => {
      // L3: a second approval would cancel the first approved command mid-run.
      if (turnActive) return setStatus('Wait for the current answer to finish, then allow.');
      el.remove();
      startTurn((e, a) => api.approve(c.tool_use_id, e, a));
    });
    allow.disabled = !live;
    const deny = button('Deny', () => {
      void api.deny(c.tool_use_id);
      el.remove();
    });
    el.append(allow, deny);
    if (!live) {
      const n = document.createElement('i');
      n.className = 'why';
      n.textContent = c.reason ?? 'The command shown may differ from what would run.';
      el.append(n);
    }
    box.appendChild(el);
  }
}

function button(label: string, fn: () => void): HTMLButtonElement {
  const b = document.createElement('button');
  b.textContent = label;
  b.onclick = fn;
  return b;
}

// ---- mic, mute, mode -------------------------------------------------------

// The webview only captures raw PCM (browser echo cancellation) and streams it to Rust.
const mic = new Mic((pcm) => void api.micFrames(pcm).catch(() => {}));
let lastBarge = false;

let micEpoch = 0; // bumped by every local mute change: a poll that started earlier must not undo it
let callBlocked = false; // an unmute was refused because a call is on ("Unmute anyway" is offered)

function paintMic(): void {
  $('btn-mute').textContent = muted ? strings().talk : strings().micOnStop;
  $('btn-mute').classList.toggle('on', !muted);
  document.body.classList.toggle('muted', muted);
  document.body.classList.toggle('call-blocked', muted && callBlocked);
}

let unmutePending = false; // an unmute is waiting for Rust's call check

/** Mute always works. Unmute asks Rust, which refuses while a call is on unless `force`. */
async function setMuted(m: boolean, force = false): Promise<void> {
  const mine = ++micEpoch;
  if (m) {
    unmutePending = false;
    muted = true;
    micLive = false;
    callBlocked = false;
    paintMic();
    stopLocal();
    clearPartial();
    transcribing = false;
    void api.micSetMuted(true); // Rust drops pending speech and cancels the turn
    // M5: muting releases the microphone entirely; unmuting asks for it again.
    void mic.setMuted(true).catch((e) => setStatus(`Microphone unavailable: ${e}`));
    return;
  }
  unmutePending = true;
  let outcome: MuteOutcome = 'superseded';
  try {
    outcome = await api.micSetMuted(false, force);
  } catch (e) {
    setStatus(`Could not unmute: ${e}`);
  }
  // The user pressed something else while Rust was checking: that wins, this reply is stale.
  if (mine !== micEpoch) return;
  unmutePending = false;
  if (outcome === 'blocked') {
    pausedReason = 'Call detected: mic stays off';
    callBlocked = true;
    paintMic();
    return;
  }
  if (outcome !== 'unmuted') return; // superseded in Rust: the next poll shows the truth
  pausedReason = '';
  callBlocked = false;
  muted = false;
  micLive = true; // optimistic; the next poll corrects it (push-to-talk keeps it off until the key is held)
  paintMic();
  void mic.setMuted(false).catch((e) => setStatus(`Microphone unavailable: ${e}`));
}

/** The Mute / Unmute button. While an unmute is still being checked, a press means "mute". */
function toggleMute(): void {
  void setMuted(unmutePending || !muted);
}

/** Rust is the source of truth for the mic (a call can mute it, push-to-talk can open it). */
async function syncMic(): Promise<void> {
  const epoch = micEpoch;
  try {
    const st = await api.micState();
    if (epoch !== micEpoch) return; // the user clicked meanwhile: this answer is stale
    if (st.muted !== muted) {
      muted = st.muted;
      if (muted) stopLocal(); // muted from outside (the hotkey, a call): drop the local view of the turn too
      else {
        pausedReason = ''; // unmuted from outside (the hotkey)
        callBlocked = false;
      }
      paintMic();
      void mic.setMuted(muted).catch((e) => setStatus(`Microphone unavailable: ${e}`)); // zero live tracks when off
    }
    micLive = st.live;
  } catch {
    /* keep the last known state */
  }
}

/** `tell` false: only show the mode (Rust already applied it at start). */
function setMode(mode: 'mini' | 'main', tell = true): void {
  document.body.classList.toggle('mini', mode === 'mini');
  document.body.classList.toggle('expanded', mode === 'main');
  if (tell) void api.setMode(mode);
}

function currentState(): OrbState {
  // Mirrors Rust's derive_status: speaking, then thinking (real work only), then listening, else off.
  if (player.active) return 'speaking';
  if (turnActive || transcribing) return 'thinking';
  return micLive ? 'listening' : 'muted';
}

function micLabel(s: OrbState): string {
  if (muted && pausedReason) return pausedReason;
  const l = strings();
  if (s === 'muted') return l.pillOff;
  return s === 'listening' ? l.pillListening : s === 'speaking' ? l.pillSpeaking : l.pillThinking;
}

function tick(): void {
  const s = currentState();
  orb.setState(s);
  if (turnActive && lastTurnEventAt && Date.now() - lastTurnEventAt > THINKING_GIVE_UP_MS) {
    turnActive = false; // never stuck on "thinking"
    markInterrupted();
    turnId++; // a late event of the abandoned turn is dropped, not drawn as a stray bubble
    rustTurn = 0;
    void api.cancel().catch(() => undefined);
    toast('One took too long to answer. Please try again.');
  }
  const typing = $('typing');
  typing.hidden = !((turnActive || transcribing) && !player.active);
  const slow = turnActive && Date.now() - lastTurnEventAt > THINKING_SLOW_MS;
  const label = slow ? strings().stillWorking : strings().thinking;
  $('btn-stop').hidden = !(turnActive || transcribing || player.active); // Stop only when there is something to stop
  if (typing.textContent !== label) typing.textContent = label;
  if (s === 'speaking') {
    const { level, fft } = player.read();
    orb.setLevel(level, fft);
  } else {
    orb.setLevel(s === 'listening' ? micLevel : 0);
  }
  // Report only whether One's audio is playing. Whether a turn is running is known to Rust,
  // and speech while merely transcribing is ordinary speech, not a barge-in (review M-1).
  const playing = s === 'speaking';
  if (playing !== lastBarge) {
    lastBarge = playing;
    void api.micSetBarge(playing);
  }
  // The mic indicator is visible in the full and the mini window: red dot while live.
  $('mic-ind').classList.toggle('live', micLive);
  $('mic-ind-text').textContent = micLabel(s);
  requestAnimationFrame(tick);
}

/** "Test voice": a fixed sentence through the normal TTS path; success or the exact error. */
function testVoice(arabic = false): void {
  void player
    .wake() // inside the click: the audio context may start
    .then(() => api.testVoice((b) => player.push(b), arabic))
    .then((note) => toast(`${note ? `${note} ` : ''}You should hear me now. If not, your speakers may be muted: turn on the sound and try again.`, true))
    .catch((e) => toast(`I couldn't play my voice: ${e}`));
}

// ---- chat box ----------------------------------------------------------------

/** Enter sends, Shift+Enter makes a new line. Typed and spoken messages share one thread. */
function initComposer(): void {
  const box = $<HTMLTextAreaElement>('msg');
  const send = () => {
    const text = box.value.trim();
    if (!text) return;
    box.value = '';
    askOne(text);
  };
  $('composer').onsubmit = (e) => {
    e.preventDefault();
    send();
  };
  box.onkeydown = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  };
}

/** The thread kept by Rust (last 50 messages), drawn once at start. */
async function loadHistory(): Promise<void> {
  try {
    const msgs = await api.history();
    for (const m of msgs) addLine(m.role === 'user' ? 'you' : 'one', m.text);
    if (!msgs.length) welcome();
  } catch (e) {
    toast(`Could not load the chat history: ${e}`);
    welcome();
  }
}

/** The first thing a new user sees in the chat. Not saved: it is only a greeting. */
function welcome(): void {
  addLine('one', strings().greeting).classList.add('welcome');
}

// ---- boot ------------------------------------------------------------------

async function fillMics(): Promise<void> {
  const sel = $<HTMLSelectElement>('mic');
  const devs = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === 'audioinput');
  sel.replaceChildren(...devs.map((d, i) => new Option((d.label || `Microphone ${i + 1}`).replace(/\s*\([0-9a-f]{4}:[0-9a-f]{4}\)$/i, ''), d.deviceId)));
  sel.onchange = () => void mic.start(sel.value).catch((e) => setStatus(String(e)));
}

async function fillVoices(): Promise<void> {
  const sel = $<HTMLSelectElement>('voice');
  const selAr = $<HTMLSelectElement>('voice-ar');
  const settings = await api.getSettings(); // UI prefs only; the working directory is file-only
  // Recommended voices carry a plain description; library voices say they need a paid plan.
  const label = (v: Voice, all: Voice[]) => {
    if (v.note) return `★ ${v.note}`;
    const short = all.filter((w) => w.name.split(' - ')[0] === v.name.split(' - ')[0]).length > 1 ? v.name : v.name.split(' - ')[0];
    return v.paid ? `${short} (paid plan)` : short;
  };
  try {
    const voices = await api.listVoices();
    sel.replaceChildren(...voices.map((v) => new Option(label(v, voices), v.voice_id)));
    // Arabic: native Arabic voices first, or the main voice speaking Arabic.
    const arabic = [...voices.filter((v) => v.arabic), ...voices.filter((v) => !v.arabic)];
    selAr.replaceChildren(new Option('Same as my main voice', ''), ...arabic.map((v) => new Option(label(v, voices), v.voice_id)));
    selAr.value = voices.some((v) => v.voice_id === settings.voice_id_ar) ? settings.voice_id_ar : '';
    if (voices.some((v) => v.voice_id === settings.voice_id)) {
      sel.value = settings.voice_id;
    } else if (voices.length) {
      // The built-in default may be a library voice the plan cannot use: take the first premade.
      settings.voice_id = voices[0].voice_id;
      await api.saveSettings(settings);
    }
  } catch (e) {
    sel.replaceChildren(new Option('Default voice', settings.voice_id));
    selAr.replaceChildren(new Option(settings.voice_id_ar ? 'Saved Arabic voice' : 'Same as my main voice', settings.voice_id_ar));
    setStatus("I couldn't load the list of voices. Check your internet; I'll use the usual voice.");
  }
  const save = () => void api.saveSettings({ voice_id: sel.value, voice_id_ar: selAr.value });
  sel.onchange = save;
  selAr.onchange = save;
}

// ---- the default session: the pin, else the coordinator, else the newest live all-for-one-<n> ----

let route: SessionRoute | null = null;
let routeId: string | null | undefined; // undefined = not resolved yet
let userPicked = false; // the user chose a session by hand: the default stops moving the picker

function paintRoute(): void {
  const l = strings();
  $('route-note').textContent = route ? routeText(route, l) : '';
  const pinned = !!route?.pinned && !route.pin_fell_back;
  $('btn-pin').textContent = pinned ? l.unpin : l.pin;
  $('btn-pin').setAttribute('aria-pressed', String(pinned));
}

/** Work out the default again; when it moved (or first time) refill the picker and focus it. */
async function applyRoute(): Promise<void> {
  try {
    route = await api.sessionRoute();
  } catch (e) {
    console.warn('[one] route', e);
    return;
  }
  paintRoute();
  if (userPicked || route.session_id === routeId) return;
  routeId = route.session_id;
  await fillSessions(); // only the suggestion in the picker moves; nothing is focused, so One's own chat keeps its tools
}

/** Pin the session shown in the picker (a live one), or let go of the pin. */
async function togglePin(): Promise<void> {
  const sel = $<HTMLSelectElement>('session');
  const pinned = !!route?.pinned && !route.pin_fell_back;
  const id = pinned ? null : sel.value;
  if (!pinned && (!id || id === 'new')) return toast('Pick a live session first, then pin it.');
  try {
    route = await api.sessionPin(id);
  } catch (e) {
    return toast(String(e));
  }
  userPicked = false; // the pin (or the default after it) is the target again
  routeId = undefined;
  await applyRoute();
}

/** Session picker: people know a session by its terminal TAB TITLE, so that comes first. */
async function fillSessions(): Promise<void> {
  const sel = $<HTMLSelectElement>('session');
  try {
    const tabs = await api.listSessions();
    // new Option(text, value) sets text, never HTML: a title is untrusted.
    // Live sessions first (most recently active on top); finished ones are marked, not hidden.
    const group = (label: string, list: SessionTab[]): HTMLOptGroupElement[] => {
      if (!list.length) return [];
      const g = document.createElement('optgroup');
      g.label = label;
      g.append(...list.map((t) => new Option(`${t.tab_title || 'A session with no name'}${t.live ? '' : ` (${t.status})`}`, t.session_id)));
      return [g];
    };
    sel.replaceChildren(
      new Option('Nothing (just chat with me)', ''),
      // the SUGGESTED target (pin, else coordinator, else newest live All for One): listed, never focused until the user picks it
      ...(route?.session_id ? [new Option(strings().routeDefault(route.tab_title), route.session_id)] : []),
      new Option('Start a new conversation', 'new'),
      ...group('Live now', tabs.filter((t) => t.live)),
      ...group('Finished', tabs.filter((t) => !t.live)),
    );
  } catch (e) {
    sel.replaceChildren(new Option('No other sessions right now', ''));
    console.warn('[one] sessions', e);
  }
  sel.onchange = () => {
    userPicked = true;
    if (sel.value === 'new') {
      void api.newSession().then(() => {
        sel.value = '';
        $('transcript').querySelectorAll('.bubble:not(#typing)').forEach((b) => b.remove()); // Rust cleared the thread too
        setStatus('Started a new conversation.');
      });
    } else if (sel.value) {
      void api.focusSession(sel.value).then(() => void refreshLock()).then(() => setStatus('One is now looking at that session. It can read it, but not change anything.')).catch((e) => toast(String(e)));
    } else {
      void api.focusSession(null).then(() => void refreshLock()).then(() => setStatus('Back to your own chat with One.')).catch((e) => toast(String(e)));
    }
  };
}

async function boot(): Promise<void> {
  await initTeam($('team'), { onAsk: askOne, onAskSession: askSession, onPrefs: syncTabLabels, onOpenTickets: () => setTab('tickets') });
  await initTickets($('tickets'), $('tab-tickets'));
  syncTabLabels();
  await api.micStart(onMicEvent, onRustAudio);
  $('tab-team').onclick = () => setTab('team');
  $('tab-tickets').onclick = () => setTab('tickets');
  $('tab-voice').onclick = () => setTab('voice');
  requestAnimationFrame(tick);
  $('btn-mute').onclick = toggleMute;
  $('btn-override').onclick = () => void setMuted(false, true);
  $('btn-stop').onclick = stop;
  $('btn-test-voice').onclick = () => testVoice();
  $('btn-test-voice-ar').onclick = () => testVoice(true);
  initComposer();
  await refreshLock();
  await loadHistory();
  // Start muted and with the mic closed: nothing is captured until the user unmutes.
  void setMuted(true);
  const st = await api.micState();
  micLive = st.live;
  setInterval(() => void syncMic(), 400);
  // Optional push-to-talk (settings.json): hold F9 in the window; the global key is Ctrl+Alt+Space.
  window.addEventListener('keydown', (e) => e.key === 'F9' && !e.repeat && void api.micSetPtt(true));
  window.addEventListener('keyup', (e) => e.key === 'F9' && void api.micSetPtt(false));
  // Never stay open when the key-up is lost (Alt+Tab, a click elsewhere).
  window.addEventListener('blur', () => void api.micSetPtt(false));
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) void api.micSetPtt(false);
    syncHubView(); // a minimised or hidden window is polled every 30 s, a visible one every 15 s
  });
  setMode(await api.getMode(), false);
  $('btn-expand').onclick = () => setMode('main');
  $('btn-shrink').onclick = () => setMode('mini');
  $('btn-pin').onclick = () => void togglePin();
  if ((await api.keyStatus()) === 'none') {
    setStatus("My voice isn't set up yet, so I'll answer in text. You can still type to me.");
    return;
  }
  try {
    await fillMics(); // the mic itself stays closed until the user unmutes
  } catch (e) {
    setStatus(`Microphone unavailable: ${e}`);
  }
  await fillVoices();
  await fillSessions();
  await applyRoute();
}

void boot();
