//! Tests for hub.rs (kept in their own file so hub.rs stays small).
use super::*;
use serde_json::json;

#[test]
fn only_loopback_ip_addresses_are_allowed() {
    for ok in [DEFAULT_BASE, "http://127.0.0.1:1/x", "http://[::1]:8891/api/v1", "https://127.0.0.1/api"] {
        assert!(loopback_only(ok).is_ok(), "{ok}");
    }
    let creds = format!("http://127.0.0.1{}evil.example/", '@');
    for bad in [
        "http://evil.example/api/v1",
        "http://127.0.0.1.evil.example/api/v1",
        "http://localhost:8891/api/v1",
        "http://10.0.0.5:8891/", // pii-scan: allow (test address)
        "http://0.0.0.0:8891/",
        "ftp://127.0.0.1/",
        "file:///C:/x",
        "127.0.0.1:8891",
        creds.as_str(),
    ] {
        assert!(loopback_only(bad).is_err(), "{bad}");
    }
    assert!(Hub::new("http://example.com/api").is_err());
}

#[test]
fn token_is_trimmed_and_checked_and_never_printed() {
    let t = parse_token("  abc.def-123 \r\n").unwrap();
    assert_eq!(t.expose(), "abc.def-123");
    assert!(!format!("{t:?}{t}").contains("abc.def"));
    for bad in ["", "   \n", "two words", "tab\tinside", "bad\u{1}char", "caf\u{e9}"] {
        assert!(parse_token(bad).is_err(), "{bad:?}");
    }
    assert!(parse_token(&"x".repeat(513)).is_err());
    let e = parse_token("has space and sk_leak_me").unwrap_err();
    assert!(!e.contains("sk_leak_me"));
}

