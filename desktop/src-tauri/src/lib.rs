mod autoboard;
mod brain;
mod call;
mod chunker;
mod claude;
mod eleven;
mod history;
mod hub;
mod hub_fail;
mod hub_lifecycle;
mod latency;
mod link;
mod live;
mod mic;
mod proc;
mod redact;
mod remind;
mod route;
mod secret;
mod stt_live;
mod state;
mod tabs;
mod team;
mod transcript;
mod tts_live;
mod turn;
mod vad;
mod wake;
mod winvoice;
mod ws;

use remind::{Team, TeamView};
use serde::Serialize;
use std::sync::Arc;
use state::{Shared, TeamPrefs, UiPrefs};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Emitter, LogicalSize, Manager, State};
use turn::{EventSink, TurnEvent, TurnRequest};

#[derive(Serialize)]
struct VoiceOut {
    voice_id: String,
    name: String,
    /// A plain description for the recommended voices ("Eric: warm, calm ..."), else empty.
    note: String,
    /// Library voices need a paid ElevenLabs plan (they fail on the free plan).
    paid: bool,
    /// The voice speaks Arabic natively (an Arabic-language voice).
    arabic: bool,
}

#[tauri::command]
fn key_status() -> &'static str {
    secret::key_source()
}

#[tauri::command]
fn get_settings(shared: State<'_, Shared>) -> UiPrefs {
    shared.ui_prefs()
}

#[tauri::command]
fn save_settings(shared: State<'_, Shared>, settings: UiPrefs) -> Result<(), String> {
    shared.save_prefs(settings)
}

#[tauri::command]
async fn list_voices() -> Result<Vec<VoiceOut>, String> {
    let key = secret::load_key()?;
    let voices = eleven::rank(eleven::list_voices(&eleven::client()?, &key).await?);
    Ok(voices
        .into_iter()
        .map(|v| VoiceOut {
            note: eleven::RECOMMENDED.iter().find(|(id, _)| *id == v.voice_id).map(|(_, n)| n.to_string()).unwrap_or_default(),
            paid: v.category != "premade",
            arabic: v.language == "ar",
            voice_id: v.voice_id,
            name: v.name,
        })
        .collect())
}

#[tauri::command]
fn get_team_prefs(shared: State<'_, Shared>) -> TeamPrefs {
    shared.team_prefs()
}

#[tauri::command]
fn set_team_prefs(shared: State<'_, Shared>, prefs: TeamPrefs) -> Result<(), String> {
    shared.save_team_prefs(prefs)
}

#[tauri::command]
async fn run_turn(
    shared: State<'_, Shared>,
    text: String,
    on_event: Channel<TurnEvent>,
    on_audio: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    shared.clear_pending();
    shared.chat_push("user", &text);
    let sink = EventSink::Chan(on_event);
    let res = turn::drive(&shared, TurnRequest { prompt: text, allowed_tool: None, speech_end: None }, sink.clone(), on_audio).await;
    report(res, &sink)
}

/// Approve one pending denial: a single resumed turn scoped to that exact tool call.
/// The scope is rebuilt from Rust-held state, never from webview input.
const DENY_ONLY: &str = turn::DENY_ONLY_REASON;

#[tauri::command]
async fn approve_tool(
    shared: State<'_, Shared>,
    tool_use_id: String,
    on_event: Channel<TurnEvent>,
    on_audio: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    if shared.tainted() {
        return Err(DENY_ONLY.into());
    }
    let d = shared.take_pending(&tool_use_id).ok_or("no such pending approval")?;
    let scope = claude::scope_for(&d.tool_name, &d.tool_input)?;
    let req = TurnRequest { prompt: format!("Approved: go ahead with {}.", d.tool_name), allowed_tool: Some(scope), speech_end: None };
    let sink = EventSink::Chan(on_event);
    let res = turn::drive(&shared, req, sink.clone(), on_audio).await;
    report(res, &sink)
}

/// The saved chat thread, for the webview to draw on start.
#[tauri::command]
fn history_get(shared: State<'_, Shared>) -> Vec<history::ChatMsg> {
    shared.chat_history()
}

/// Open a link from a reply in the system browser (http and https only).
#[tauri::command]
fn open_link(url: String) -> Result<(), String> {
    link::open(&url)
}

