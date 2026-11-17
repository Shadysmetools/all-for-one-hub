# One (desktop)

The All for One voice app: a Tauri 2 shell (Rust) with a vanilla TypeScript UI.
You talk, One answers aloud. The brain is Claude Code (`claude -p`, streamed);
the voice is ElevenLabs (Scribe STT, Flash TTS). Windows-first, mac-portable.

## Run it

```
cd desktop
npm i
npm run tauri dev
```

Windows build notes: MSVC toolchain (pinned in `src-tauri/rust-toolchain.toml`).
On a tight machine set `CARGO_TARGET_DIR` to a roomy drive and `CARGO_BUILD_JOBS=2`.

Needs the `claude` CLI on PATH (or `ONE_CLAUDE_BIN=<path to claude.exe>`). The default
"One" session runs in your home folder (set `cwd` in `settings.json` in the app data dir to your
project; changing it starts a fresh session). The session id is saved there too.

## Where the ElevenLabs key comes from

1. `%USERPROFILE%\.claude\secrets\elevenlabs.json` (field `api_key`), else
2. the OS keychain: service `all-for-one-one`, user `elevenlabs`.
   Store it with `one-desktop --store-key` (reads one line from stdin).

The key lives only in Rust. It is wrapped in a type whose `Debug`/`Display` redact,
is never logged, never in an error string, never sent to the webview, never in the
bundle. All ElevenLabs calls are made by Rust to `api.elevenlabs.io` only.

## Safety rules (do not weaken)

- `--permission-mode manual` is ALWAYS passed (the user's global default is
  `bypassPermissions`). A unit test asserts the argv never contains
  `--dangerously-skip-permissions`, `bypassPermissions`, `acceptEdits`, `auto`, `dontAsk`.
- Denied tool calls become approval cards (tool + short input summary). Approve needs a
  **click**; a spoken "yes" never approves. Approving re-runs one turn with
  `--resume <id> --allowedTools "<Tool(exact arg)>"`, rebuilt in Rust from the stored
  denial (the webview only sends an id). Single use, never persisted, never widened.
  MVP: only `Bash(<exact command>)` is approvable (verified live: a denied `ping` ran
  after approval). Write/Edit and unknown tools show a card with Deny only, because
  Windows drive-letter path rules did not match any syntax we probed. Commands with
  wildcards, parentheses or newlines also cannot be approved.
- `claude` is spawned only from Rust, directly (never via cmd.exe). The prompt goes over
  stdin, not argv. The webview has no shell/process/fs/http plugin permissions; IPC is
  deny-by-default (`build.rs` command list + `capabilities/default.json`) and the CSP is
  `default-src 'self'`.
- Audio is held in memory only and never written to disk. No camera.
- Mute: button, or global hotkey **Ctrl+Alt+M**. Muting stops every track and closes the audio context, so the OS mic indicator goes off; unmuting asks for the mic again.
  The orb shows a muted state and the badge says Listening / Muted.
- Barge-in: while One speaks or thinks, the VAD threshold rises; if you talk, playback
  stops, the `claude` child and the TTS request are killed, and the reply is marked
  interrupted.

## Team screen (home tab)

The expanded window opens on **Team**; **One** is the voice tab (the mic keeps listening on
both; speaking switches to the One tab). Ported from the approved mockup: EN/AR with real RTL,
light/dark, big text, "My asks / Team", "People and agents / Who needs whom", age on every ask.
All board text is rendered as text nodes, never HTML. Language, theme and view are remembered
in the webview's localStorage.

