//! The All for One hub, read-only: open tickets and pending approvals for the Tickets screen.
//!
//! All logic is here; the webview only draws what these commands return.
//! - Loopback only: the base URL must be `127.0.0.1` / `::1` (checked on construction and on every
//!   request), no proxy is used and no redirect is followed, so the bearer token cannot leave the
//!   machine. The token is read from the secrets folder on each poll (rotation needs no restart),
//!   is wrapped in a redacting type and never logged, stored in an error or sent to the webview.
//! - Tolerant parsing: the hub API is still moving, so every field is optional, unknown fields are
//!   ignored, ids and times may be numbers or strings, and one bad row never drops the list.
//! - No approving from here. "Open to approve" opens `https://<AFO_ADMIN_HOST>/tickets/<id>`,
//!   built in Rust from the numeric ticket id. The hub's own `approve_url` is ignored, and the
//!   webview sends a ticket id and fingerprint, never an address. `link::open_vetted` enforces it.
//! - Known limit: the token goes over plain http to whatever listens on 127.0.0.1:8891 (loopback
//!   only, so only a process on this machine). A health pre-check would be a weak guard, so none.

use crate::hub_lifecycle::{self as lc, age_ticket, Delivery, HubBrief, Lifecycle, Sent};
use crate::link;
use crate::secret::{self, ApiKey};
use crate::team::parse_iso;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;
use url::{Host, Url};

pub const DEFAULT_BASE: &str = "http://127.0.0.1:8891/api/v1";
const TOKEN_FILE: &str = "hub-api-token.txt";
const NO_TOKEN: &str = "No hub token. Save it to ~/.claude/secrets/hub-api-token.txt (or set AFO_HUB_TOKEN_FILE).";
const MIN_MANUAL_GAP: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_TICKETS: usize = 200;
const MAX_APPROVALS: usize = 50;
const MAX_TIMELINE: usize = 200;
const MAX_DRAFT_FETCHES: usize = 10;
/// The hub allows about 10 requests a second: space the draft fetches well inside that.
const DRAFT_GAP: Duration = Duration::from_millis(150);
/// Sent only when something the screens show has changed (a ticket, an approval, the state).
pub const EVENT: &str = "hub-update";
/// Sent after every poll, tiny: lets the screens say "Updated 5 s ago" without a full redraw.
pub const PULSE: &str = "hub-pulse";

// ---- the guard and the token --------------------------------------------------

/// The URL only if it is http(s) on a loopback IP literal, with no credentials. `localhost` is
/// refused on purpose: a hosts-file or DNS change must not be able to redirect the token.
pub fn loopback_only(raw: &str) -> Result<Url, String> {
    let no = || "the hub address must be on this computer (127.0.0.1)".to_string();
    let u = Url::parse(raw).map_err(|_| no())?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() {
        return Err(no());
    }
    match u.host() {
        Some(Host::Ipv4(a)) if a.is_loopback() => Ok(u),
        Some(Host::Ipv6(a)) if a.is_loopback() => Ok(u),
        _ => Err(no()),
    }
}

/// `AFO_HUB_TOKEN_FILE` when set, else `~/.claude/secrets/hub-api-token.txt`.
fn token_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AFO_HUB_TOKEN_FILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    secret::home_dir().map(|h| h.join(".claude").join("secrets").join(TOKEN_FILE))
}

/// A token is one visible-ASCII word: anything else could not be a header value anyway.
pub fn parse_token(raw: &str) -> Result<ApiKey, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("The hub token file is empty.".into());
    }
    if t.len() > 512 || !t.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("The hub token file does not look like a token.".into());
    }
    Ok(ApiKey::new(t))
}

pub fn load_token_from(path: &Path) -> Result<ApiKey, String> {
    let raw = std::fs::read_to_string(path).map_err(|_| NO_TOKEN.to_string())?;
    parse_token(&raw)
}

fn load_token() -> Result<ApiKey, String> {
    load_token_from(&token_path().ok_or(NO_TOKEN)?)
}