#[test]
fn token_file_is_read_and_missing_is_a_plain_error() {
    let dir = std::env::temp_dir().join(format!("one-hub-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(TOKEN_FILE);
    std::fs::write(&f, "  fake-token-for-test\n").unwrap();
    assert_eq!(load_token_from(&f).unwrap().expose(), "fake-token-for-test");
    std::fs::write(&f, "\n").unwrap();
    assert!(load_token_from(&f).is_err());
    assert!(load_token_from(&dir.join("nope.txt")).unwrap_err().contains("api-token"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tickets_parse_tolerantly() {
    let v = json!({"items": [
        {"id": 7, "odoo_ticket_id": 4521, "customer_name": "شركة النور", "owner_name": "Jordan", "status": "open",
         "waiting_on": null, "has_pending_approval": "true", "channel": "whatsapp", "summary": "Line1\nLine2\u{7}",
         "created": "2026-11-15T10:00:00Z", "updated": "2026-11-15T12:00:00+00:00", "brand_new_field": {"x": 1}},
        {"id": "a-2", "updated": 1_794_800_000},
        {"id": "../etc", "summary": "bad id"},
        {"summary": "no id"},
        "garbage",
        {"id": "c3", "customer_name": {"nested": true}, "has_pending_approval": 3}
    ]});
    let t = parse_tickets(&v);
    assert_eq!(t.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["a-2", "7", "c3"]); // newest first, bad rows dropped
    let first = t.iter().find(|x| x.id == "7").unwrap();
    assert_eq!(first.odoo_ticket_id, "4521");
    assert_eq!(first.customer_name, "شركة النور");
    assert!(first.has_pending_approval);
    assert_eq!(first.summary, "Line1\nLine2");
    assert_eq!(first.created_ts, 1_794_736_800);
    assert!(t.iter().find(|x| x.id == "c3").unwrap().has_pending_approval);
    assert_eq!(t.iter().find(|x| x.id == "c3").unwrap().customer_name, "");
    assert_eq!(parse_tickets(&json!([{"id": "z"}])).len(), 1); // a bare array works
    assert!(parse_tickets(&json!({"error": "nope"})).is_empty());
    assert!(parse_tickets(&json!(null)).is_empty());
}

#[test]
fn approvals_parse_and_sort_and_ignore_the_hubs_address() {
    let v = json!([
        {"ticket_id": 1, "customer_name": "A", "fingerprint": "f1", "expires": "2026-11-16T12:00:00Z", "approve_url": "/admin/approve/1"},
        {"ticket_id": "2", "fingerprint": "f2", "expires": "2026-11-16T10:00:00Z", "approve_url": "https://evil.example/a/2"},
        {"ticket_id": "x3", "fingerprint": "f3"},
        {"ticket_id": "4", "fingerprint": ""},
        {"fingerprint": "f5"}
    ]);
    let a = parse_approvals(&v);
    assert_eq!(a.iter().map(|x| x.fingerprint.as_str()).collect::<Vec<_>>(), ["f2", "f1", "f3"]);
    // the address is always the admin page for the numeric id, whatever the hub sent
    assert_eq!(a[0].open_url, format!("https://{}/tickets/2", crate::link::approval_host()));
    assert_eq!(a[1].open_url, format!("https://{}/tickets/1", crate::link::approval_host()));
    assert_eq!(a[2].open_url, ""); // not a numeric id: cannot be opened
    let json = serde_json::to_string(&a).unwrap();
    assert!(!json.contains("evil.example") && !json.contains("approve_url"));
}

#[test]
fn detail_parses_pending_and_timeline() {
    let v = json!({"id": "t9", "customer_name": "Riley", "status": "open",
        "pending_approval": {"draft": "Hello\nthere", "fingerprint": "abc123", "expires": "2026-11-16T10:00:00Z", "approvers": ["Alex"]},
        "timeline": [{"type": "created", "actor": "hub", "at": "2026-11-15T10:00:00Z"}, {"type": 5, "at": null}, "junk"]});
    let d = parse_detail(&v).unwrap();
    assert_eq!(d.ticket.id, "t9");
    let p = d.pending.unwrap();
    assert_eq!((p.draft.as_str(), p.fingerprint.as_str(), p.customer_name.as_str()), ("Hello\nthere", "abc123", "Riley"));
    assert_eq!(d.approver, "Alex");
    assert_eq!(d.timeline.len(), 2);
    assert_eq!(d.timeline[1].kind, "5");
    assert!(parse_detail(&json!({"pending_approval": null})).is_none());
    assert!(parse_detail(&json!({"id": "t1", "pending_approval": null})).unwrap().pending.is_none());
}

#[test]
fn hub_pr81_shapes_parse() {
    let list = json!({"tickets": [{"id": 12, "odoo_ticket_id": 12, "created_at": "2026-11-15T10:00:00Z", "updated_at": "2026-11-15T12:00:00Z",
        "customer": "Nile Co", "owner": null, "stage": "awaiting_owner", "status": "Waiting for owner", "waiting_on": "approval",
        "has_pending_approval": true, "channel": "whatsapp", "summary": "Invoice"}], "count": 1, "status": "open", "limit": 50});
    let t = &parse_tickets(&list)[0];
    assert_eq!((t.id.as_str(), t.customer_name.as_str(), t.owner_name.as_str(), t.stage.as_str()), ("12", "Nile Co", "", "awaiting_owner"));
    assert_eq!((t.waiting_on.as_str(), t.updated_ts, t.created_ts), ("approval", 1_794_744_000, 1_794_736_800));
    let appr = json!({"approvals": [{"ticket_id": 12, "kind": "reply", "kind_label": "Reply to client", "client_facing": true, "fingerprint": "ab12",
        "issued_at": "2026-11-15T10:00:00Z", "expires_at": "2026-11-16T10:00:00Z", "approvers": ["Alex", "Riley"], "approve_url": "http://127.0.0.1:8891/admin/approve/12",
        "customer": "Nile Co", "owner": "Jordan", "summary": "x"}], "count": 1});
    let a = &parse_approvals(&appr)[0];
    assert_eq!((a.customer_name.as_str(), a.approvers.as_str(), a.kind_label.as_str(), a.draft.as_str()), ("Nile Co", "Alex, Riley", "Reply to client", ""));
    assert_eq!(a.open_url, format!("https://{}/tickets/12", crate::link::approval_host()));
    let det = json!({"ticket": {"id": 12, "customer": "Nile Co", "pending_approval": {"ticket_id": 12, "fingerprint": "ab12", "expires_at": "2026-11-16T10:00:00Z",
        "approvers": ["Alex"], "draft_text": "Dear Nile", "approve_url": "/x"}, "timeline": [{"type": "created", "actor": "hub", "at": null, "label": "Ticket created"}]}});
    let d = parse_detail(&det).unwrap();
    assert_eq!((d.pending.as_ref().unwrap().draft.as_str(), d.approver.as_str(), d.timeline[0].label.as_str()), ("Dear Nile", "Alex", "Ticket created"));
}

#[test]
fn clean_strips_controls_and_bidi_marks_and_caps() {
    assert_eq!(clean("a\u{202e}b\u{0}c\td", 50, false), "a b c d");
    assert_eq!(clean("x\ny", 50, false), "x y");
    assert_eq!(clean("x\ny", 50, true), "x\ny");
    assert_eq!(clean("abcdef", 3, false), "abc…");
}

/// A tiny loopback server: answers by path, and records every request head.
async fn serve(routes: Vec<(&'static str, u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let line = head.lines().next().unwrap_or("").to_string();
            log.lock().unwrap().push(head);
            let (code, body) = routes.iter().find(|(p, _, _)| line.contains(p)).map(|(_, c, b)| (*c, b.clone())).unwrap_or((404, "{}".into()));
            let resp = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = s.write_all(resp.as_bytes()).await;
        }
    });
    (format!("http://{addr}/api/v1"), seen)
}

#[tokio::test]
async fn refresh_reads_tickets_and_approvals_and_fills_the_draft() {
    let tickets = json!([{"id": "1", "customer_name": "Riley", "updated": 1_763_200_000}]).to_string();
    let approvals = json!([{"ticket_id": "1", "fingerprint": "fp1", "expires": 4_000_000_000_i64, "approve_url": "https://admin.example.com/tickets/1"}]).to_string();
    let detail = json!({"id": "1", "pending_approval": {"draft": "Hi Riley", "fingerprint": "fp1"}}).to_string();
    let (base, seen) = serve(vec![("tickets?status=open", 200, tickets), ("approvals/pending", 200, approvals), ("tickets/1 ", 200, detail)]).await;
    let hub = Hub::new(&base).unwrap();
    let token = ApiKey::new("fake-token-for-test");
    let s = hub.refresh_with(&token).await;
    assert_eq!(s.status, HubStatus::Connected, "{}", s.message);
    assert_eq!(s.tickets.len(), 1);
    assert_eq!((s.approvals[0].draft.as_str(), s.approvals[0].open_url.is_empty()), ("Hi Riley", false));
    assert!(s.approvals[0].expires_in_secs.unwrap() > 0);
    assert!(seen.lock().unwrap().iter().all(|h| h.contains("authorization: Bearer fake-token-for-test") || h.contains("Authorization: Bearer fake-token-for-test")));
    let before = seen.lock().unwrap().len();
    hub.refresh_with(&token).await; // the draft is cached: no extra detail request
    assert_eq!(seen.lock().unwrap().len(), before + 3, "open list + recently closed + approvals, and no detail request");
}

#[tokio::test]
async fn failures_keep_the_last_good_data_and_name_the_state() {
    let ok = json!([{"id": "t1"}]).to_string();
    let (base, _) = serve(vec![("tickets?status=open", 200, ok), ("approvals/pending", 200, "[]".into())]).await;
    let hub = Hub::new(&base).unwrap();
    let token = ApiKey::new("fake-token-for-test");
    assert_eq!(hub.refresh_with(&token).await.status, HubStatus::Connected);
    let (dead_base, _) = serve(vec![("tickets", 401, "{}".into()), ("approvals", 401, "{}".into())]).await;
    let rejected = Hub::new(&dead_base).unwrap().refresh_with(&token).await;
    assert_eq!(rejected.status, HubStatus::Rejected);
    let (nf, _) = serve(vec![("tickets", 404, r#"{"error":"not_found"}"#.into()), ("approvals", 404, "{}".into())]).await;
    assert_eq!(Hub::new(&nf).unwrap().refresh_with(&token).await.status, HubStatus::NotEnabled);
    let (busy, _) = serve(vec![("tickets", 429, "{}".into()), ("approvals", 200, "[]".into())]).await;
    assert_eq!(Hub::new(&busy).unwrap().refresh_with(&token).await.status, HubStatus::Busy);
    let (e500, _) = serve(vec![("tickets", 500, "{}".into()), ("approvals", 200, "[]".into())]).await;
    assert_eq!(Hub::new(&e500).unwrap().refresh_with(&token).await.status, HubStatus::Offline);
    // nothing listening: offline, and the earlier data stays
    let gone = Hub::new("http://127.0.0.1:1/api/v1").unwrap();
    let s = gone.refresh_with(&token).await;
    assert_eq!((s.status, s.message.as_str()), (HubStatus::Offline, "Offline: hub not reachable"));
    gone.lock().tickets = vec![Ticket { id: "keep".into(), ..Default::default() }];
    assert_eq!(gone.refresh_with(&token).await.tickets[0].id, "keep");
}

#[tokio::test]
async fn ticket_id_must_be_plain() {
    let hub = Hub::new(DEFAULT_BASE).unwrap();
    for bad in ["", "../x", "a/b", "a?b=c", "a b"] {
        assert!(hub.ticket(bad).await.is_err(), "{bad}");
    }
}

#[test]
fn open_needs_a_waiting_approval() {
    let hub = Hub::new(DEFAULT_BASE).unwrap();
    assert!(hub.open_approval("t1", "fp").unwrap_err().contains("no longer waiting"));
}

/// One canned raw answer for every request, plus the request heads it saw.
async fn serve_raw(response: String) -> (u16, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap_or(0);
            log.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = s.write_all(response.as_bytes()).await;
        }
    });
    (port, seen)
}

#[tokio::test]
async fn a_redirect_to_another_host_never_gets_the_token() {
    let (other_port, other_seen) = serve_raw("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]".into()).await;
    let redirect = format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{other_port}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let (port, hub_seen) = serve_raw(redirect).await;
    let hub = Hub::new(&format!("http://127.0.0.1:{port}/api/v1")).unwrap();
    let s = hub.refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.status, HubStatus::Offline);
    assert!(!hub_seen.lock().unwrap().is_empty());
    assert!(other_seen.lock().unwrap().is_empty(), "the redirect target must not be contacted");
}

#[tokio::test]
async fn proxy_environment_variables_are_ignored() {
    let (proxy_port, proxy_seen) = serve_raw("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()).await;
    for k in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(k, format!("http://127.0.0.1:{proxy_port}"));
    }
    let (base, seen) = serve(vec![("tickets?status=open", 200, "[]".into()), ("approvals/pending", 200, "[]".into())]).await;
    let s = Hub::new(&base).unwrap().refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.status, HubStatus::Connected, "{}", s.message);
    assert!(!seen.lock().unwrap().is_empty());
    assert!(proxy_seen.lock().unwrap().is_empty(), "nothing may go through a proxy");
}

#[test]
fn retry_after_is_parsed_and_never_shorter_than_one_poll() {
    assert_eq!(retry_after_secs(None), 30);
    assert_eq!(retry_after_secs(Some("5")), 30);
    assert_eq!(retry_after_secs(Some(" 90 ")), 90);
    assert_eq!(retry_after_secs(Some("100000")), 600);
    assert_eq!(retry_after_secs(Some("Wed, 21 Oct 2026 07:28:00 GMT")), 30); // a date form is ignored
    assert!(Fail::Busy(90).message().contains("90 seconds"));
}

#[tokio::test]
async fn busy_keeps_the_wait_and_stops_the_draft_loop() {
    let list = json!([{"ticket_id": 1, "fingerprint": "a"}, {"ticket_id": 2, "fingerprint": "b"}]).to_string();
    let (base, seen) = serve(vec![("tickets?status=open", 200, "[]".into()), ("approvals/pending", 200, list), ("tickets/", 429, "{}".into())]).await;
    let hub = Hub::new(&base).unwrap();
    hub.refresh_with(&ApiKey::new("fake-token-for-test")).await;
    let detail_calls = seen.lock().unwrap().iter().filter(|h| h.contains("GET /api/v1/tickets/")).count();
    assert_eq!(detail_calls, 1, "after a 429 the loop must stop");
}

#[tokio::test]
async fn a_draft_with_another_fingerprint_is_not_kept() {
    let list = json!([{"ticket_id": 1, "fingerprint": "new"}]).to_string();
    let detail = json!({"ticket": {"id": 1, "pending_approval": {"fingerprint": "old", "draft_text": "stale text"}}}).to_string();
    let (base, _) = serve(vec![("tickets?status=open", 200, "[]".into()), ("approvals/pending", 200, list), ("tickets/1 ", 200, detail)]).await;
    let s = Hub::new(&base).unwrap().refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.approvals[0].draft, "");
}

