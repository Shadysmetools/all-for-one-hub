//! The ticket lifecycle and the "who needs me" view built from the hub's data. Pure functions only:
//! no network, no clock (callers pass `now`), so every rule here is unit-tested in `hub_tests.rs`.
//!
//! - `Lifecycle`: the live step a ticket is at. Newer hubs send it; for an older hub it is derived
//!   from the fields every hub sends (pending approval, stage, status, waiting_on).
//! - `HubBrief`: what the Team view needs: is the hub reachable, and which tickets wait for a person.
//! - `poll_interval` / `wake_allowed`: when the poll loop runs next (15 s in view, 30 s hidden).

use crate::hub::{clean, flex_text, ts_of, Approval, HubStatus, Ticket};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// How often the hub is polled while the Tickets screen or the Team view is on screen.
pub const POLL_ACTIVE: Duration = Duration::from_secs(15);
/// ... and while the window is hidden, minimised or on another tab.
pub const POLL_HIDDEN: Duration = Duration::from_secs(30);
/// A closed ticket stays on the Tickets screen this long after it closed.
pub const CLOSED_KEEP_SECS: i64 = 48 * 3600;
/// Rows shown in the Team view's "Hub tickets need you" card.
const MAX_NEEDS: usize = 8;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    #[default]
    New,
    NeedsApproval,
    Approved,
    ReplySent,
    ReplyDelivered,
    ReplyRead,
    AwaitingCustomer,
    Closed,
    Held,
}

impl Lifecycle {
    /// The hub's word for a step (any case, `-` or space for `_`), or a few plain synonyms.
    pub fn parse(raw: &str) -> Option<Lifecycle> {
        let k = raw.trim().to_lowercase().replace(['-', ' '], "_");
        // the hub's owner stages (awaiting_owner, awaiting_owner_advance, awaiting_owner_resolve) and a plain
        // "waiting for owner": a person has to act, which is the needs-approval step
        if k.starts_with("awaiting_owner") || k.starts_with("waiting_for_owner") || k == "waiting_owner" || k == "owner" {
            return Some(Lifecycle::NeedsApproval);
        }
        Some(match k.as_str() {
            "new" | "open" | "opened" => Lifecycle::New,
            "needs_approval" | "pending_approval" | "awaiting_approval" => Lifecycle::NeedsApproval,
            "approved" => Lifecycle::Approved,
            "reply_sent" | "replied" | "sent" => Lifecycle::ReplySent,
            "reply_delivered" | "delivered" => Lifecycle::ReplyDelivered,
            "reply_read" | "read" => Lifecycle::ReplyRead,
            "awaiting_customer" | "waiting_customer" | "awaiting_client" => Lifecycle::AwaitingCustomer,
            "closed" | "cancelled" | "canceled" | "solved" | "resolved" | "done" => Lifecycle::Closed,
            "held" | "on_hold" | "hold" => Lifecycle::Held,
            _ => return None,
        })
    }
}

/// The step for a hub that does not send `lifecycle`: from pending approval, stage, status, waiting_on.
pub fn derive_lifecycle(has_pending_approval: bool, stage: &str, status: &str, waiting_on: &str) -> Lifecycle {
    let (s, st) = (Lifecycle::parse(stage), Lifecycle::parse(status));
    if has_pending_approval {
        return Lifecycle::NeedsApproval;
    }
    if s == Some(Lifecycle::Closed) || st == Some(Lifecycle::Closed) {
        return Lifecycle::Closed;
    }
    // a stage such as "awaiting_customer" or "held" names the step itself; a plain "open" status does not
    if let Some(l) = s.filter(|l| *l != Lifecycle::New).or(st.filter(|l| *l != Lifecycle::New)) {
        return l;
    }
    // waiting_on is free text: only the customer itself counts ("client success" does not)
    let w = waiting_on.trim().to_lowercase();
    if matches!(w.as_str(), "customer" | "client" | "the customer" | "the client" | "العميل") {
        return Lifecycle::AwaitingCustomer;
    }
    Lifecycle::parse(&w).filter(|l| *l != Lifecycle::New).unwrap_or(Lifecycle::New)
}

/// The last message the hub sent to the customer, and how it went.
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Delivery {
    /// "ack" (the instant acknowledgement) or "reply".
    pub kind: String,
    /// "sent", "delivered", "read" or "failed".
    pub status: String,
    pub at_ts: i64,
    pub ago_secs: Option<i64>,
}

/// `last_delivery` from the hub; None unless kind and status are words we know (nothing unknown is shown).
pub fn parse_delivery(v: &Value) -> Option<Delivery> {
    let o = v.as_object()?;
    let word = |k: &str, ok: &[&str]| {
        let w = clean(&flex_text(o.get(k)), 20, false).to_lowercase();
        ok.contains(&w.as_str()).then_some(w)
    };
    Some(Delivery {
        kind: word("kind", &["ack", "reply"])?,
        status: word("status", &["sent", "delivered", "read", "failed"])?,
        at_ts: ts_of(o.get("at").unwrap_or(&Value::Null)),
        ago_secs: None,
    })
}

/// Drop closed tickets older than `CLOSED_KEEP_SECS`; a closed ticket with no time at all stays.
pub fn keep_recent_closed(list: &mut Vec<Ticket>, now: i64) {
    list.retain(|t| {
        let at = if t.lifecycle_at_ts > 0 { t.lifecycle_at_ts } else { t.updated_ts };
        t.lifecycle != Lifecycle::Closed || at == 0 || now - at <= CLOSED_KEEP_SECS
    });
}

