//! Why a hub request failed, and the back-off rules around it (split out of hub.rs).

use crate::hub::HubStatus;
use crate::hub_lifecycle as lc;

#[derive(Debug, PartialEq)]
pub enum Fail {
    Offline,
    Rejected,
    /// 404 on the list: the hub runs without its API.
    NotEnabled,
    /// 429: the hub asked us to wait (seconds, from Retry-After).
    Busy(u64),
    Server(u16),
    Unreadable,
}

/// How long to wait after a 429: the hub's Retry-After (seconds), but never less than one poll.
pub fn retry_after_secs(header: Option<&str>) -> u64 {
    let asked = header.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
    asked.clamp(lc::POLL_HIDDEN.as_secs(), 600)
}

/// Two failures in one poll: a 429 wins, so its back-off is never lost; else the first.
pub fn prefer_busy(first: Option<Fail>, second: Fail) -> Fail {
    match (first, second) {
        (Some(Fail::Busy(s)), _) | (_, Fail::Busy(s)) => Fail::Busy(s),
        (Some(a), _) => a,
        (None, b) => b,
    }
}

impl Fail {
    pub(crate) fn status(&self) -> HubStatus {
        match self {
            Fail::Rejected => HubStatus::Rejected,
            Fail::NotEnabled => HubStatus::NotEnabled,
            Fail::Busy(_) => HubStatus::Busy,
            _ => HubStatus::Offline,
        }
    }
    pub(crate) fn message(&self) -> String {
        match self {
            Fail::Offline => "Offline: hub not reachable".into(),
            Fail::Rejected => "The hub token is missing or wrong.".into(),
            Fail::NotEnabled => "The hub API is not turned on.".into(),
            Fail::Busy(s) => format!("The hub asked us to slow down. Trying again in {s} seconds."),
            Fail::Server(c) => format!("Offline: the hub had a problem (error {c})"),
            Fail::Unreadable => "Offline: the hub sent an answer I could not read".into(),
        }
    }
}
