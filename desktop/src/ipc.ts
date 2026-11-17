// Thin typed wrappers over Tauri IPC. The webview never sees the ElevenLabs key
// and has no shell/process access: everything goes through these Rust commands.
import { Channel, invoke } from '@tauri-apps/api/core';

export type TurnEvent =
  | { type: 'delta'; text: string }
  | { type: 'cards'; cards: Card[] }
  | { type: 'latency'; latency: Latency }
  | { type: 'done'; interrupted: boolean }
  | { type: 'notice'; message: string }
  | { type: 'error'; message: string };

/** End of speech (or send) to the first audio of the reply, with its parts. Target: under 1.5 s. */
export interface Latency {
  total_ms: number;
  stt_ms: number;
  claude_ms: number;
  voice_ms: number;
  under_target: boolean;
}

export interface ChatMsg {
  role: 'user' | 'assistant';
  text: string;
}

export interface Card {
  tool_use_id: string;
  tool_name: string;
  summary: string;
  /** Bash only: the FULL command, exactly what would run. */
  command: string | null;
  /** The command has shell operators (; & | > < $( `). */
  warning: boolean;
  approvable: boolean;
  /** The request as one plain sentence, e.g. "One wants to search for ... Allow?". Plain text. */
  sentence: string;
  /** Why there is no Approve button. */
  reason: string | null;
}
/** Mic events from Rust: it decides when an utterance starts and ends and runs STT itself. */
export type MicEvent =
  | { type: 'level'; level: number }
  | { type: 'barge_in' }
  | { type: 'transcribing'; on: boolean }
  /** What the user said. Rust has already started turn `turn` with it. */
  | { type: 'transcript'; text: string; turn: number }
  /** What the user is saying right now (live). Shown only; never stored. */
  | { type: 'partial'; text: string }
  /** An event of a turn Rust started; `turn` lets us drop a cancelled turn's late events. */
  | { type: 'turn'; turn: number; event: TurnEvent }
  /** The mic was switched off automatically (a call was detected). Unmute stays manual. */
  | { type: 'paused'; reason: string }
  /** The mute hotkey was refused because a call is on; "Unmute anyway" is offered. */
  | { type: 'blocked'; reason: string }
  | { type: 'error'; message: string };

export type MuteOutcome = 'muted' | 'unmuted' | 'blocked' | 'superseded';

/** Where the mic is. It starts muted at every launch; Rust derives the status. */
export interface MicStatus {
  status: 'muted' | 'listening' | 'thinking' | 'speaking';
  /** Audio is being listened to right now (red dot). */
  live: boolean;
  muted: boolean;
  push_to_talk: boolean;
}

/** A Claude Code session, named by its terminal tab title. All strings are plain text. */
export interface SessionTab {
  tab_title: string;
  session_id: string;
  project: string;
  modified: number;
  /** The session's name (what SendMessage uses), when it is a live named session. */
  name: string | null;
  /** Running now. Finished sessions are listed after the live ones, marked with `status`. */
  live: boolean;
  /** "live now" or "finished 3 h ago". */
  status: string;
}

/** Team screen prefs, stored by Rust in settings.json. The webview keeps no persisted state. */
export interface TeamPrefs {
  lang: 'en' | 'ar';
  theme: 'light' | 'dark';
  scope: 'mine' | 'team';
  view: 'cards' | 'graph';
  helpers: boolean;
  open: Record<string, boolean>;
}

/** The only settings the webview can read or write. The working directory is file-only. */
export interface Settings {
  voice_id: string;
  /** The voice for Arabic replies; empty = the main voice. */
  voice_id_ar: string;
}
export interface Voice {
  voice_id: string;
  name: string;
  /** A plain description for the recommended voices, else empty. */
  note: string;
  /** Needs a paid ElevenLabs plan. */
  paid: boolean;
  /** A native Arabic voice. */
  arabic: boolean;
}