#[tokio::test]
async fn an_approvals_404_means_the_api_is_off() {
    let (base, _) = serve(vec![("tickets?status=open", 200, "[]".into()), ("approvals/pending", 404, "{}".into())]).await;
    assert_eq!(Hub::new(&base).unwrap().refresh_with(&ApiKey::new("fake-token-for-test")).await.status, HubStatus::NotEnabled);
}

#[test]
fn the_real_admin_url_in_the_hubs_data_changes_nothing() {
    let v = json!({"approvals": [{"ticket_id": 12, "fingerprint": "f", "approve_url": "https://admin.example.com/tickets/12"}]});
    assert_eq!(parse_approvals(&v)[0].open_url, format!("https://{}/tickets/12", crate::link::approval_host()));
}

// ---- the live lifecycle -----------------------------------------------------------------

use crate::hub_lifecycle::{self as lc, Lifecycle};

fn one(v: serde_json::Value) -> Ticket {
    parse_tickets(&json!([v])).remove(0)
}

#[test]
fn the_hubs_lifecycle_is_read_and_unknown_words_fall_back() {
    for (word, want) in [
        ("needs_approval", Lifecycle::NeedsApproval), ("Reply-Sent", Lifecycle::ReplySent), ("reply_delivered", Lifecycle::ReplyDelivered),
        ("reply_read", Lifecycle::ReplyRead), ("awaiting_customer", Lifecycle::AwaitingCustomer), ("closed", Lifecycle::Closed),
        ("held", Lifecycle::Held), ("approved", Lifecycle::Approved), ("new", Lifecycle::New),
    ] {
        assert_eq!(one(json!({"id": 1, "lifecycle": word})).lifecycle, want, "{word}");
    }
    // a word we do not know is ignored and the older fields decide
    assert_eq!(one(json!({"id": 1, "lifecycle": "teleported", "has_pending_approval": true})).lifecycle, Lifecycle::NeedsApproval);
}

