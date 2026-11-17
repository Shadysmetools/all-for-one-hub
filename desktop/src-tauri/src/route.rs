//! Which session "Chat with One" and the overall "Ask about" talk to by default.
//!
//! Always a LIVE session, never a dead one. The order:
//!   1. a session the user pinned, for as long as it is live;
//!   2. the coordinator (its name is a setting; default `coordinator`), when it is live;
//!   3. the newest live `all-for-one-<n>` session (the highest n wins; `all-for-one-desktop*` never).
//!
//! A per-session "Ask about X" is not routed here: it stays on X. "Live" is the existing rule in
//! `live.rs` (it already treats a finished job as dead); this module only chooses among tabs that
//! `tabs::mark_live` has marked. `resolve` is a pure function so every rule is unit-tested.

use crate::tabs::SessionTab;
use serde::Serialize;
use std::sync::Mutex;

pub const DEFAULT_COORDINATOR: &str = "coordinator";
pub const NO_LIVE: &str = "No live All for One session";
const AFO_PREFIX: &str = "all-for-one-";

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Pin,
    Coordinator,
    AllForOne,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Route {
    pub via: Option<Via>,
    pub session_id: Option<String>,
    /// What the human knows the session by (the terminal tab title).
    pub tab_title: String,
    pub name: Option<String>,
    /// One plain sentence saying which session is used and why (English; the screen has its own words too).
    pub message: String,
    /// A pin is set (it may be dead: see `pin_fell_back`).
    pub pinned: bool,
    pub pin_fell_back: bool,
}

/// `n` of `all-for-one-<n>`; None for anything else, `all-for-one-desktop*` included.
pub fn afo_number(label: &str) -> Option<u64> {
    let l = label.trim().to_lowercase();
    let rest = l.strip_prefix(AFO_PREFIX)?;
    if rest.is_empty() || rest.len() > 9 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn labels(t: &SessionTab) -> [&str; 2] {
    [t.tab_title.as_str(), t.name.as_deref().unwrap_or("")]
}

/// The live `all-for-one-<n>` session with the highest n (the newest of equals: tabs are newest first).
pub fn newest_afo(tabs: &[SessionTab]) -> Option<&SessionTab> {
    let mut best: Option<(u64, &SessionTab)> = None;
    for t in tabs.iter().filter(|t| t.live) {
        let Some(n) = labels(t).iter().filter_map(|l| afo_number(l)).max() else { continue };
        if best.map_or(true, |(b, _)| n > b) {
            best = Some((n, t));
        }
    }
    best.map(|(_, t)| t)
}

/// The live session called `name` (tab title or session name, any case).
pub fn find_live<'a>(tabs: &'a [SessionTab], name: &str) -> Option<&'a SessionTab> {
    let want = name.trim().to_lowercase();
    tabs.iter().find(|t| t.live && !want.is_empty() && labels(t).iter().any(|l| l.trim().to_lowercase() == want))
}

fn pick(t: &SessionTab, via: Via, message: String, pinned: bool, fell_back: bool) -> Route {
    Route { via: Some(via), session_id: Some(t.session_id.clone()), tab_title: t.tab_title.clone(), name: t.name.clone(), message, pinned, pin_fell_back: fell_back }
}

/// Choose the target. `pin` is a session id.
pub fn resolve(tabs: &[SessionTab], coordinator: &str, pin: Option<&str>) -> Route {
    let pinned = pin.is_some();
    if let Some(t) = pin.and_then(|id| tabs.iter().find(|t| t.live && t.session_id == id)) {
        return pick(t, Via::Pin, format!("Pinned: {}", t.tab_title), true, false);
    }
    let lost = if pinned { " The pinned session is no longer live, so the default is used." } else { "" };
    if let Some(t) = find_live(tabs, coordinator) {
        return pick(t, Via::Coordinator, format!("Using the coordinator: {}.{lost}", t.tab_title), pinned, pinned);
    }
    if let Some(t) = newest_afo(tabs) {
        let m = format!("The coordinator is not available to One (not live, or in another project), so using the newest live All for One session: {}.{lost}", t.tab_title);
        return pick(t, Via::AllForOne, m, pinned, pinned);
    }
    Route { message: format!("{NO_LIVE}.{lost}"), pinned, pin_fell_back: pinned, ..Default::default() }
}

// ---- the pin: kept in memory until the session stops being live ----------------------------

static PIN: Mutex<Option<String>> = Mutex::new(None);