**"Ask One about this"** switches to the One tab and runs a normal turn through the existing
`run_turn` command with `Tell me about: <who> needs <whom> for <what>` (the item's safe text).

### Data file (read-only, Phase 1)

`team-asks.json` in the app data dir (`%APPDATA%\dev.allforone.one\`). Copy
`desktop/team-asks.example.json` there to start. Missing file: the screen explains where it goes.
It is re-read every minute and on every tab switch. Shape:

```
{ "people": [ { "name", "name_ar"?, "kind": "person|agent", "role"?, "working_on"?, "status": "active|waiting", "gender"?: "m|f" } ],
  "asks":   [ { "id", "who" (needs), "whom", "what" (<=120), "what_ar"?, "kind": "ticket|approval|access",
                "created_at" (ISO), "ticket_id"? } ] }
```

Validated on load (Rust, `team.rs`): any text that looks like a phone number, email, IP or token
drops the ask (people: the text is blanked); `what` is cut to
120 chars; bad kind/date/duplicate ids are skipped (the screen says how many). Reading goes
through a `TeamSource` trait; only the file adapter exists. Stubs for later: hub API/MCP, team-chat
posts, `afo` CLI. A `STATUS.md` in the working directory (or settings.json `status_file`) adds project cards.

### Done, Remind, ageing

- **Done** only marks the ask on the board (`team-state.json`, app data dir). Undo for 5 s on
  screen; nothing goes to the hub.
- **Remind** posts to your team chat through a script you supply (settings.json `remind_script`;
  none by default, so Remind says it is not configured). Rust spawns
  `<remind_python> -I -X utf8 <remind_script> one-desktop "<msg>"` directly, no
  shell. The webview sends only the ask id; Rust builds
  `@<Name> reminder: <who> needs <whom> for <what> (open <N> days).`, strips control chars and
  newlines, caps at 300. Limit: 1 manual remind per ask per hour (persisted). Never Telegram or WhatsApp.
- **Ageing**: an ask turns amber at 2 days.
- **Automatic reminders are OFF by default.** Turn them on by adding `"auto_remind": true` to
  `settings.json` (file-only; restart the app). When on: at 3 days ONE automatic reminder
  per ask, recorded in `team-state.json` before it is sent, so it is never repeated, even across
  restarts. Only asks created AFTER auto-remind was first enabled are eligible (that time is
  stored once), so old or sample asks never trigger a burst. At most 3 are posted per hourly run
  (checked on start, then hourly). An automatic post is NEVER retried, whatever happens.
  Done asks are never reminded.
- **What counts as "not sent"**: the script is expected to queue the message before it posts it, so
  only a `REFUSED:` answer on stderr means nothing was queued (a manual Remind can then be clicked again).
  Any other failure (non-zero exit, timeout) counts as "maybe sent" and is not retried.
- Timestamps in `created_at` without an offset are read as **UTC**. Use `Z` or `+02:00`.
- Phase 1 assumes one person runs One: each app keeps its own `team-state.json`, so two running
  copies would each send their own reminder (note for Phase 2).
- **Visibility**: everyone sees every card in full. Agent cards sit behind "Show helpers" (off).
- `settings.json` extra keys (hand-edited, read at start, not changeable from the webview):
  `me` (default `Alex`, the first person in the sample file: which name is "you"), `remind_python` (default `python`),
  `remind_script` (default empty: Remind is off), `auto_remind` (default `false`).
  A partial file is fine (every key has a default); a file that cannot be parsed is never
  rewritten by the app, at start-up OR while it runs: a save is refused with an error and the
  file is left exactly as you wrote it. Saves go through a temp file and a rename (atomic).
- **Automatic reminder limits**: at most 10 automatic posts per calendar day (persisted), on top of
  3 per hourly run. Only `@mentions` of people listed in `team-asks.json` are kept in a reminder;
  any other `@name` (`@everyone`, `@here`, an invented name) loses its `@`.

## Window and microphone (follow-ups)

- **Window**: One opens as a normal resizable window (minimum 760x560, title bar, taskbar entry)
  showing the Team / One tabs and the session picker. The first launch is 1200x800, at most 90% of
  the screen; after that it opens at the size you left it (`main_size` in `settings.json`). The
  compact orb ("mini", 220x220, always on top) is optional: the corner buttons switch between them
  and the choice is remembered (`window_mode`). The mini window is the only one that stays on top.
- **The mic is OFF at every launch** and is never saved as "on". Nothing is captured or processed
  until you press Unmute (or Ctrl+Alt+M). A pill at the top (full) or bottom (mini) always says
  "Mic off", "Paused: call detected", or, with a red dot, "Listening" / "Thinking" / "Speaking".
  The status is derived in Rust (`mic::derive_status`), so a fresh app shows Mic off, not Thinking.
- After Unmute the default is hands-free conversation. Optional `settings.json` keys (hand-edited,
  restart to apply): `"push_to_talk": true` (listen only while Ctrl+Alt+Space, or F9 in the window,
  is held; released when the window loses focus), `"wake_word": true` (only speech that starts with
  "One" or "ون", optionally after hey / hi / ok / يا, becomes a turn; the word is removed; "وان" is not
  accepted, it is a common Arabic word), `"auto_pause": false` (turns call detection off; on by default).
- **Call detection**: signals are call windows (Zoom's CptHost; a Teams, Zoom or WhatsApp window whose
  title says meeting or call; a browser tab "Meet - ...", "Zoom Meeting" or a Teams meeting) and any other
  app holding the microphone (Windows consent store, desktop AND packaged Store apps such as new Teams
  and WhatsApp; read in-process, no child process). One itself is ignored by its own process id and
  path. At UNMUTE, if a call is on, the mic stays off and the pill says "Call detected: mic stays
  off" with an "Unmute anyway" button. While unmuted it checks every 3 s and mutes ("Paused: call
  detected") when a NEW signal appears, so an app that always holds the mic cannot hide a later call
  (it is logged once). Nothing is polled while muted. There is no auto-resume.
- The persona tells Claude the team is not technical: plain, short English for "what's new".

## Voice and sessions (follow-ups)

- Rust starts the turn itself when speech is transcribed; the webview only shows events. Speech
  heard while an earlier utterance is still being transcribed is NOT a barge-in: the texts are
  kept in order and joined into one request. Barge-in counts only while a turn runs or audio plays.
  Stop and mute drop any pending transcript. At most 20 speech-to-text calls per minute.
- **Ask about a session (One tab)**: only sessions of the working directory in `settings.json` are
  listed, by their terminal TAB TITLE (last `customTitle`, else last `aiTitle`, read from the tail of
  `~/.claude/projects/<project>/<id>.jsonl`), then the short id. One ALWAYS talks in its own
  session: it never resumes or forks another one (a busy session carries a huge context and may be
  live in another terminal). Picking a session adds a read-only block to One's system prompt: the tab
  title and the last ~20 user/assistant TEXT messages (no tool calls or results), capped near 8 KB,
  read from the last 1 MB of the transcript, redacted, inside delimiters marked as untrusted data.
  "One (own session)" (the default) clears it; "Start a new conversation" drops One's own saved
  session. An old `session.json` that holds a picked or forked id is ignored.
- **Never stuck on Thinking**: a watchdog kills the claude tree when no reply text arrives within
  30 s, or the turn passes 120 s, and says "That's taking too long...". Every end path sends a final
  Done. Sound tags such as `[door closing]` are dropped; so are turns of fewer than 2 words (bar
  yes/no/stop and a few more). Errors show as a red toast for ~8 s. "Test voice" speaks a fixed
  sentence and shows success or the exact error; the audio context starts on the first click or key.

## Security hardening (round 1)

- Approval card shows the full Bash command (monospace, pre-wrap, scrolls). Approve only for printable ASCII, 500 chars or fewer, no control or hidden/bidi characters; otherwise Deny-only with the reason. A red warning shows for `; & | > < $( \``.
- The webview can save only UI prefs (voice). `cwd`, `me`, `remind_python`, `remind_script` are file-only in `settings.json`; forged fields are rejected.
- claude runs with `--setting-sources project,local` (no user hooks/plugins).
- Replies are redacted in Rust (`redact.rs`) before display and TTS: secrets become "[hidden]".
- Stop/barge-in kills the whole claude process tree (`taskkill /T /F`; process group on unix).
- ElevenLabs client never follows redirects; errors show status plus a fixed message only.
- `--store-key` reads without echo on a terminal; when piping use `< file`, not `echo key |`.
- Not done (Low): L5 (single-execution approvals, pending cleared on cwd change: cwd is now file-only), L6 (CSP `unsafe-inline` styles).

## Tests

```
cd src-tauri
cargo test                                  # unit tests (no key, no network)
cargo test keyring_roundtrip -- --ignored   # OS keychain write/read/delete
cargo test live_tts_stt_smoke -- --ignored --nocapture   # live ElevenLabs, needs the key
```

## Orb

`src/orb/orb.ts` is an original WebGL fragment-shader orb in the brand colours
(navy, #1F6FD6, teal glow), with a Canvas2D fallback. States: idle (breathing),
listening (mic level), thinking (swirl), speaking (driven live by the output
AnalyserNode level + FFT), muted (dim, no motion). Keep the
`Orb`/`OrbState`/`createOrb` interface if you change it.

## Known follow-ups

Attach-to-session, `afo ask`, an MCP server for Claude Desktop, wake word "Hey One",
Team follow-ups (live hub/MCP, team-chat and afo adapters; Done and Remind through the hub with approvals; FeatureTour anchors `topbar scope view cards ask lang theme` are not added yet; loading/hub-unreachable polish; the per-user "me" from sign-in instead of settings.json; Remind on a hub/script failure shows the raw script error), Arabic STT bake-off, realtime
(streaming) STT, TTS prefetch of the next sentence, speaking over code blocks,
macOS transparent-window config, path-scoped (Write/Edit) approvals, single-execution approvals (L5),
the VAD noise floor adapting upward (a constant fan can keep sending noise clips to STT), a
frontend test setup (mic start/stop, toast lifetime), hashed styles instead of CSP `unsafe-inline`,
queueing approvals (today a second Approve is refused while a turn runs), and the chunker splitting "e.g. " mid-abbreviation.

## Reliability notes (round 2)

- A turn whose `claude` run fails (error result, no result, or a non-zero exit with no output)
  never saves its session id. If it was a resumed turn that failed before any text, the saved
  session is cleared and the turn retried once without `--resume`; otherwise the status and
  transcript show "One could not reach Claude: <reason>". stderr goes to the log (truncated, redacted).
  `session.json` stores the working directory too; a different one drops the saved session.
- ElevenLabs timeouts: connect 5 s, speech-to-text 30 s total, text-to-speech 15 s idle.
- The claude process tree dies with the app: on Windows every claude child is in a Job Object
  with kill-on-close (also on a crash); on exit all tracked trees are killed (unix: process group).

## Tickets screen (hub, read-only)

The **Tickets** tab shows the live All for One tickets and the approvals waiting for a person.
Rust (`hub.rs`) polls the hub every 15 s while the Tickets screen or the Team view is on screen (30 s when the
window is hidden or minimised, and at once when it gains focus) at `http://127.0.0.1:8891/api/v1` (loopback IP only, no proxy,
no redirects) with the bearer token from `AFO_HUB_TOKEN_FILE`, else `%USERPROFILE%\.claude\secrets\hub-api-token.txt`.
The hub HTTP API this screen reads is not part of this repository yet (see the Roadmap in the main README). The last good result is kept, and
the screen says "Offline: hub not reachable" when the hub cannot be reached. Parsing is tolerant
(unknown fields ignored, ids may be numbers). Nothing is approved from the desktop: **Open to approve**
asks first, then opens `https://<AFO_ADMIN_HOST>/tickets/<id>` in the browser (`AFO_ADMIN_HOST` is a bare
hostname read once at start; default `admin.example.com`). Rust builds that
address from the numeric ticket id (the hub's own `approve_url` is ignored) and `link::open_vetted`
refuses anything else. The webview only names the approval (ticket id + fingerprint).
Known limit: the token goes over plain http to whatever listens on 127.0.0.1:8891, so only a
process on this machine could receive it.

### Live lifecycle and Team-view parity

- Each ticket shows a step strip: New, Needs approval, Approved, Reply sent, Delivered, Read, Awaiting customer,
  Closed (and Held), with "Ack sent", "Approved by X", the last delivery status and how long it has been at that
  step. The hub sends `lifecycle`, `lifecycle_at`, `acked`, `approved_by`, `last_delivery`; an older hub that
  does not is handled by deriving the step in Rust from `has_pending_approval`, `stage`, `status`, `waiting_on`
  (`hub_lifecycle.rs`). Tickets closed in the last 48 h stay visible (the hub has `status=open|all`; Rust asks for
  `all` and keeps the closed ones).
- `hub-update` is sent only when something changed; `hub-pulse` (tiny) after every poll drives "Updated Ns ago" and
  the Live / Hub offline / No token pill. Rust owns the timers; the webview only says whether the hub screens are
  in view (`hub_set_in_view`) and whether the window got focus (`hub_nudge`).
- The Team view reads the hub too: "Hub tickets need you: N" (ids and one-line summaries, click opens Tickets), a
  count badge on the Tickets tab, and "You are all caught up" only when the team asks AND the hub approvals are
  empty AND the hub answered (`team_caught_up`).

### Which session the default chat uses (`route.rs`)

The **suggested** session in the picker ("All for One (current): ...") is always a live one: a **pinned** session
while it is live, else the **coordinator** (settings.json key `coordinator_session`, default `coordinator`),
else the newest live `all-for-one-<n>` (highest n; `all-for-one-desktop*` never). The screen says which one it used.
Nothing is focused at start: One keeps its own conversation (and its tools) until the user picks a session. A per-session "Ask about X" stays on X. Only sessions of One's own project folder can be read, so a coordinator that
lives in another project is not reachable from One and the fallback is used (the screen says so).