// ---- what the webview gets ------------------------------------------------------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HubStatus {
    #[default]
    Connecting,
    Connected,
    Offline,
    TokenMissing,
    /// The hub answered but refused the token.
    Rejected,
    /// The hub is running without its API (404 on the list).
    NotEnabled,
    /// The hub said "slow down" (429).
    Busy,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Ticket {
    pub id: String,
    pub odoo_ticket_id: String,
    pub customer_name: String,
    pub owner_name: String,
    pub status: String,
    /// The hub's machine stage (awaiting_owner, in_progress, ...).
    pub stage: String,
    pub waiting_on: String,
    pub channel: String,
    pub summary: String,
    pub has_pending_approval: bool,
    pub created_ts: i64,
    pub updated_ts: i64,
    pub updated_ago_secs: Option<i64>,
    /// The live step (from the hub, or worked out from the fields above for an older hub).
    pub lifecycle: Lifecycle,
    /// True when the hub sent no usable `lifecycle` and Rust worked the step out (the strip then shows only the current step).
    pub lifecycle_derived: bool,
    pub lifecycle_at_ts: i64,
    pub lifecycle_ago_secs: Option<i64>,
    /// The instant acknowledgement reached the customer.
    pub acked: bool,
    pub approved_by: String,
    pub last_delivery: Option<Delivery>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Approval {
    pub ticket_id: String,
    pub odoo_ticket_id: String,
    pub customer_name: String,
    pub owner_name: String,
    pub fingerprint: String,
    pub draft: String,
    /// What the approval is for ("Reply to client"), and who may approve it.
    pub kind_label: String,
    pub approvers: String,
    pub expires_ts: i64,
    /// Negative once expired; None when the hub gave no expiry.
    pub expires_in_secs: Option<i64>,
    /// The exact address "Open to approve" opens (shown in the confirm). Empty = cannot be opened.
    pub open_url: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct TimelineItem {
    pub kind: String,
    pub label: String,
    pub actor: String,
    pub at_ts: i64,
    pub ago_secs: Option<i64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct TicketDetail {
    pub ticket: Ticket,
    pub pending: Option<Approval>,
    pub approver: String,
    pub timeline: Vec<TimelineItem>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub status: HubStatus,
    pub message: String,
    pub tickets: Vec<Ticket>,
    pub approvals: Vec<Approval>,
    /// Seconds since the last good answer (None = never).
    pub last_ok_ago_secs: Option<i64>,
    /// Tickets that wait for a person (the nav badge).
    pub needs_count: usize,
}

/// The small event after every poll.
#[derive(Serialize, Clone, Debug)]
pub struct Pulse {
    pub status: HubStatus,
    pub last_ok_ago_secs: Option<i64>,
}

// ---- tolerant parsing -------------------------------------------------------------

fn flex_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

fn flex_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().is_some_and(|x| x != 0),
        Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"),
        _ => false,
    })
}

/// Text from the hub, made safe to show: control characters become spaces (newlines kept when
/// `multiline`), and it is cut to `cap` characters.
pub fn clean(s: &str, cap: usize, multiline: bool) -> String {
    let mapped: String = s
        .chars()
        .map(|c| if c == '\n' && multiline { c } else if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { ' ' } else { c })
        .collect();
    let t = mapped.trim();
    let mut out: String = t.chars().take(cap).collect();
    if t.chars().count() > cap {
        out.push('…');
    }
    out
}

