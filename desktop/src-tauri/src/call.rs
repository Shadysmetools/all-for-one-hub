//! Call detection: when a call or meeting is going on, One keeps its mic off.
//!
//! Where it acts:
//!   - at UNMUTE: if a call is detected the mic stays off ("Call detected: mic stays off") until
//!     the user presses "Unmute anyway" (`CallWatch::check_unmute`);
//!   - while UNMUTED, every 3 s: a NEW call signal mutes the mic ("Paused: call detected"). It is a
//!     per-signal rising edge, so something that is always on (a mic held by a chat app, say)
//!     cannot hide a call that starts later. Nothing is polled while muted. Unmute stays manual.
//!
//! What counts as a signal (Windows only, all in-process, no child processes):
//!   1. a visible top-level window that is a call: Zoom's meeting helper, a Teams / Zoom / WhatsApp
//!      window whose title says meeting or call, or a browser tab "Meet - ...", "Zoom Meeting" or a
//!      Teams meeting;
//!   2. an app holding the microphone, read from the consent store under
//!      `CapabilityAccessManager\ConsentStore\microphone`: both the desktop apps (`NonPackaged`)
//!      and the packaged Store apps (new Teams, WhatsApp, Phone Link, ...).
//! One itself is ignored by its own process id and exe path, not by a fixed name. The matchers
//! and the edge logic are pure functions so they can be tested.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Shown when a call mutes the mic. (The refused-unmute text, "Call detected: mic stays off",
/// is shown by the webview, which is the one that asked.)
pub const PAUSED: &str = "Paused: call detected";

/// Windows that are, or hold, a call. `exe` is the window's process image path (or name).
/// Zoom's meeting helper (CptHost) only runs during a meeting. Teams, Zoom and WhatsApp windows
/// count only when the title says meeting or call, so an idle main window does not mute you.
/// Browser tabs count for Google Meet, Zoom and Teams meeting pages.
pub fn is_call_window(exe: &str, title: &str) -> bool {
    let exe = exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_lowercase();
    let t = title.to_lowercase();
    if exe == "cpthost.exe" {
        return true;
    }
    let says_call = t.contains("meeting") || t.contains("call");
    if matches!(exe.as_str(), "ms-teams.exe" | "teams.exe" | "zoom.exe") || exe.starts_with("whatsapp") {
        return says_call;
    }
    t.contains("meet -") || t.contains("zoom meeting") || (t.contains("microsoft teams") && says_call)
}

/// Consent-store entries `(key name, LastUsedTimeStop)` whose use has no stop time yet: the app
/// is using the microphone right now. Desktop app keys are exe paths with `\` written as `#`;
/// packaged apps use their package family name (`MSTeams_8wekyb3d8bbwe`).
pub fn holders_from(entries: &[(String, u64)]) -> Vec<String> {
    entries.iter().filter(|(_, stop)| *stop == 0).map(|(k, _)| k.clone()).collect()
}

/// True when something other than One itself holds the mic. `own_exe` is One's own exe path;
/// the WebView2 that hosts our UI is ignored only when `exclude_webview` (One is unmuted, so it
/// is the one holding the mic then).
#[cfg(test)]
pub fn others_hold_mic(holders: &[String], own_exe: &str, exclude_webview: bool) -> bool {
    !other_holders(holders, own_exe, exclude_webview).is_empty()
}

fn other_holders(holders: &[String], own_exe: &str, exclude_webview: bool) -> Vec<String> {
    let own = own_exe.to_lowercase();
    holders
        .iter()
        .filter(|h| {
            let path = h.replace('#', "\\").to_lowercase();
            let name = path.rsplit('\\').next().unwrap_or("").to_string();
            path != own && !(exclude_webview && name == "msedgewebview2.exe")
        })
        .cloned()
        .collect()
}

/// Polls a signal may be missing (3 s each, about 30 s) before an override is forgotten.
const OVERRIDE_QUIET_POLLS: u32 = 10;

#[derive(Default)]
struct WatchState {
    seen: HashSet<String>,
    /// Signals the user chose to unmute into ("Unmute anyway"), with how many polls each has
    /// been absent. A signal that blinks (another browser tab and back) stays overridden.
    overridden: HashMap<String, u32>,
}

impl WatchState {
    fn age(&mut self, now: &HashSet<String>) {
        self.overridden.retain(|k, absent| {
            if now.contains(k) {
                *absent = 0;
                true
            } else {
                *absent += 1;
                *absent < OVERRIDE_QUIET_POLLS
            }
        });
    }

    /// The signals that still count: all but the ones the user overrode.
    fn effective(&self, now: &HashSet<String>) -> HashSet<String> {
        now.iter().filter(|s| !self.overridden.contains_key(*s)).cloned().collect()
    }
}