#[tauri::command]
fn deny_tool(shared: State<'_, Shared>, tool_use_id: String) {
    let _ = shared.take_pending(&tool_use_id);
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Off the main thread: the board scans session transcripts on disk.
#[tauri::command]
async fn team_load(team: State<'_, Arc<Team>>, hub: State<'_, Arc<hub::Hub>>) -> Result<TeamView, String> {
    let t = team.inner().clone();
    let mut v = tokio::task::spawn_blocking(move || t.view(now_secs())).await.map_err(|_| "could not load the board".to_string())?;
    // the hub's tickets that wait for a person count too: "all caught up" needs both lists empty and the hub reachable
    let me = v.me.to_lowercase();
    let on_me = v.asks.iter().filter(|a| a.ask.whom.to_lowercase() == me).count();
    v.hub = hub.brief();
    v.caught_up = hub_lifecycle::team_caught_up(on_me, &v.hub);
    Ok(v)
}

/// The hub's tickets and approvals as last fetched (instant; Rust polls every 15-30 s).
#[tauri::command]
fn hub_snapshot(hub: State<'_, Arc<hub::Hub>>) -> hub::Snapshot {
    hub.view()
}

/// The Refresh button: poll now. Also tells every listener, so the badge stays right.
#[tauri::command]
async fn hub_refresh(app: tauri::AppHandle, hub: State<'_, Arc<hub::Hub>>) -> Result<hub::Snapshot, String> {
    hub.refresh_manual().await;
    Ok(hub.publish(&app)) // tells every listener only if something changed
}

/// The webview says whether the Tickets screen or the Team view is on screen (poll every 15 s, else 30 s).
#[tauri::command]
fn hub_set_in_view(hub: State<'_, Arc<hub::Hub>>, on: bool) {
    hub.set_in_view(on);
}

/// The window gained focus: poll soon (Rust applies the minimum gap).
#[tauri::command]
fn hub_nudge(hub: State<'_, Arc<hub::Hub>>) {
    hub.nudge();
}

/// One ticket with its timeline, fetched live.
#[tauri::command]
async fn hub_ticket(hub: State<'_, Arc<hub::Hub>>, id: String) -> Result<hub::TicketDetail, String> {
    hub.ticket(&id).await
}

/// "Open to approve": the webview names the approval (ticket id + fingerprint); Rust holds the
/// address and opens it. Nothing is approved from here.
#[tauri::command]
fn hub_open_approval(hub: State<'_, Arc<hub::Hub>>, ticket_id: String, fingerprint: String) -> Result<(), String> {
    hub.open_approval(&ticket_id, &fingerprint)
}

/// Whether replies must show links as plain text (another session's text may be in play).
#[tauri::command]
fn lock_state(shared: State<'_, Shared>) -> bool {
    shared.locked()
}

/// Done is local to this app: it never reaches the hub in Phase 1.
#[tauri::command]
fn team_set_done(team: State<'_, Arc<Team>>, id: String, done: bool) -> Result<(), String> {
    team.set_done(&id, done, now_secs())
}

/// The webview sends only the item id; the reminder text is built in Rust from the stored item.
#[tauri::command]
async fn team_remind(team: State<'_, Arc<Team>>, id: String) -> Result<(), String> {
    let t = team.inner().clone();
    tokio::task::spawn_blocking(move || t.remind(&id, now_secs())).await.map_err(|_| "reminder failed")?
}

/// Tell the Team screen to refresh once a minute (the webview keeps no timers of its own).
fn spawn_team_tick(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let _ = app.emit("team-tick", ());
        }
    });
}

/// Check on start, then hourly: one automatic reminder per item at 3 days.
fn spawn_auto_remind(team: Arc<Team>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let t = team.clone();
            let _ = tokio::task::spawn_blocking(move || t.auto_tick(now_secs())).await;
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}

/// Stop: cancels the turn AND drops any pending speech (still transcribing, held, or still
/// inside the VAD), so nothing fires after it. The webview calls this on every Stop.
#[tauri::command]
fn cancel_turn(shared: State<'_, Shared>, mic: State<'_, Arc<mic::MicState>>) {
    mic.stop_pending();
    shared.cancel_turn();
}

/// Sessions of One's own working directory, named by their terminal TAB TITLE first.
/// Other projects' sessions cannot be resumed from here, so they are not offered.
#[tauri::command]
async fn list_sessions(shared: State<'_, Shared>) -> Result<Vec<tabs::SessionTab>, String> {
    let cwd = shared.settings().cwd;
    tokio::task::spawn_blocking(move || tabs::list_sessions(&cwd)).await.map_err(|_| "could not list sessions".to_string())
}