/// A time as Unix seconds: ISO text, or a number (seconds, or milliseconds when huge). 0 = unknown.
pub fn ts_of(v: &Value) -> i64 {
    match v {
        Value::String(s) => parse_iso(s).unwrap_or(0),
        Value::Number(n) => {
            let x = n.as_i64().unwrap_or(0);
            if x > 100_000_000_000 { x / 1000 } else { x.max(0) }
        }
        _ => 0,
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTicket {
    #[serde(deserialize_with = "flex_str")]
    id: String,
    #[serde(deserialize_with = "flex_str")]
    odoo_ticket_id: String,
    #[serde(deserialize_with = "flex_str", alias = "customer")]
    customer_name: String,
    #[serde(deserialize_with = "flex_str", alias = "owner")]
    owner_name: String,
    #[serde(deserialize_with = "flex_str")]
    status: String,
    #[serde(deserialize_with = "flex_str")]
    stage: String,
    #[serde(deserialize_with = "flex_str")]
    waiting_on: String,
    #[serde(deserialize_with = "flex_str")]
    channel: String,
    #[serde(deserialize_with = "flex_str")]
    summary: String,
    #[serde(deserialize_with = "flex_bool")]
    has_pending_approval: bool,
    #[serde(alias = "created_at")]
    created: Value,
    #[serde(alias = "updated_at")]
    updated: Value,
    lifecycle: Value,
    lifecycle_at: Value,
    #[serde(deserialize_with = "flex_bool")]
    acked: bool,
    #[serde(deserialize_with = "flex_str")]
    approved_by: String,
    last_delivery: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawApproval {
    #[serde(deserialize_with = "flex_str")]
    ticket_id: String,
    #[serde(deserialize_with = "flex_str")]
    odoo_ticket_id: String,
    #[serde(deserialize_with = "flex_str", alias = "customer")]
    customer_name: String,
    #[serde(deserialize_with = "flex_str", alias = "owner")]
    owner_name: String,
    #[serde(deserialize_with = "flex_str")]
    fingerprint: String,
    #[serde(deserialize_with = "flex_str", alias = "draft_text")]
    draft: String,
    #[serde(deserialize_with = "flex_str")]
    kind_label: String,
    /// Names of the people who may approve (the hub sends a list; a single name also works).
    approvers: Value,
    #[serde(alias = "expires_at")]
    expires: Value,
}

fn names_of(v: &Value) -> String {
    let list: Vec<String> = match v {
        Value::Array(a) => a.iter().map(|x| flex_text(Some(x))).collect(),
        other => vec![flex_text(Some(other))],
    };
    clean(&list.into_iter().filter(|n| !n.trim().is_empty()).collect::<Vec<_>>().join(", "), 160, false)
}

/// The rows of a list answer: a bare array, or an object holding one under a usual key.
fn rows(v: &Value) -> Vec<&Value> {
    let arr = match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => ["items", "tickets", "approvals", "results", "data"].iter().find_map(|k| o.get(*k).and_then(Value::as_array)),
        _ => None,
    };
    arr.map(|a| a.iter().collect()).unwrap_or_default()
}

fn to_ticket(r: RawTicket) -> Option<Ticket> {
    let id = r.id.trim().to_string();
    if !valid_id(&id) {
        return None;
    }
    let (stage, status, waiting_on) = (clean(&r.stage, 40, false), clean(&r.status, 60, false), clean(&r.waiting_on, 120, false));
    // the hub's own step when it sends one we know; otherwise worked out from the older fields
    let sent = Lifecycle::parse(&flex_text(Some(&r.lifecycle)));
    let lifecycle = sent.unwrap_or_else(|| lc::derive_lifecycle(r.has_pending_approval, &stage, &status, &waiting_on));
    let updated_ts = ts_of(&r.updated);
    let lifecycle_at_ts = Some(ts_of(&r.lifecycle_at)).filter(|t| *t > 0).unwrap_or(updated_ts);
    Some(Ticket {
        lifecycle,
        lifecycle_derived: sent.is_none(),
        lifecycle_at_ts,
        lifecycle_ago_secs: None,
        acked: r.acked,
        approved_by: clean(&r.approved_by, 120, false),
        last_delivery: lc::parse_delivery(&r.last_delivery),
        id,
        odoo_ticket_id: clean(&r.odoo_ticket_id, 40, false),
        customer_name: clean(&r.customer_name, 120, false),
        owner_name: clean(&r.owner_name, 120, false),
        status,
        stage,
        waiting_on,
        channel: clean(&r.channel, 40, false),
        summary: clean(&r.summary, 2000, true),
        has_pending_approval: r.has_pending_approval,
        created_ts: ts_of(&r.created),
        updated_ts,
        updated_ago_secs: None,
    })
}

pub fn parse_tickets(v: &Value) -> Vec<Ticket> {
    let mut out: Vec<Ticket> = rows(v).into_iter().filter_map(|r| serde_json::from_value::<RawTicket>(r.clone()).ok()).filter_map(to_ticket).collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.updated_ts)); // newest first (stable)
    out.truncate(MAX_TICKETS);
    out
}