pub fn pin() -> Option<String> {
    PIN.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Pin a live session, or clear the pin (`None`). Refuses anything that is not a live session now.
pub fn set_pin(tabs: &[SessionTab], id: Option<&str>) -> Result<(), String> {
    let next = match id {
        None => None,
        Some(id) if tabs.iter().any(|t| t.live && t.session_id == id) => Some(id.to_string()),
        Some(_) => return Err("That session is not live, so it cannot be pinned.".into()),
    };
    *PIN.lock().unwrap_or_else(|p| p.into_inner()) = next;
    Ok(())
}

/// The route from the real session folders and the saved pin.
pub fn current(cwd: &str, coordinator: &str) -> Route {
    resolve(&crate::tabs::list_sessions(cwd), coordinator, pin().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(title: &str, sid: &str, live: bool) -> SessionTab {
        SessionTab { tab_title: title.into(), session_id: sid.into(), project: "p".into(), modified: 0, name: None, live, status: String::new() }
    }
    fn id(r: &Route) -> Option<&str> {
        r.session_id.as_deref()
    }
    const COORD: &str = DEFAULT_COORDINATOR;

    #[test]
    fn only_numbered_all_for_one_names_match() {
        assert_eq!(afo_number("all-for-one-6"), Some(6));
        assert_eq!(afo_number(" All-For-One-12 "), Some(12));
        for no in ["all-for-one-desktop", "all-for-one-desktop-2", "all-for-one", "all-for-one-", "all-for-one-6b", "my-all-for-one-6", "all-for-one-1234567890"] {
            assert_eq!(afo_number(no), None, "{no}");
        }
    }

    #[test]
    fn a_dead_5_and_a_live_6_gives_6() {
        let tabs = [tab("all-for-one-5", "s5", false), tab("all-for-one-6", "s6", true)];
        assert_eq!(id(&resolve(&tabs, "nobody", None)), Some("s6"));
        assert_eq!(newest_afo(&tabs).map(|t| t.session_id.as_str()), Some("s6"));
    }

    #[test]
    fn two_live_gives_the_highest_n_even_if_the_lower_is_newer() {
        let tabs = [tab("all-for-one-4", "s4", true), tab("all-for-one-7", "s7", true), tab("all-for-one-desktop", "sd", true)];
        assert_eq!(id(&resolve(&tabs, "nobody", None)), Some("s7"));
    }

    #[test]
    fn the_name_counts_as_well_as_the_tab_title() {
        let mut t = tab("Support work", "s3", true);
        t.name = Some("all-for-one-3".into());
        assert_eq!(id(&resolve(&[t], "nobody", None)), Some("s3"));
    }

    #[test]
    fn none_live_gives_none_and_a_plain_message() {
        let r = resolve(&[tab("all-for-one-5", "s5", false), tab("all-for-one-desktop", "sd", true)], "nobody", None);
        assert_eq!((r.via, r.session_id.clone()), (None, None));
        assert!(r.message.starts_with(NO_LIVE), "{}", r.message);
        assert_eq!(resolve(&[], COORD, None).via, None);
    }

    #[test]
    fn a_live_coordinator_wins_over_all_for_one() {
        let tabs = [tab("all-for-one-9", "s9", true), tab(COORD, "co", true)];
        let r = resolve(&tabs, COORD, None);
        assert_eq!((id(&r), r.via), (Some("co"), Some(Via::Coordinator)));
    }

    #[test]
    fn a_dead_coordinator_falls_back_to_the_newest_live_all_for_one() {
        let tabs = [tab(COORD, "co", false), tab("all-for-one-2", "s2", false), tab("all-for-one-6", "s6", true)];
        let r = resolve(&tabs, COORD, None);
        assert_eq!((id(&r), r.via), (Some("s6"), Some(Via::AllForOne)));
        assert!(r.message.contains("coordinator is not available") && r.message.contains("all-for-one-6"));
    }

    #[test]
    fn the_coordinator_name_is_a_setting() {
        let tabs = [tab("boss-session", "b", true), tab("all-for-one-1", "s1", true)];
        assert_eq!(id(&resolve(&tabs, "Boss-Session", None)), Some("b"));
        assert_eq!(id(&resolve(&tabs, "", None)), Some("s1"), "an empty name matches nothing");
    }

    #[test]
    fn a_pin_overrides_both_until_it_dies() {
        let mut tabs = vec![tab(COORD, "co", true), tab("all-for-one-6", "s6", true), tab("other work", "ot", true)];
        let r = resolve(&tabs, COORD, Some("ot"));
        assert_eq!((id(&r), r.via, r.pinned, r.pin_fell_back), (Some("ot"), Some(Via::Pin), true, false));
        tabs[2].live = false; // the pinned session stops being live
        let r = resolve(&tabs, COORD, Some("ot"));
        assert_eq!((id(&r), r.via, r.pin_fell_back), (Some("co"), Some(Via::Coordinator), true));
        assert!(r.message.contains("pinned session is no longer live"));
        tabs[0].live = false;
        assert_eq!(id(&resolve(&tabs, COORD, Some("ot"))), Some("s6"), "falls back all the way to all-for-one");
        tabs[1].live = false;
        let r = resolve(&tabs, COORD, Some("ot"));
        assert!(r.session_id.is_none() && r.pin_fell_back && r.message.starts_with(NO_LIVE));
    }

    #[test]
    fn only_a_live_session_can_be_pinned() {
        let tabs = [tab("a", "live1", true), tab("b", "dead1", false)];
        assert!(set_pin(&tabs, Some("dead1")).is_err());
        assert!(set_pin(&tabs, Some("nope")).is_err());
        assert!(set_pin(&tabs, Some("live1")).is_ok());
        assert_eq!(pin().as_deref(), Some("live1"));
        assert!(set_pin(&tabs, None).is_ok());
        assert_eq!(pin(), None);
    }
}