#[test]
fn an_older_hub_without_lifecycle_is_derived_from_the_old_fields() {
    let d = |v| one(v).lifecycle;
    assert_eq!(d(json!({"id": 1, "has_pending_approval": true, "stage": "closed"})), Lifecycle::NeedsApproval);
    assert_eq!(d(json!({"id": 2, "stage": "awaiting_customer"})), Lifecycle::AwaitingCustomer);
    assert_eq!(d(json!({"id": 3, "waiting_on": "Customer"})), Lifecycle::AwaitingCustomer);
    assert_eq!(d(json!({"id": 4, "waiting_on": "العميل"})), Lifecycle::AwaitingCustomer);
    for closed in ["closed", "Cancelled", "solved"] {
        assert_eq!(d(json!({"id": 5, "status": closed, "stage": "in_progress"})), Lifecycle::Closed, "{closed}");
        assert_eq!(d(json!({"id": 6, "stage": closed, "status": "open"})), Lifecycle::Closed, "{closed}");
    }
    assert_eq!(d(json!({"id": 7, "stage": "on_hold"})), Lifecycle::Held);
    assert_eq!(d(json!({"id": 8, "stage": "awaiting_owner", "status": "open", "waiting_on": "Jordan"})), Lifecycle::NeedsApproval, "waiting on the owner is never New");
    assert_eq!(d(json!({"id": 9})), Lifecycle::New);
}