/// The admin page for this ticket, built in Rust from the numeric id. Empty when the id is not a number.
fn open_url_for(ticket_id: &str) -> String {
    link::approval_url(ticket_id).map(|u| u.to_string()).unwrap_or_default()
}

fn to_approval(r: RawApproval) -> Option<Approval> {
    let ticket_id = r.ticket_id.trim().to_string();
    if !valid_id(&ticket_id) {
        return None;
    }
    Some(Approval {
        open_url: open_url_for(&ticket_id),
        ticket_id,
        odoo_ticket_id: clean(&r.odoo_ticket_id, 40, false),
        customer_name: clean(&r.customer_name, 120, false),
        owner_name: clean(&r.owner_name, 120, false),
        fingerprint: clean(&r.fingerprint, 128, false),
        draft: clean(&r.draft, 4000, true),
        kind_label: clean(&r.kind_label, 80, false),
        approvers: names_of(&r.approvers),
        expires_ts: ts_of(&r.expires),
        expires_in_secs: None,
    })
}

pub fn parse_approvals(v: &Value) -> Vec<Approval> {
    let mut out: Vec<Approval> = rows(v)
        .into_iter()
        .filter_map(|r| serde_json::from_value::<RawApproval>(r.clone()).ok())
        .filter_map(to_approval)
        .filter(|a| !a.fingerprint.is_empty())
        .collect();
    out.sort_by_key(|a| if a.expires_ts == 0 { i64::MAX } else { a.expires_ts }); // soonest first
    out.truncate(MAX_APPROVALS);
    out
}

pub fn parse_detail(v: &Value) -> Option<TicketDetail> {
    let outer = v;
    let v = v.get("ticket").filter(|t| t.is_object()).unwrap_or(v); // {"ticket": {...}} or the bare ticket
    let mut inner = v.clone();
    lift_lifecycle(&mut inner, outer);
    let ticket = to_ticket(serde_json::from_value::<RawTicket>(inner).ok()?)?;
    let pa = v.get("pending_approval").filter(|p| p.is_object());
    let raw = pa.and_then(|p| serde_json::from_value::<RawApproval>(p.clone()).ok());
    let approver = raw.as_ref().map(|r| names_of(&r.approvers)).unwrap_or_default();
    let pending = raw.and_then(|mut r| {
        r.ticket_id = ticket.id.clone();
        r.customer_name = ticket.customer_name.clone();
        r.owner_name = ticket.owner_name.clone();
        r.odoo_ticket_id = ticket.odoo_ticket_id.clone();
        to_approval(r)
    });
    let timeline = v.get("timeline").map(rows_of_timeline).unwrap_or_default();
    Some(TicketDetail { ticket, pending, approver, timeline })
}

/// Lifecycle fields the hub put beside the ticket (not inside it) still count.
fn lift_lifecycle(inner: &mut Value, outer: &Value) {
    let (Some(i), Some(o)) = (inner.as_object_mut(), outer.as_object()) else { return };
    for k in ["lifecycle", "lifecycle_at", "acked", "approved_by", "last_delivery"] {
        if let (false, Some(x)) = (i.contains_key(k), o.get(k)) {
            i.insert(k.to_string(), x.clone());
        }
    }
}

fn rows_of_timeline(v: &Value) -> Vec<TimelineItem> {
    let Some(a) = v.as_array() else { return Vec::new() };
    let text = |x: &Value, k: &str| clean(&flex_text(x.get(k)), 120, false);
    let mut out: Vec<TimelineItem> = a
        .iter()
        .filter(|x| x.is_object())
        .map(|x| TimelineItem { kind: text(x, "type"), label: text(x, "label"), actor: text(x, "actor"), at_ts: ts_of(x.get("at").unwrap_or(&Value::Null)), ago_secs: None })
        .collect();
    out.truncate(MAX_TIMELINE);
    out
}

pub fn flex_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// ---- the client ---------------------------------------------------------------------

pub use crate::hub_fail::{prefer_busy, retry_after_secs, Fail};