export interface Person {
  name: string;
  name_ar: string | null;
  role: string;
  role_ar: string | null;
  kind: 'person' | 'agent' | 'project' | 'session';
  working_on: string;
  working_on_ar: string | null;
  status: 'active' | 'waiting';
  /** Session cards only: the id that "Ask about" focuses (by id, never by text). */
  session_id?: string;
  gender: 'm' | 'f';
}
export interface Ask {
  id: string;
  who: string;
  whom: string;
  what: string;
  what_ar: string | null;
  kind: 'ticket' | 'approval' | 'access';
  ticket_id: string | null;
  created_at: string;
  age_hours: number;
  amber: boolean; // 2 days or more
  long: boolean; // 3 days or more
  reminded_recently: boolean;
  auto_reminded: boolean;
}
export interface TeamView {
  me: string;
  status: 'ok' | 'missing' | 'error';
  path: string;
  message: string;
  people: Person[];
  asks: Ask[];
  dropped: number;
  hub: HubBrief;
  /** Nothing on you from the team or the hub, and the hub answered. Rust decides. */
  caught_up: boolean;
}

/** The hub's tickets and approvals (read-only). Rust fetches and polls; all text is plain text. */
export type HubState = 'connecting' | 'connected' | 'offline' | 'token_missing' | 'rejected' | 'not_enabled' | 'busy';
export interface HubTicket {
  id: string;
  odoo_ticket_id: string;
  customer_name: string;
  owner_name: string;
  status: string;
  /** The hub's machine stage, e.g. awaiting_owner. */
  stage: string;
  waiting_on: string;
  channel: string;
  summary: string;
  has_pending_approval: boolean;
  updated_ago_secs: number | null;
  /** The live step. Rust takes it from the hub, or works it out for an older hub. */
  lifecycle: HubLifecycle;
  lifecycle_ago_secs: number | null;
  /** The hub sent no step; Rust worked it out, so earlier steps are not proven. */
  lifecycle_derived: boolean;
  /** The instant acknowledgement reached the customer. */
  acked: boolean;
  approved_by: string;
  last_delivery: HubDelivery | null;
}
export type HubLifecycle = 'new' | 'needs_approval' | 'approved' | 'reply_sent' | 'reply_delivered' | 'reply_read' | 'awaiting_customer' | 'closed' | 'held';
export interface HubDelivery {
  kind: 'ack' | 'reply';
  status: 'sent' | 'delivered' | 'read' | 'failed';
  ago_secs: number | null;
}
/** What the Team view needs from the hub: is it reachable, and which tickets wait for a person. */
export interface HubBrief {
  status: HubState;
  needs: { ticket_id: string; odoo_ticket_id: string; customer_name: string; summary: string }[];
  needs_count: number;
  last_ok_ago_secs: number | null;
}
/** Sent after every poll (the full snapshot only comes when something changed). */
export interface HubPulse {
  status: HubState;
  last_ok_ago_secs: number | null;
}
/** Where the default chat goes: the pin, else the coordinator, else the newest live all-for-one-n. */
export interface SessionRoute {
  via: 'pin' | 'coordinator' | 'all_for_one' | null;
  session_id: string | null;
  tab_title: string;
  name: string | null;
  message: string;
  pinned: boolean;
  pin_fell_back: boolean;
}
export interface HubApproval {
  ticket_id: string;
  odoo_ticket_id: string;
  customer_name: string;
  owner_name: string;
  fingerprint: string;
  draft: string;
  /** What the approval is for, e.g. "Reply to client". */
  kind_label: string;
  /** Who may approve, as one text. */
  approvers: string;
  /** Negative once expired; null when the hub gave no expiry. */
  expires_in_secs: number | null;
  /** The exact page "Open to approve" opens (shown in the confirm). Empty = cannot be opened. */
  open_url: string;
}
export interface HubSnapshot {
  status: HubState;
  message: string;
  tickets: HubTicket[];
  approvals: HubApproval[];
  last_ok_ago_secs: number | null;
  /** Tickets that wait for a person (the nav badge). */
  needs_count: number;
}
export interface HubTimelineItem {
  kind: string;
  label: string;
  actor: string;
  ago_secs: number | null;
}
export interface HubTicketDetail {
  ticket: HubTicket;
  pending: HubApproval | null;
  approver: string;
  timeline: HubTimelineItem[];
}

function channels(onEvent: (e: TurnEvent) => void, onAudio: (b: ArrayBuffer) => void) {
  const ev = new Channel<TurnEvent>();
  ev.onmessage = onEvent;
  const audio = new Channel<ArrayBuffer>();
  audio.onmessage = onAudio;
  return { onEvent: ev, onAudio: audio };
}