#[test]
fn lifecycle_time_ack_approver_and_delivery_are_tolerant() {
    let t = one(json!({"id": 1, "updated": "2026-11-15T10:00:00Z", "lifecycle": "reply_delivered", "lifecycle_at": "2026-11-15T12:00:00Z",
        "acked": "true", "approved_by": "Maya", "last_delivery": {"kind": "reply", "status": "delivered", "at": "2026-11-15T12:00:00Z"}}));
    assert_eq!(t.lifecycle_at_ts, ts_of(&json!("2026-11-15T12:00:00Z")));
    assert!(t.acked);
    assert_eq!(t.approved_by, "Maya");
    let d = t.last_delivery.unwrap();
    assert_eq!((d.kind.as_str(), d.status.as_str()), ("reply", "delivered"));
    // no lifecycle_at: the last update time stands in; nothing at all is fine too
    let u = one(json!({"id": 2, "updated": "2026-11-15T10:00:00Z", "last_delivery": {"kind": "sms", "status": "sent"}}));
    assert_eq!(u.lifecycle_at_ts, u.updated_ts);
    assert!(u.last_delivery.is_none() && !u.acked && u.approved_by.is_empty());
    assert!(one(json!({"id": 3, "last_delivery": "sent"})).last_delivery.is_none());
}

#[test]
fn lifecycle_fields_next_to_the_ticket_are_found_in_a_detail_answer() {
    let v = json!({"ticket": {"id": 5, "customer_name": "X"}, "lifecycle": "approved", "approved_by": "Riley", "acked": true});
    let t = parse_detail(&v).unwrap().ticket;
    assert_eq!((t.lifecycle, t.approved_by.as_str(), t.acked), (Lifecycle::Approved, "Riley", true));
    let bare = parse_detail(&json!({"id": 5, "lifecycle": "closed"})).unwrap().ticket;
    assert_eq!(bare.lifecycle, Lifecycle::Closed);
}