#[derive(Default)]
struct Inner {
    status: HubStatus,
    message: String,
    tickets: Vec<Ticket>,
    approvals: Vec<Approval>,
    last_ok_ts: i64,
    /// Drafts already fetched, by (ticket id, fingerprint), so each is fetched once.
    drafts: HashMap<(String, String), String>,
    last_refresh: Option<Instant>,
    /// After a 429: seconds the poll loop must wait before the next poll.
    retry_secs: u64,
    /// What the screens were last told (see `take_change`).
    last_sent: Option<Sent>,
}

pub struct Hub {
    client: Client,
    base: Url,
    inner: Mutex<Inner>,
    gate: tokio::sync::Mutex<()>,
    /// The Tickets screen or the Team view is on screen (the webview says so): poll every 15 s, else 30 s.
    in_view: AtomicBool,
    /// Wakes the poll loop early (window focus, tab opened).
    nudge: tokio::sync::Notify,
    /// Who "you" are on the Team board: approvals meant for someone else do not count as "need you".
    me: Mutex<String>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Hub {
    pub fn new(base: &str) -> Result<Hub, String> {
        let base = loopback_only(base)?;
        let client = Client::builder()
            .no_proxy() // the token must never go to a proxy
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "could not build the hub client".to_string())?;
        Ok(Hub { client, base, inner: Mutex::new(Inner::default()), gate: tokio::sync::Mutex::new(()), in_view: AtomicBool::new(true), nudge: tokio::sync::Notify::new(), me: Mutex::new(String::new()) })
    }

    pub fn set_me(&self, me: &str) {
        *self.me.lock().unwrap_or_else(|p| p.into_inner()) = me.to_string();
    }