export const api = {
  keyStatus: () => invoke<string>('key_status'),
  getSettings: () => invoke<Settings>('get_settings'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  listVoices: () => invoke<Voice[]>('list_voices'),
  /** Registers the long-lived channels: mic events, and the audio of turns Rust starts itself. */
  micStart: (onMic: (e: MicEvent) => void, onAudio: (b: ArrayBuffer) => void) => {
    const ch = new Channel<MicEvent>();
    ch.onmessage = onMic;
    const audio = new Channel<ArrayBuffer>();
    audio.onmessage = onAudio;
    return invoke<void>('mic_start', { onMic: ch, onAudio: audio });
  },
  /** The saved chat thread (last 50 messages). */
  history: () => invoke<ChatMsg[]>('history_get'),
  /** True while another session's text may be in play: replies show no live links. */
  lockState: () => invoke<boolean>('lock_state'),
  /** Open an http(s) link in the system browser (Rust refuses anything else). */
  openLink: (url: string) => invoke<void>('open_link', { url }),
  listSessions: () => invoke<SessionTab[]>('list_sessions'),
  /** "Ask about a session": One stays in its own session and gets a read-only extract. `null` clears it. */
  focusSession: (sessionId: string | null) => invoke<void>('focus_session', { sessionId }),
  /** Speak a short fixed sentence through the normal TTS path. Rejects with the exact error. */
  testVoice: (onAudio: (b: ArrayBuffer) => void, arabic = false) => {
    const audio = new Channel<ArrayBuffer>();
    audio.onmessage = onAudio;
    return invoke<string | null>('test_voice', { onAudio: audio, arabic });
  },
  /** Raw little-endian f32 PCM, 16 kHz mono, straight from the AudioWorklet. */
  micFrames: (pcm: ArrayBuffer) => invoke<void>('mic_frames', pcm),
  /** An unmute is refused while a call is on (`blocked`) unless `force`, and loses to a mute that arrives meanwhile (`superseded`). */
  micSetMuted: (muted: boolean, force = false) => invoke<MuteOutcome>('mic_set_muted', { muted, force }),
  newSession: () => invoke<void>('new_session'),
  micState: () => invoke<MicStatus>('mic_state'),
  micSetPtt: (down: boolean) => invoke<void>('mic_set_ptt', { down }),
  getMode: () => invoke<'main' | 'mini'>('get_mode'),
  micSetBarge: (on: boolean) => invoke<void>('mic_set_barge', { on }),
  getTeamPrefs: () => invoke<TeamPrefs>('get_team_prefs'),
  setTeamPrefs: (prefs: TeamPrefs) => invoke<void>('set_team_prefs', { prefs }),
  runTurn: (text: string, e: (e: TurnEvent) => void, a: (b: ArrayBuffer) => void) =>
    invoke<void>('run_turn', { text, ...channels(e, a) }),
  approve: (toolUseId: string, e: (e: TurnEvent) => void, a: (b: ArrayBuffer) => void) =>
    invoke<void>('approve_tool', { toolUseId, ...channels(e, a) }),
  deny: (toolUseId: string) => invoke<void>('deny_tool', { toolUseId }),
  cancel: () => invoke<void>('cancel_turn'),
  teamLoad: () => invoke<TeamView>('team_load'),
  teamSetDone: (id: string, done: boolean) => invoke<void>('team_set_done', { id, done }),
  teamRemind: (id: string) => invoke<void>('team_remind', { id }),
  setMode: (mode: 'mini' | 'main') => invoke<void>('set_mode', { mode }),
  hubSnapshot: () => invoke<HubSnapshot>('hub_snapshot'),
  hubRefresh: () => invoke<HubSnapshot>('hub_refresh'),
  /** Tell Rust whether the Tickets screen or the Team view is on screen (it polls every 15 s then, else 30 s). */
  hubSetInView: (on: boolean) => invoke<void>('hub_set_in_view', { on }),
  /** The window gained focus: poll soon. */
  hubNudge: () => invoke<void>('hub_nudge'),
  sessionRoute: () => invoke<SessionRoute>('session_route'),
  /** Pin a live session as the default (null clears the pin). */
  sessionPin: (sessionId: string | null) => invoke<SessionRoute>('session_pin', { sessionId }),
  hubTicket: (id: string) => invoke<HubTicketDetail>('hub_ticket', { id }),
  /** Rust holds the address; the webview names the approval only. */
  hubOpenApproval: (ticketId: string, fingerprint: string) => invoke<void>('hub_open_approval', { ticketId, fingerprint }),
};