#[test]
fn closed_tickets_stay_for_two_days_then_go() {
    let now = 2_000_000_000;
    let mk = |id: &str, at: i64| Ticket { id: id.into(), lifecycle: Lifecycle::Closed, lifecycle_at_ts: at, updated_ts: at, ..Default::default() };
    let open = Ticket { id: "o".into(), updated_ts: now - 10, ..Default::default() };
    let mut list = vec![open.clone(), mk("fresh", now - 3600)];
    lc::merge_closed(&mut list, vec![mk("fresh", now - 1), mk("old", now - lc::CLOSED_KEEP_SECS - 1), mk("nodate", 0), open.clone()], now, 50);
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"fresh") && ids.contains(&"o") && ids.contains(&"nodate") && !ids.contains(&"old"), "{ids:?}");
    assert_eq!(ids.iter().filter(|i| **i == "fresh").count(), 1, "an id already listed is not added twice");
    assert_eq!(list.iter().find(|t| t.id == "fresh").unwrap().lifecycle_at_ts, now - 3600, "the list's own copy wins");
}

#[tokio::test]
async fn a_recently_closed_ticket_is_fetched_and_shown_as_closed() {
    let now = now_secs();
    let open = json!([{"id": "1", "lifecycle": "needs_approval", "updated": now}]).to_string();
    let closed = json!([{"id": "2", "lifecycle": "closed", "updated": now - 600}, {"id": "3", "lifecycle": "closed", "updated": now - 30 * 86400}]).to_string();
    let (base, _) = serve(vec![("tickets?status=open", 200, open), ("tickets?status=all", 200, closed), ("approvals/pending", 200, "[]".into())]).await;
    let s = Hub::new(&base).unwrap().refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.status, HubStatus::Connected, "{}", s.message);
    let got: Vec<(&str, Lifecycle)> = s.tickets.iter().map(|t| (t.id.as_str(), t.lifecycle)).collect();
    assert_eq!(got, [("1", Lifecycle::NeedsApproval), ("2", Lifecycle::Closed)]);
    assert_eq!(s.needs_count, 1, "a ticket whose step is needs_approval counts even before the approvals list shows it");
}

// ---- change detection ---------------------------------------------------------------------

#[test]
fn the_screens_are_told_only_when_something_changed() {
    let hub = Hub::new(DEFAULT_BASE).unwrap();
    assert!(hub.take_change(), "the first state is always sent");
    assert!(!hub.take_change(), "the same state is not sent again");
    {
        // time passing alone (ages, last-good time) is not a change
        let mut i = hub.lock();
        i.last_ok_ts += 5;
        i.last_refresh = Some(Instant::now());
    }
    assert!(!hub.take_change());
    hub.lock().tickets.push(Ticket { id: "9".into(), ..Default::default() });
    assert!(hub.take_change(), "a new ticket");
    assert!(!hub.take_change());
    hub.lock().tickets[0].lifecycle = Lifecycle::Approved;
    assert!(hub.take_change(), "a ticket moved one step");
    assert!(!hub.take_change());
    hub.lock().status = HubStatus::Offline;
    assert!(hub.take_change(), "the connection state");
    assert!(!hub.take_change());
    hub.lock().approvals.push(Approval { ticket_id: "9".into(), fingerprint: "f".into(), ..Default::default() });
    assert!(hub.take_change(), "an approval");
}

// ---- when to poll -----------------------------------------------------------------------

#[test]
fn polls_every_15_seconds_in_view_30_hidden_and_obeys_a_429() {
    use std::time::Duration as D;
    assert_eq!(lc::poll_interval(true, 0), D::from_secs(15));
    assert_eq!(lc::poll_interval(false, 0), D::from_secs(30));
    assert_eq!(lc::poll_interval(true, 90), D::from_secs(90), "Retry-After beats the fast poll");
    assert_eq!(lc::poll_interval(false, 10), D::from_secs(30));
}

#[test]
fn a_focus_nudge_polls_at_once_but_never_inside_the_gap_or_a_backoff() {
    use std::time::Duration as D;
    let gap = D::from_secs(2);
    assert!(lc::wake_allowed(None, gap, 0), "never polled yet");
    assert!(!lc::wake_allowed(Some(D::from_millis(500)), gap, 0), "inside the minimum gap");
    assert!(lc::wake_allowed(Some(D::from_secs(3)), gap, 0));
    assert!(!lc::wake_allowed(Some(D::from_secs(10)), gap, 30), "the hub said wait 30 s");
    assert!(lc::wake_allowed(Some(D::from_secs(31)), gap, 30));
}