    fn me(&self) -> String {
        self.me.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn get(&self, path: &str, token: &ApiKey) -> Result<Value, Fail> {
        let url = format!("{}/{}", self.base.as_str().trim_end_matches('/'), path.trim_start_matches('/'));
        loopback_only(&url).map_err(|_| Fail::Offline)?; // re-checked on every request
        let mut resp = self.client.get(&url).bearer_auth(token.expose()).header("Accept", "application/json").send().await.map_err(|_| Fail::Offline)?;
        let code = resp.status();
        if code == reqwest::StatusCode::UNAUTHORIZED || code == reqwest::StatusCode::FORBIDDEN {
            return Err(Fail::Rejected);
        }
        if code == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Fail::Busy(retry_after_secs(resp.headers().get("retry-after").and_then(|v| v.to_str().ok()))));
        }
        if code == reqwest::StatusCode::NOT_FOUND && (path.starts_with("tickets?") || path.starts_with("approvals")) {
            return Err(Fail::NotEnabled);
        }
        if !code.is_success() {
            return Err(Fail::Server(code.as_u16()));
        }
        if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
            return Err(Fail::Unreadable);
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| Fail::Offline)? {
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_BODY {
                return Err(Fail::Unreadable);
            }
        }
        serde_json::from_slice(&buf).map_err(|_| Fail::Unreadable)
    }

    /// The open tickets, plus the ones closed in the last two days: the hub knows only `status=open` and `all`, so the second call asks for all and keeps the closed (a failure there keeps the closed ones we had; a 429 is passed on).
    async fn fetch_tickets(&self, token: &ApiKey) -> Result<Vec<Ticket>, Fail> {
        let mut list = parse_tickets(&self.get("tickets?status=open&limit=50", token).await?);
        tokio::time::sleep(DRAFT_GAP).await;
        let closed = match self.get("tickets?status=all&limit=50", token).await {
            Ok(v) => parse_tickets(&v),
            Err(Fail::Busy(s)) => return Err(Fail::Busy(s)), // honour the hub's back-off
            // any other failure: keep the closed tickets we already had, so the section does not flicker
            Err(_) => self.lock().tickets.iter().filter(|t| t.lifecycle == Lifecycle::Closed).cloned().collect(),
        };
        lc::merge_closed(&mut list, closed, now_secs(), MAX_TICKETS);
        Ok(list)
    }

    async fn fetch_approvals(&self, token: &ApiKey) -> Result<Vec<Approval>, Fail> {
        let mut list = parse_approvals(&self.get("approvals/pending", token).await?);
        let mut fetched = 0;
        for a in list.iter_mut().filter(|a| a.draft.is_empty()) {
            let key = (a.ticket_id.clone(), a.fingerprint.clone());
            let known = self.lock().drafts.get(&key).cloned();
            if let Some(d) = known {
                a.draft = d;
                continue;
            }
            if fetched >= MAX_DRAFT_FETCHES {
                break;
            }
            if fetched > 0 {
                tokio::time::sleep(DRAFT_GAP).await;
            }
            fetched += 1;
            match self.get(&format!("tickets/{}", a.ticket_id), token).await {
                Ok(v) => {
                    // keep the draft only if it belongs to THIS approval (same fingerprint)
                    let p = parse_detail(&v).and_then(|d| d.pending).filter(|p| p.fingerprint == a.fingerprint);
                    a.draft = p.map(|p| p.draft).unwrap_or_default();
                    self.lock().drafts.insert(key, a.draft.clone());
                }
                Err(Fail::Busy(_)) => break, // the hub asked us to slow down: the next poll continues
                Err(_) => {}
            }
        }
        let live: Vec<(String, String)> = list.iter().map(|a| (a.ticket_id.clone(), a.fingerprint.clone())).collect();
        self.lock().drafts.retain(|k, _| live.contains(k)); // forget drafts that are no longer pending
        Ok(list)
    }

    /// A nudge may poll only after the minimum gap, and never inside a 429 back-off.
    fn wake_ok(&self) -> bool {
        let i = self.lock();
        lc::wake_allowed(i.last_refresh.map(|t| t.elapsed()), MIN_MANUAL_GAP, i.retry_secs)
    }

    /// Poll now with the token from the secrets folder.
    pub async fn refresh(&self) -> Snapshot {
        match load_token() {
            Ok(t) => self.refresh_with(&t).await,
            Err(m) => {
                let _g = self.gate.lock().await;
                let mut i = self.lock();
                i.status = HubStatus::TokenMissing;
                i.message = m;
                drop(i);
                self.view()
            }
        }
    }

    pub async fn refresh_with(&self, token: &ApiKey) -> Snapshot {
        let _g = self.gate.lock().await;
        let (t, a) = tokio::join!(self.fetch_tickets(token), self.fetch_approvals(token));
        let mut i = self.lock();
        i.last_refresh = Some(Instant::now());
        let mut err = None;
        match t {
            Ok(v) => i.tickets = v,
            Err(e) => err = Some(e),
        }
        match a {
            Ok(v) => i.approvals = v,
            Err(e) => err = Some(prefer_busy(err, e)),
        }
        match err {
            None => {
                i.retry_secs = 0;
                i.status = HubStatus::Connected;
                i.message.clear();
                i.last_ok_ts = now_secs();
            }
            Some(e) => {
                i.retry_secs = if let Fail::Busy(s) = e { s } else { 0 };
                i.status = e.status();
                i.message = e.message();
            }
        }
        drop(i);
        self.view()
    }

    /// For the manual Refresh button: a second press within a moment just returns what we have.
    pub async fn refresh_manual(&self) -> Snapshot {
        let recent = self.lock().last_refresh.is_some_and(|t| t.elapsed() < MIN_MANUAL_GAP);
        if recent { self.view() } else { self.refresh().await }
    }

    /// The cached state with ages worked out for now.
    pub fn view(&self) -> Snapshot {
        let me = self.me();
        self.lock().snapshot(&me, now_secs())
    }

    /// For the Team view: is the hub reachable, and what waits for you.
    pub fn brief(&self) -> HubBrief {
        let me = self.me();
        let i = self.lock();
        lc::brief(i.status, &i.tickets, &i.approvals, i.last_ok_ago(now_secs()), &me)
    }

    /// The webview says whether the Tickets screen or the Team view is on screen. Coming into view polls at once.
    pub fn set_in_view(&self, on: bool) {
        if self.in_view.swap(on, Ordering::SeqCst) != on && on {
            self.nudge.notify_one();
        }
    }

    /// Poll soon (the window gained focus). The poll loop applies the minimum gap.
    pub fn nudge(&self) {
        self.nudge.notify_one();
    }

    /// True once per change: whether what a screen shows differs from what it was last sent.
    #[cfg(test)]
    pub fn take_change(&self) -> bool {
        self.lock().mark_sent()
    }

    /// After a poll: the full snapshot only on change, plus the small pulse always. The snapshot and the
    /// "already sent" record come from one lock, so two publishers cannot leave a stale one as the newest.
    pub fn publish(&self, app: &tauri::AppHandle) -> Snapshot {
        let me = self.me();
        let (snap, changed) = {
            let mut i = self.lock();
            (i.snapshot(&me, now_secs()), i.mark_sent())
        };
        if changed {
            let _ = app.emit(EVENT, &snap);
        }
        let _ = app.emit(PULSE, &Pulse { status: snap.status, last_ok_ago_secs: snap.last_ok_ago_secs });
        snap
    }

    /// One ticket with its timeline, fetched live. The id must be a plain id (it goes into a path).
    pub async fn ticket(&self, id: &str) -> Result<TicketDetail, String> {
        if !valid_id(id) {
            return Err("that ticket id is not valid".into());
        }
        let token = load_token()?;
        let v = self.get(&format!("tickets/{id}"), &token).await.map_err(|e| e.message())?;
        let mut d = parse_detail(&v).ok_or("the hub sent a ticket I could not read")?;
        let now = now_secs();
        age_ticket(&mut d.ticket, now);
        d.timeline.iter_mut().for_each(|t| t.ago_secs = (t.at_ts > 0).then(|| (now - t.at_ts).max(0)));
        if let Some(p) = d.pending.as_mut() {
            p.expires_in_secs = (p.expires_ts > 0).then(|| p.expires_ts - now);
            // only an approval the pending list still holds may be opened
            let still_pending = self.lock().approvals.iter().any(|a| a.ticket_id == p.ticket_id && a.fingerprint == p.fingerprint);
            if !still_pending {
                p.open_url.clear();
            }
        }
        Ok(d)
    }

    /// Open the approval page for exactly this ticket and fingerprint, in the system browser.
    pub fn open_approval(&self, ticket_id: &str, fingerprint: &str) -> Result<(), String> {
        let url = self
            .lock()
            .approvals
            .iter()
            .find(|a| a.ticket_id == ticket_id && a.fingerprint == fingerprint)
            .and_then(|a| link::approval_url(&a.ticket_id))
            .ok_or("That approval is no longer waiting, or it has no page to open.")?;
        link::open_vetted(&url)
    }
}