/// Per-signal edge detection and the "unmute anyway" override. Pure over sets of signals.
#[derive(Default)]
pub struct CallWatch {
    state: Mutex<WatchState>,
}

impl CallWatch {
    /// The user asks to unmute. Allowed when no call signal is present (ones the user already
    /// overrode do not count), or when `force` (the explicit "Unmute anyway"): a forced unmute
    /// overrides the signals present, for as long as they last and about 30 s after they
    /// vanish. A DIFFERENT signal afterwards still mutes.
    pub fn check_unmute(&self, now: &HashSet<String>, force: bool) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let eff = st.effective(now);
        if eff.is_empty() {
            st.seen.clear();
            true
        } else if force {
            st.overridden.extend(eff.into_iter().map(|s| (s, 0)));
            st.seen.clear();
            true
        } else {
            false
        }
    }

    /// The mic became muted (user, hotkey or auto-pause): every override ends, so a later call
    /// is judged afresh, even in an app that was overridden before.
    pub fn clear_overrides(&self) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.overridden.clear();
        st.seen.clear();
    }

    /// One poll while unmuted. True when a signal appeared that was not there before.
    pub fn tick(&self, now: HashSet<String>) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.age(&now);
        let eff = st.effective(&now);
        let fresh = eff.difference(&st.seen).next().is_some();
        st.seen = eff;
        fresh
    }
}