#[tokio::test]
async fn coming_into_view_wakes_the_poll_loop_once() {
    let hub = Hub::new(DEFAULT_BASE).unwrap();
    assert!(hub.in_view.load(Ordering::SeqCst), "the Team view is home, so a start is in view");
    hub.set_in_view(false);
    assert!(!hub.in_view.load(Ordering::SeqCst));
    hub.set_in_view(true);
    let woke = tokio::time::timeout(Duration::from_millis(200), hub.nudge.notified()).await;
    assert!(woke.is_ok(), "a stored wake-up is waiting");
    hub.set_in_view(true); // already in view: no new wake-up
    assert!(tokio::time::timeout(Duration::from_millis(50), hub.nudge.notified()).await.is_err());
}

// ---- the Team view ------------------------------------------------------------------------

fn pending(id: &str, summary: &str) -> (Ticket, Approval) {
    (
        Ticket { id: id.into(), odoo_ticket_id: "2714".into(), customer_name: "Nour".into(), summary: summary.into(), lifecycle: Lifecycle::NeedsApproval, ..Default::default() },
        Approval { ticket_id: id.into(), odoo_ticket_id: "2714".into(), customer_name: "Nour".into(), fingerprint: "f".into(), kind_label: "Reply to client".into(), ..Default::default() },
    )
}

#[test]
fn team_is_caught_up_only_when_both_lists_are_empty_and_the_hub_answered() {
    let (t, a) = pending("1", "Invoice question\nmore");
    let brief = |tickets: &[Ticket], approvals: &[Approval], status| lc::brief(status, tickets, approvals, Some(3), "");
    assert!(lc::team_caught_up(0, &brief(&[], &[], HubStatus::Connected)));
    assert!(!lc::team_caught_up(1, &brief(&[], &[], HubStatus::Connected)), "a team ask on you");
    assert!(!lc::team_caught_up(0, &brief(&[t.clone()], &[a], HubStatus::Connected)), "a hub approval waits");
    assert!(!lc::team_caught_up(0, &brief(&[t], &[], HubStatus::Connected)), "a ticket at needs_approval alone is enough");
    for down in [HubStatus::Offline, HubStatus::TokenMissing, HubStatus::Rejected, HubStatus::NotEnabled, HubStatus::Connecting, HubStatus::Busy] {
        assert!(!lc::team_caught_up(0, &brief(&[], &[], down)), "{down:?}: an unreachable hub proves nothing");
    }
}

#[test]
fn the_hub_brief_lists_who_needs_you_with_one_line_summaries() {
    let (t1, a1) = pending("1", "Invoice question\nsecond line");
    let (t2, _) = pending("2", "Refund request");
    let b = lc::brief(HubStatus::Connected, &[t1, t2], &[a1], None, "");
    assert_eq!(b.needs_count, 2, "the approval and the ticket at needs_approval, no duplicates");
    assert_eq!((b.needs[0].ticket_id.as_str(), b.needs[0].odoo_ticket_id.as_str(), b.needs[0].summary.as_str()), ("1", "2714", "Invoice question"));
    assert_eq!(b.needs[1].summary, "Refund request");
    // an approval whose ticket is not in the list falls back to what the approval is for
    let (_, orphan) = pending("77", "x");
    assert_eq!(lc::needs_you(&[], &[orphan], "")[0].summary, "Reply to client");
    // the row list is capped, the count is not
    let many: Vec<Ticket> = (0..20).map(|n| pending(&n.to_string(), "s").0).collect();
    let b = lc::brief(HubStatus::Connected, &many, &[], None, "");
    assert_eq!((b.needs.len(), b.needs_count), (8, 20));
}

// ---- review fixes: real hub vocabulary, approvers, back-off ----------------------------------