/// Poll every 15 s while a screen that shows the hub is in view (30 s otherwise), and early when the
/// window gains focus. A nudge the gap or a back-off refuses does not move the next poll: the loop keeps
/// its deadline. Tell the webview only when something changed. The webview keeps no timer for this.
pub fn spawn_poll(hub: Arc<Hub>, app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut next = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(next) => {}
                _ = hub.nudge.notified() => {
                    if !hub.wake_ok() {
                        continue;
                    }
                }
            }
            hub.refresh().await;
            hub.publish(&app);
            next = tokio::time::Instant::now() + lc::poll_interval(hub.in_view.load(Ordering::SeqCst), hub.lock().retry_secs);
        }
    });
}

impl Inner {
    fn last_ok_ago(&self, now: i64) -> Option<i64> {
        (self.last_ok_ts > 0).then(|| (now - self.last_ok_ts).max(0))
    }

    fn snapshot(&self, me: &str, now: i64) -> Snapshot {
        let mut tickets = self.tickets.clone();
        tickets.iter_mut().for_each(|t| age_ticket(t, now));
        let mut approvals = self.approvals.clone();
        approvals.iter_mut().for_each(|a| a.expires_in_secs = (a.expires_ts > 0).then(|| a.expires_ts - now));
        let needs_count = lc::needs_you(&self.tickets, &self.approvals, me).len();
        Snapshot { status: self.status, message: self.message.clone(), tickets, approvals, last_ok_ago_secs: self.last_ok_ago(now), needs_count }
    }

    /// True when what the screens show differs from what they were last sent; records it as sent.
    fn mark_sent(&mut self) -> bool {
        let now = Sent { status: self.status, message: self.message.clone(), tickets: self.tickets.clone(), approvals: self.approvals.clone() };
        let same = self.last_sent.as_ref() == Some(&now);
        self.last_sent = Some(now);
        !same
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