/// Add the recently-closed answer to the open list (an id already in the list wins), newest first.
pub fn merge_closed(list: &mut Vec<Ticket>, closed: Vec<Ticket>, now: i64, cap: usize) {
    for t in closed.into_iter().filter(|t| t.lifecycle == Lifecycle::Closed) {
        if !list.iter().any(|x| x.id == t.id) {
            list.push(t);
        }
    }
    keep_recent_closed(list, now);
    list.sort_by_key(|t| std::cmp::Reverse(t.updated_ts));
    list.truncate(cap);
}

// ---- the Team view's "hub tickets need you" ------------------------------------------

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct NeedsYou {
    pub ticket_id: String,
    pub odoo_ticket_id: String,
    pub customer_name: String,
    /// One line, plain text.
    pub summary: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct HubBrief {
    pub status: HubStatus,
    /// The first few tickets that need a person (see `needs_count` for the total).
    pub needs: Vec<NeedsYou>,
    pub needs_count: usize,
    pub last_ok_ago_secs: Option<i64>,
}

fn one_line(s: &str) -> String {
    clean(s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""), 110, false)
}

/// May `me` approve? An empty approver list (or no known user) means anybody may.
fn approver_ok(approvers: &str, me: &str) -> bool {
    let me = me.trim().to_lowercase();
    me.is_empty() || approvers.trim().is_empty() || approvers.split(',').any(|n| n.trim().to_lowercase() == me)
}

/// What the screens last got, minus the ages that change every second (see `Hub::take_change`).
#[derive(PartialEq)]
pub struct Sent {
    pub status: HubStatus,
    pub message: String,
    pub tickets: Vec<Ticket>,
    pub approvals: Vec<Approval>,
}

/// Work out the ages of a ticket for `now`.
pub fn age_ticket(t: &mut Ticket, now: i64) {
    let ago = |ts: i64| (ts > 0).then(|| (now - ts).max(0));
    t.updated_ago_secs = ago(t.updated_ts);
    t.lifecycle_ago_secs = ago(t.lifecycle_at_ts);
    if let Some(d) = t.last_delivery.as_mut() {
        d.ago_secs = ago(d.at_ts);
    }
}

/// Every ticket that waits for an approval: the pending-approvals list plus tickets whose step says so.
pub fn needs_you(tickets: &[Ticket], approvals: &[Approval], me: &str) -> Vec<NeedsYou> {
    let mut out: Vec<NeedsYou> = Vec::new();
    for a in approvals.iter().filter(|a| approver_ok(&a.approvers, me)) {
        let t = tickets.iter().find(|t| t.id == a.ticket_id);
        let summary = t.map(|t| one_line(&t.summary)).filter(|s| !s.is_empty()).unwrap_or_else(|| clean(&a.kind_label, 110, false));
        out.push(NeedsYou { ticket_id: a.ticket_id.clone(), odoo_ticket_id: a.odoo_ticket_id.clone(), customer_name: a.customer_name.clone(), summary });
    }
    // a ticket at the needs-approval step with no pending approval at all (the owner has to act); one whose
    // approval is for somebody else is not yours
    for t in tickets.iter().filter(|t| t.lifecycle == Lifecycle::NeedsApproval) {
        if !out.iter().any(|n| n.ticket_id == t.id) && !approvals.iter().any(|a| a.ticket_id == t.id) {
            out.push(NeedsYou { ticket_id: t.id.clone(), odoo_ticket_id: t.odoo_ticket_id.clone(), customer_name: t.customer_name.clone(), summary: one_line(&t.summary) });
        }
    }
    out
}

/// A brief from the hub's cached state: the first `MAX_NEEDS` rows and the true total.
pub fn brief(status: HubStatus, tickets: &[Ticket], approvals: &[Approval], last_ok_ago_secs: Option<i64>, me: &str) -> HubBrief {
    let mut needs = needs_you(tickets, approvals, me);
    let needs_count = needs.len();
    needs.truncate(MAX_NEEDS);
    HubBrief { status, needs, needs_count, last_ok_ago_secs }
}

/// "You are all caught up" is true only when nobody asks you for anything, no hub ticket waits for you,
/// AND the hub answered: an unreachable hub proves nothing.
pub fn team_caught_up(asks_on_me: usize, hub: &HubBrief) -> bool {
    asks_on_me == 0 && hub.needs_count == 0 && hub.status == HubStatus::Connected
}

// ---- when to poll ---------------------------------------------------------------------

/// The wait before the next poll: 15 s in view, 30 s hidden, never less than a 429's Retry-After.
pub fn poll_interval(in_view: bool, retry_secs: u64) -> Duration {
    (if in_view { POLL_ACTIVE } else { POLL_HIDDEN }).max(Duration::from_secs(retry_secs))
}

/// May a "window gained focus" / "tab opened" nudge poll now? Not within `min_gap` of the last poll,
/// and never while the hub has told us to back off (`retry_secs`).
pub fn wake_allowed(since_last: Option<Duration>, min_gap: Duration, retry_secs: u64) -> bool {
    since_last.map_or(true, |d| d >= min_gap.max(Duration::from_secs(retry_secs)))
}