#[cfg(windows)]
mod win {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, ERROR_SUCCESS, HWND, LPARAM};
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_QWORD};
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible};

    fn exe_of(pid: u32) -> Option<String> {
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    struct Found {
        own_pid: u32,
        out: Vec<(String, String)>,
    }

    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the `Found` pointer passed by `windows()`, alive for the whole enumeration.
        let found = &mut *(lparam as *mut Found);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut title = [0u16; 512];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if n <= 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == found.own_pid {
            return 1; // One's own window is never a call
        }
        if let Some(exe) = exe_of(pid) {
            found.out.push((exe, String::from_utf16_lossy(&title[..n as usize])));
        }
        1
    }

    /// (exe path, title) of every visible top-level window with a title, except One's own.
    pub fn windows() -> Vec<(String, String)> {
        let mut found = Found { own_pid: std::process::id(), out: Vec::new() };
        // SAFETY: `each` only touches `found` while EnumWindows runs.
        unsafe { EnumWindows(Some(each), &mut found as *mut _ as LPARAM) };
        found.out
    }

    const MIC: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn open(parent: HKEY, path: &str) -> Option<HKEY> {
        let mut h: HKEY = std::mem::zeroed();
        (RegOpenKeyExW(parent, wide(path).as_ptr(), 0, KEY_READ, &mut h) == ERROR_SUCCESS).then_some(h)
    }

    unsafe fn subkeys(h: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..4000u32 {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(h, i, buf.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut()) != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }

    unsafe fn stop_time(parent: HKEY, name: &str) -> Option<u64> {
        let key = open(parent, name)?;
        let (mut data, mut size, mut ty) = (0u64, 8u32, 0u32);
        let r = RegQueryValueExW(key, wide("LastUsedTimeStop").as_ptr(), null(), &mut ty, &mut data as *mut u64 as *mut u8, &mut size);
        RegCloseKey(key);
        (r == ERROR_SUCCESS && ty == REG_QWORD).then_some(data)
    }

    /// `(key name, LastUsedTimeStop)` of every app in the microphone consent store, packaged and
    /// not. None when the store cannot be read (the caller says so once and uses windows only).
    pub fn mic_entries() -> Option<Vec<(String, u64)>> {
        // SAFETY: plain registry calls on handles opened and closed in this function.
        unsafe {
            let root = open(HKEY_CURRENT_USER, MIC)?;
            let mut out = Vec::new();
            for name in subkeys(root) {
                if name == "NonPackaged" {
                    if let Some(np) = open(root, &name) {
                        out.extend(subkeys(np).into_iter().filter_map(|k| stop_time(np, &k).map(|t| (k, t))));
                        RegCloseKey(np);
                    }
                } else if let Some(t) = stop_time(root, &name) {
                    out.push((name, t)); // a packaged (Store) app
                }
            }
            RegCloseKey(root);
            Some(out)
        }
    }

    pub fn exe_path() -> String {
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Every call signal right now. `exclude_webview`: One is unmuted, so its own WebView2 holds the mic.
#[cfg(windows)]
pub fn signals(exclude_webview: bool) -> HashSet<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    let mut out = HashSet::new();
    for (exe, title) in win::windows() {
        if is_call_window(&exe, &title) {
            let name = exe.rsplit('\\').next().unwrap_or(&exe).to_lowercase();
            out.insert(format!("win:{name}:{title}"));
        }
    }
    match win::mic_entries() {
        Some(entries) => {
            for h in other_holders(&holders_from(&entries), &win::exe_path(), exclude_webview) {
                out.insert(format!("mic:{h}"));
            }
        }
        // Visible, once: the mic part of detection is off, the window part still works.
        None if !WARNED.swap(true, Ordering::SeqCst) => eprintln!("call detection: the microphone consent store could not be read; using window titles only"),
        None => {}
    }
    out
}

#[cfg(not(windows))]
pub fn signals(_exclude_webview: bool) -> HashSet<String> {
    HashSet::new()
}

/// Poll every 3 s, only while unmuted (while muted there is nothing to protect and nothing runs).
pub fn spawn_watch(app: tauri::AppHandle) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let mic = app.state::<std::sync::Arc<crate::mic::MicState>>();
            if mic.is_muted() {
                continue;
            }
            let Ok(now) = tokio::task::spawn_blocking(|| signals(true)).await else { continue };
            if first && !now.is_empty() {
                // An app that is always on must not silently blind the detector: say so once.
                eprintln!("call detection: {} signal(s) were already present when listening began; only NEW ones will pause the mic", now.len());
            }
            first = false;
            if app.state::<std::sync::Arc<CallWatch>>().tick(now) {
                crate::mic::pause_for_call(&app, PAUSED);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn zoom_meeting_helper_and_call_windows_match() {
        assert!(is_call_window(r"C:\Users\x\AppData\Roaming\Zoom\bin\CptHost.exe", ""));
        assert!(is_call_window(r"C:\Program Files\Zoom\bin\Zoom.exe", "Zoom Meeting"));
        assert!(is_call_window(r"C:\x\ms-teams.exe", "Meeting with Riley | Microsoft Teams"));
        assert!(is_call_window(r"C:\x\Teams.exe", "Call with Maya"));
    }

    #[test]
    fn whatsapp_call_windows_match_but_an_idle_chat_does_not() {
        assert!(is_call_window(r"C:\Program Files\WindowsApps\5319275A.WhatsAppDesktop_x\WhatsApp.Root.exe", "WhatsApp Call"));
        assert!(is_call_window(r"C:\x\WhatsApp.exe", "Voice call - Maya"));
        assert!(!is_call_window(r"C:\x\WhatsApp.exe", "WhatsApp"));
    }

    #[test]
    fn an_idle_teams_or_zoom_window_does_not_match() {
        assert!(!is_call_window(r"C:\x\ms-teams.exe", "Chat | Microsoft Teams"));
        assert!(!is_call_window(r"C:\x\Zoom.exe", "Zoom Workplace"));
    }

    #[test]
    fn browser_tabs_for_meet_zoom_and_teams_match() {
        assert!(is_call_window(r"C:\x\chrome.exe", "Meet - abc-defg-hij - Google Chrome"));
        assert!(is_call_window(r"C:\x\msedge.exe", "Zoom Meeting - Microsoft Edge"));
        assert!(is_call_window(r"C:\x\firefox.exe", "Meeting | Microsoft Teams - Mozilla Firefox"));
        assert!(!is_call_window(r"C:\x\chrome.exe", "Inbox - Gmail - Google Chrome"));
        assert!(!is_call_window(r"C:\x\chrome.exe", "Teams pricing - Microsoft Teams"));
    }

    fn entries() -> Vec<(String, u64)> {
        vec![
            ("C:#Program Files#Google#Chrome#Application#chrome.exe".into(), 0),
            ("C:#Apps#old.exe".into(), 0x1dc5a3b2c4d5e6f),
            ("MSTeams_8wekyb3d8bbwe".into(), 0),
            ("5319275A.WhatsAppDesktop_cv1g1gvanyjgm".into(), 0),
            ("Microsoft.WindowsCamera_8wekyb3d8bbwe".into(), 0x1dc00000000000),
        ]
    }

    #[test]
    fn holders_include_packaged_store_apps_and_skip_entries_with_a_stop_time() {
        let h = holders_from(&entries());
        assert_eq!(h.len(), 3);
        assert!(h.contains(&"MSTeams_8wekyb3d8bbwe".to_string()), "new Teams is a packaged app");
        assert!(h.iter().any(|k| k.contains("WhatsAppDesktop")), "so is WhatsApp");
        assert!(!h.iter().any(|k| k.contains("old.exe") || k.contains("WindowsCamera")));
    }

    #[test]
    fn one_itself_is_ignored_by_its_own_path_not_by_a_fixed_name() {
        let own = r"E:\apps\Renamed One.exe";
        let me = vec!["E:#apps#Renamed One.exe".to_string()];
        assert!(!others_hold_mic(&me, own, false), "even if the binary is renamed");
        // a different app that merely has the same file name elsewhere is another app
        assert!(others_hold_mic(&["C:#x#Renamed One.exe".to_string()], own, false));
        assert!(others_hold_mic(&["C:#x#one-desktop.exe".to_string()], own, false), "no hard-coded name");
    }

    #[test]
    fn webview2_is_ignored_only_while_one_itself_is_unmuted() {
        let wv = vec!["C:#Program Files (x86)#Microsoft#EdgeWebView#Application#msedgewebview2.exe".to_string()];
        assert!(!others_hold_mic(&wv, "x", true), "unmuted: that is our own UI holding the mic");
        assert!(others_hold_mic(&wv, "x", false), "muted: so it must be another WebView2 app");
    }

    #[test]
    fn launch_muted_during_a_call_then_unmute_is_blocked_until_forced() {
        let w = CallWatch::default();
        let call = set(&["win:chrome.exe:Meet - abc"]);
        assert!(!w.check_unmute(&call, false), "a call is on: the mic stays off");
        assert!(!w.check_unmute(&call, false));
        assert!(w.check_unmute(&call, true), "explicit Unmute anyway");
        assert!(!w.tick(call.clone()), "the same call does not mute it again 3 s later");
        assert!(w.tick(set(&["win:chrome.exe:Meet - abc", "mic:MSTeams_8wekyb3d8bbwe"])), "but a new signal does");
    }

    #[test]
    fn no_call_means_unmute_is_allowed_and_a_new_call_pauses() {
        let w = CallWatch::default();
        assert!(w.check_unmute(&HashSet::new(), false));
        assert!(!w.tick(HashSet::new()));
        assert!(w.tick(set(&["mic:chrome"])), "a call starts");
        assert!(!w.tick(set(&["mic:chrome"])), "still the same call");
        assert!(!w.tick(HashSet::new()), "it ends");
        assert!(w.tick(set(&["mic:chrome"])), "the next call pauses again");
    }

    #[test]
    fn r2_l1_after_unmute_anyway_a_blinking_signal_does_not_mute_again() {
        let w = CallWatch::default();
        let meet = set(&["win:chrome.exe:Meet - abc"]);
        assert!(w.check_unmute(&meet, true));
        assert!(!w.tick(meet.clone()));
        // the user switches to another tab and back: the Meet window vanishes for a poll or two
        assert!(!w.tick(HashSet::new()));
        assert!(!w.tick(HashSet::new()));
        assert!(!w.tick(meet.clone()), "the same call must not mute it again");
        // unmuting again within the same call needs no second override
        assert!(w.check_unmute(&meet, false));
        // a different call still pauses
        assert!(w.tick(set(&["win:chrome.exe:Meet - abc", "mic:WhatsAppDesktop"])));
    }

    #[test]
    fn r3_m1_muting_clears_the_overrides_so_a_later_call_in_the_same_app_is_detected() {
        let w = CallWatch::default();
        let meet = set(&["win:chrome.exe:Meet - first call"]);
        assert!(w.check_unmute(&meet, true), "Unmute anyway");
        assert!(!w.tick(meet.clone()));
        w.clear_overrides(); // the mic is muted (user, hotkey or auto-pause)
        // later: unmute while a call is on in the same browser: detected, not waved through
        let later = set(&["win:chrome.exe:Meet - first call"]);
        assert!(!w.check_unmute(&later, false), "the old override must be gone");
        assert!(w.check_unmute(&later, true));
        w.clear_overrides();
        assert!(w.check_unmute(&HashSet::new(), false) && w.tick(set(&["win:chrome.exe:Meet - second call"])), "a new call signal is detected");
    }

    #[test]
    fn r2_l1_the_override_is_forgotten_after_about_30_seconds_without_the_signal() {
        let w = CallWatch::default();
        let call = set(&["win:zoom.exe:Zoom Meeting"]);
        assert!(w.check_unmute(&call, true));
        for _ in 0..(OVERRIDE_QUIET_POLLS - 1) {
            assert!(!w.tick(HashSet::new()));
        }
        assert!(!w.tick(call.clone()), "back within the window: still overridden");
        for _ in 0..OVERRIDE_QUIET_POLLS {
            assert!(!w.tick(HashSet::new()));
        }
        assert!(w.tick(call), "after a quiet spell a new call pauses again");
    }

    #[test]
    fn an_always_on_holder_does_not_hide_a_later_call() {
        let w = CallWatch::default();
        let discord = "mic:C:#Discord#Discord.exe";
        // it was already holding the mic when listening began
        w.check_unmute(&set(&[discord]), true);
        assert!(!w.tick(set(&[discord])), "always on: not a rising edge");
        assert!(!w.tick(set(&[discord])));
        assert!(w.tick(set(&[discord, "win:zoom.exe:Zoom Meeting"])), "a real call still pauses");
    }
}