/// "Ask about a session": One's next turns still run in One's OWN session, with a read-only
/// extract of this session's tail added. The id must be one we just listed. `None` (the
/// "One (own session)" entry) clears the focus.
#[tauri::command]
async fn focus_session(shared: State<'_, Shared>, session_id: Option<String>) -> Result<(), String> {
    let Some(session_id) = session_id else { return shared.set_focus(None) };
    let cwd = shared.settings().cwd;
    let known = tokio::task::spawn_blocking(move || tabs::list_sessions(&cwd)).await.map_err(|_| "could not list sessions".to_string())?;
    if !known.iter().any(|t| t.session_id == session_id) {
        return Err("unknown session".into());
    }
    shared.set_focus(Some(&session_id))
}

/// The session the default chat and the overall "Ask about" use: the pin, else the coordinator,
/// else the newest live `all-for-one-<n>`. Always live, or none (with a plain message).
#[tauri::command]
async fn session_route(shared: State<'_, Shared>) -> Result<route::Route, String> {
    let (cwd, coord) = (shared.settings().cwd, shared.team_config().coordinator_session);
    tokio::task::spawn_blocking(move || route::current(&cwd, &coord)).await.map_err(|_| "could not list sessions".to_string())
}

/// Pin a live session as the default (`None` clears it). It holds until the session stops being live.
#[tauri::command]
async fn session_pin(shared: State<'_, Shared>, session_id: Option<String>) -> Result<route::Route, String> {
    let (cwd, coord) = (shared.settings().cwd, shared.team_config().coordinator_session);
    tokio::task::spawn_blocking(move || {
        route::set_pin(&tabs::list_sessions(&cwd), session_id.as_deref())?;
        Ok(route::current(&cwd, &coord))
    })
    .await
    .map_err(|_| "could not list sessions".to_string())?
}

/// "Test voice": speak a short fixed sentence through the normal TTS path, so a silent voice
/// shows its exact error instead of nothing.
#[tauri::command]
/// Ok(Some(note)) = it played, with something to tell (e.g. the standard voice stood in).
async fn test_voice(shared: State<'_, Shared>, on_audio: Channel<InvokeResponseBody>, arabic: Option<bool>) -> Result<Option<String>, String> {
    static GATE: std::sync::Mutex<state::VoiceGate> = std::sync::Mutex::new(state::VoiceGate::new());
    let lock = || GATE.lock().unwrap_or_else(|p| p.into_inner());
    lock().try_start(std::time::Instant::now(), std::time::Duration::from_secs(5))?;
    let res = speak_test(&shared, on_audio, arabic.unwrap_or(false)).await;
    lock().finish(std::time::Instant::now());
    res
}

async fn speak_test(shared: &Shared, on_audio: Channel<InvokeResponseBody>, arabic: bool) -> Result<Option<String>, String> {
    // The same live path as a real reply (streaming socket, then the HTTP fallback), so the
    // test hears exactly what One will sound like, and a failure shows its real reason.
    let key = secret::load_key()?;
    let client = eleven::client()?;
    let s = shared.settings();
    let fallback = if tts_live::eleven_down() {
        String::new()
    } else {
        eleven::resolve_voice(&client, &key, &s.voice_id).await.unwrap_or_else(|e| {
            eprintln!("voice test: premium voice unavailable ({e}), using the built-in one");
            tts_live::note_lookup_failure(&e);
            String::new()
        })
    };
    let line = if arabic { "أهلاً، أنا وان. سامعني كويس؟" } else { "Hi, it's One. Can you hear me okay?" };
    let (voice, lang) = tts_live::pick_voice(line, &s.voice_id, &s.voice_id_ar);
    let voice = if voice.is_empty() { fallback.clone() } else { voice };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(line.to_string());
    drop(tx);
    let notice = std::sync::Mutex::new(None::<String>);
    let plan = tts_live::Plan { key, voice, fallback_voice: fallback, lang };
    // A whole test may take at most 30 s, whatever the server does.
    let cancel = tokio_util::sync::CancellationToken::new();
    let limit = cancel.clone();
    let timer = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        limit.cancel();
    });
    let spoken = tts_live::speak(plan, rx, cancel, |b| drop(on_audio.send(InvokeResponseBody::Raw(b))), |m| {
        notice.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(m);
    })
    .await;
    timer.abort();
    match spoken {
        tts_live::Spoken::Failed(e) => Err(e),
        tts_live::Spoken::Cancelled => Err("the voice test took too long".into()),
        tts_live::Spoken::TextOnly => Err(tts_live::no_voice_notice(arabic)),
        // e.g. "That voice needs a paid ElevenLabs plan, so I'm using the standard voice."
        _ => Ok(notice.into_inner().unwrap_or_else(|p| p.into_inner())),
    }
}