#[test]
fn the_real_hub_stage_words_are_never_new_when_a_person_has_to_act() {
    let d = |v| one(v).lifecycle;
    for stage in ["awaiting_owner", "awaiting_owner_advance", "awaiting_owner_resolve", "Awaiting-Owner"] {
        assert_eq!(d(json!({"id": 1, "stage": stage})), Lifecycle::NeedsApproval, "{stage}");
    }
    assert_eq!(d(json!({"id": 2, "status": "Waiting for owner"})), Lifecycle::NeedsApproval);
    assert_eq!(d(json!({"id": 3, "waiting_on": "owner"})), Lifecycle::NeedsApproval);
    assert_eq!(d(json!({"id": 4, "status": "resolved"})), Lifecycle::Closed);
    assert_eq!(d(json!({"id": 5, "stage": "in_progress"})), Lifecycle::New);
    assert!(one(json!({"id": 5, "stage": "in_progress"})).lifecycle_derived, "the strip must not claim earlier steps");
    assert!(!one(json!({"id": 6, "lifecycle": "approved"})).lifecycle_derived);
    // free text that only mentions the customer is not "awaiting customer"
    assert_eq!(d(json!({"id": 7, "waiting_on": "client success"})), Lifecycle::New);
    assert_eq!(d(json!({"id": 8, "waiting_on": "client approval"})), Lifecycle::New);
    assert_eq!(d(json!({"id": 9, "waiting_on": "Client"})), Lifecycle::AwaitingCustomer);
    // and a ticket waiting on the owner is counted as needing a person
    let t = one(json!({"id": 10, "stage": "awaiting_owner_resolve", "customer_name": "Nour"}));
    assert_eq!(lc::needs_you(&[t], &[], "Alex").len(), 1);
}

#[test]
fn approvals_meant_for_somebody_else_do_not_need_you() {
    let (t, mut a) = pending("1", "Invoice");
    a.approvers = "Riley, Sam".into();
    assert_eq!(lc::needs_you(&[t.clone()], &[a.clone()], "Alex").len(), 0, "not an approver: not yours, and the ticket is not double counted");
    assert_eq!(lc::needs_you(&[t.clone()], &[a.clone()], " riley ").len(), 1, "matched by name, any case");
    assert_eq!(lc::needs_you(&[t.clone()], &[a.clone()], "").len(), 1, "no known user: everything counts");
    a.approvers = String::new();
    assert_eq!(lc::needs_you(&[t], &[a], "Alex").len(), 1, "no approver list: anybody may approve");
}

#[test]
fn a_429_wins_when_two_calls_fail_and_a_failed_all_call_keeps_the_closed_tickets() {
    assert_eq!(prefer_busy(Some(Fail::Offline), Fail::Busy(40)), Fail::Busy(40));
    assert_eq!(prefer_busy(Some(Fail::Busy(40)), Fail::Offline), Fail::Busy(40));
    assert_eq!(prefer_busy(Some(Fail::Rejected), Fail::Offline), Fail::Rejected);
    assert_eq!(prefer_busy(None, Fail::Offline), Fail::Offline);
}

#[tokio::test]
async fn a_busy_all_call_passes_the_back_off_on() {
    let open = json!([{"id": "1"}]).to_string();
    let (base, _) = serve(vec![("tickets?status=open", 200, open), ("tickets?status=all", 429, "{}".into()), ("approvals/pending", 200, "[]".into())]).await;
    let s = Hub::new(&base).unwrap().refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.status, HubStatus::Busy);
}

#[tokio::test]
async fn a_failed_all_call_does_not_drop_the_closed_tickets_we_had() {
    let now = now_secs();
    let open = json!([{"id": "1"}]).to_string();
    let (base, _) = serve(vec![("tickets?status=open", 200, open), ("tickets?status=all", 500, "{}".into()), ("approvals/pending", 200, "[]".into())]).await;
    let hub = Hub::new(&base).unwrap();
    hub.lock().tickets.push(Ticket { id: "c".into(), lifecycle: Lifecycle::Closed, lifecycle_at_ts: now - 60, updated_ts: now - 60, ..Default::default() });
    let s = hub.refresh_with(&ApiKey::new("fake-token-for-test")).await;
    assert_eq!(s.status, HubStatus::Connected, "{}", s.message);
    assert!(s.tickets.iter().any(|t| t.id == "c"), "the closed ticket stays");
}

#[test]
fn the_snapshot_and_the_sent_record_come_from_one_state() {
    let hub = Hub::new(DEFAULT_BASE).unwrap();
    hub.set_me("Alex");
    {
        let mut i = hub.lock();
        i.tickets.push(Ticket { id: "1".into(), lifecycle: Lifecycle::NeedsApproval, ..Default::default() });
    }
    let snap = hub.view();
    assert_eq!(snap.needs_count, 1);
    assert!(hub.take_change() && !hub.take_change());
    assert_eq!(hub.brief().needs_count, 1);
}