/// "Start a new conversation": the only way the saved session is dropped.
#[tauri::command]
fn new_session(shared: State<'_, Shared>) {
    shared.clear_session();
}

/// Logical size of the window now (None if the window cannot say).
fn logical_size(w: &tauri::WebviewWindow) -> Option<[f64; 2]> {
    let s = w.inner_size().ok()?;
    let f = w.scale_factor().ok()?;
    Some([s.width as f64 / f, s.height as f64 / f])
}

/// Remember the full window's size (only while it is in main mode and not minimised).
fn remember_main_size(w: &tauri::WebviewWindow, shared: &Shared) {
    if shared.settings().window_mode != "main" || w.is_minimized().unwrap_or(false) {
        return;
    }
    if let Some(size) = logical_size(w) {
        if let Err(e) = shared.save_main_size(size) {
            eprintln!("could not remember the window size: {e}");
        }
    }
}

/// Size and chrome for each mode. Main is a normal resizable window (taskbar entry, title bar)
/// at the size the user left it (first launch: 1200x800, at most 90% of the screen); mini is
/// the compact always-on-top orb.
fn apply_mode(w: &tauri::WebviewWindow, mode: &str, saved: Option<[f64; 2]>) -> Result<(), String> {
    let mini = match mode {
        "mini" => true,
        "main" => false,
        _ => return Err("unknown mode".into()),
    };
    let e = |e: tauri::Error| e.to_string();
    // Mini is a small always-on-top window, never a minimised one: make sure the window is
    // restored before it changes shape, and shown and restored again after.
    w.unminimize().map_err(e)?;
    w.set_resizable(true).map_err(e)?;
    // Chrome first, then the size: changing the frame after a 220 px resize can leave the window iconic.
    w.set_decorations(!mini).map_err(e)?;
    if mini {
        w.set_min_size(None::<LogicalSize<f64>>).map_err(e)?;
        w.set_size(LogicalSize::new(220.0, 220.0)).map_err(e)?;
    } else {
        let screen = w.current_monitor().ok().flatten().map(|m| [m.size().width as f64 / m.scale_factor(), m.size().height as f64 / m.scale_factor()]);
        let [width, height] = state::clamp_window(saved.unwrap_or(state::FIRST_LAUNCH_SIZE), screen);
        w.set_min_size(Some(LogicalSize::new(width.min(state::MIN_WINDOW[0]), height.min(state::MIN_WINDOW[1])))).map_err(e)?;
        w.set_size(LogicalSize::new(width, height)).map_err(e)?;
    }
    w.set_resizable(!mini).map_err(e)?;
    w.set_always_on_top(mini).map_err(e)?;
    w.unminimize().map_err(e)?;
    w.show().map_err(e)?;
    w.set_focus().map_err(e)
}

#[tauri::command]
fn get_mode(shared: State<'_, Shared>) -> String {
    shared.settings().window_mode
}

#[tauri::command]
fn set_mode(app: tauri::AppHandle, shared: State<'_, Shared>, mode: String) -> Result<(), String> {
    let w = app.get_webview_window("main").ok_or("no window")?;
    remember_main_size(&w, &shared); // before the window changes shape
    apply_mode(&w, &mode, shared.settings().main_size)?;
    if let Err(e) = shared.save_window_mode(&mode) {
        eprintln!("could not remember the window mode: {e}");
    }
    Ok(())
}

fn report(res: Result<(), String>, ev: &EventSink) -> Result<(), String> {
    if let Err(message) = &res {
        ev.send(TurnEvent::Error { message: message.clone() });
        ev.send(TurnEvent::Done { interrupted: false });
    }
    Ok(())
}

fn register_hotkeys(app: &tauri::AppHandle, push_to_talk: bool) {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
    let mods = Some(Modifiers::CONTROL | Modifiers::ALT);
    if app.global_shortcut().register(Shortcut::new(mods, Code::KeyM)).is_err() {
        eprintln!("could not register the Ctrl+Alt+M mute hotkey (already in use?)");
    }
    // Optional push-to-talk (settings.json `push_to_talk`): hold Ctrl+Alt+Space.
    if push_to_talk && app.global_shortcut().register(Shortcut::new(mods, Code::Space)).is_err() {
        eprintln!("could not register the Ctrl+Alt+Space push-to-talk hotkey (already in use?)");
    }
}

/// Reads the key without echo when stdin is a terminal; otherwise one line from the pipe
/// (use `< file`, not `echo key |`, so it stays out of shell history). Never prints it.
pub fn store_key_from_stdin() -> Result<(), String> {
    use std::io::IsTerminal;
    let line = if std::io::stdin().is_terminal() {
        eprint!("ElevenLabs key (hidden): ");
        rpassword::read_password().map_err(|_| "could not read the key")?
    } else {
        let mut l = String::new();
        std::io::stdin().read_line(&mut l).map_err(|_| "could not read stdin")?;
        l
    };
    let key = line.trim();
    if key.is_empty() {
        return Err("empty key".into());
    }
    secret::store_in_keyring(key)
}

pub fn run() {
    use tauri_plugin_global_shortcut::ShortcutState;
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, sc, event| {
                    let down = event.state() == ShortcutState::Pressed;
                    if sc.key == tauri_plugin_global_shortcut::Code::Space {
                        mic::ptt(app, down); // push-to-talk: held = listening
                    } else if down {
                        mic::toggle_hotkey(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let shared = Shared::load(dir.clone());
            let cfg = shared.team_config();
            let st = shared.settings();
            app.manage(shared);
            let mic = Arc::new(mic::MicState::new()); // starts MUTED, every launch
            mic.set_push_to_talk(st.push_to_talk);
            app.manage(mic);
            app.manage(Arc::new(call::CallWatch::default()));
            let me = cfg.me.clone();
            let team = Arc::new(Team::new(dir, cfg.me, cfg.remind_python, cfg.remind_script, cfg.auto_remind, autoboard::BoardPaths::from_config(&cfg.status_file, &cfg.team_asks_file, &st.cwd)));
            app.manage(team.clone());
            spawn_auto_remind(team);
            spawn_team_tick(app.handle().clone());
            let hub = Arc::new(hub::Hub::new(hub::DEFAULT_BASE)?);
            hub.set_me(&me);
            app.manage(hub.clone());
            hub::spawn_poll(hub, app.handle().clone());
            register_hotkeys(app.handle(), st.push_to_talk);
            if st.auto_pause {
                call::spawn_watch(app.handle().clone());
            }
            if let Some(w) = app.get_webview_window("main") {
                if let Err(e) = apply_mode(&w, &st.window_mode, st.main_size) {
                    eprintln!("could not set the window mode: {e}");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            key_status, get_settings, save_settings, list_voices, run_turn, approve_tool, deny_tool,
            cancel_turn, set_mode, get_mode, team_load, team_set_done, team_remind, get_team_prefs, set_team_prefs,
            mic::mic_start, mic::mic_frames, mic::mic_set_muted, mic::mic_set_barge, mic::mic_state, mic::mic_set_ptt,
            list_sessions, focus_session, test_voice, new_session, history_get, open_link, lock_state,
            hub_snapshot, hub_refresh, hub_ticket, hub_open_approval, hub_set_in_view, hub_nudge,
            session_route, session_pin
        ])
        .on_window_event(|window, event| {
            // Coming back to the window: poll the hub at once (Rust applies the minimum gap).
            if let tauri::WindowEvent::Focused(true) = event {
                if let Some(hub) = window.app_handle().try_state::<Arc<hub::Hub>>() {
                    hub.nudge();
                }
            }
            // Remember the size the user left the full window at.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(w) = window.app_handle().get_webview_window("main") {
                    remember_main_size(&w, &window.app_handle().state::<Shared>());
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building One")
        .run(|_app, event| {
            // The claude tree must not outlive the app (the Windows Job Object covers a crash too).
            if let tauri::RunEvent::Exit = event {
                proc::kill_all();
            }
        });
}
