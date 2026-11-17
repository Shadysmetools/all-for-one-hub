//! Open a link from a reply in the system browser. Only `http://` and `https://` addresses are
//! accepted, parsed with the `url` crate; the address goes to the OS URL handler as ONE argument
//! (no shell is involved). Loopback, private and link-local hosts are refused (a link must not be
//! able to poke a local service), credentials in the address are refused, and a long query or
//! fragment is refused (it could carry data out).
//! The tauri opener plugin is not in the offline crate cache, so this is the small equivalent.

use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

const MAX_URL: usize = 2000;
/// Longest query plus fragment: enough for a normal link, too small to smuggle a document.
const MAX_QUERY: usize = 200;

fn private_v4(a: Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.octets()[0] == 0
}

fn private_v6(a: Ipv6Addr) -> bool {
    let seg = a.segments();
    a.is_loopback() || a.is_unspecified() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80 || a.to_ipv4_mapped().is_some_and(private_v4)
}

/// The normalised address (punycode host, escaped path) if it is a safe public http(s) link.
pub fn checked(url: &str) -> Result<String, String> {
    let u = url.trim();
    let no = || "that link cannot be opened".to_string();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err("only http and https links can be opened".into());
    }
    let bad = |c: char| c.is_control() || c.is_whitespace() || matches!(c, '"' | '<' | '>' | '^' | '|' | '`' | '\\' | '{' | '}');
    if u.len() > MAX_URL || u.chars().any(bad) {
        return Err(no());
    }
    let parsed = Url::parse(u).map_err(|_| no())?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(no());
    }
    match parsed.host() {
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase(); // "localhost." is localhost
            if d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") || d.ends_with(".internal") || !d.contains('.') {
                return Err(no());
            }
        }
        Some(Host::Ipv4(a)) if !private_v4(a) => {}
        Some(Host::Ipv6(a)) if !private_v6(a) => {}
        _ => return Err(no()),
    }
    if parsed.query().map_or(0, str::len) + parsed.fragment().map_or(0, str::len) > MAX_QUERY {
        return Err("that link is too long to open".into());
    }
    Ok(parsed.as_str().to_string())
}

pub fn open(url: &str) -> Result<(), String> {
    let u = checked(url)?;
    spawn_browser(&u)
}

/// The admin host used when `AFO_ADMIN_HOST` is not set (or is not a bare hostname).
pub const DEFAULT_APPROVAL_HOST: &str = "admin.example.com";

/// The only host "Open to approve" may open: the hub's admin site. Read once from the
/// `AFO_ADMIN_HOST` environment variable (a bare hostname: no scheme, port, path or IP literal),
/// else `DEFAULT_APPROVAL_HOST`.
pub fn approval_host() -> &'static str {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| valid_host(std::env::var("AFO_ADMIN_HOST").ok().as_deref()).unwrap_or_else(|| DEFAULT_APPROVAL_HOST.to_string()))
}

/// A bare DNS hostname (lowercased), or `None`.
fn valid_host(raw: Option<&str>) -> Option<String> {
    let h = raw?.trim().to_ascii_lowercase();
    let shape = !h.is_empty() && h.len() <= 253 && h.contains('.') && !h.starts_with(['.', '-']) && !h.ends_with(['.', '-'])
        && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let ip_literal = h.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    (shape && !ip_literal).then_some(h)
}

/// `https://<admin host>/tickets/<id>` for a numeric ticket id; nothing the hub sent is used.
pub(crate) fn approval_url(ticket_id: &str) -> Option<url::Url> {
    approval_url_for(approval_host(), ticket_id)
}

fn approval_url_for(host: &str, ticket_id: &str) -> Option<url::Url> {
    if ticket_id.is_empty() || ticket_id.len() > 12 || !ticket_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    url::Url::parse(&format!("https://{host}/tickets/{ticket_id}")).ok()
}

/// The allowlist: https, exactly the admin host, no port, no credentials, path `/tickets/<digits>`,
/// no query, no fragment.
fn approval_allowed(u: &url::Url) -> bool {
    approval_allowed_for(approval_host(), u)
}

fn approval_allowed_for(host: &str, u: &url::Url) -> bool {
    let path_ok = u.path().strip_prefix("/tickets/").is_some_and(|id| !id.is_empty() && id.len() <= 12 && id.bytes().all(|b| b.is_ascii_digit()));
    u.scheme() == "https"
        && u.host_str() == Some(host)
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && path_ok
        && u.query().is_none()
        && u.fragment().is_none()
}

/// Open an approval page in the system browser. The allowlist is enforced here, so no caller can
/// open anything else through this function. Not reachable from the webview.
pub(crate) fn open_vetted(u: &url::Url) -> Result<(), String> {
    if !approval_allowed(u) {
        return Err("that link cannot be opened".into());
    }
    spawn_browser(u.as_str())
}

fn spawn_browser(u: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = {
        // the full path: a same-named program next to the app must not win
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut c = std::process::Command::new(std::path::PathBuf::from(root).join("System32").join("rundll32.exe"));
        c.args(["url.dll,FileProtocolHandler", u]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(u);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(u);
        c
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ()).map_err(|_| "could not open the browser".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_host_setting_must_be_a_bare_hostname() {
        assert_eq!(valid_host(Some(" Admin.Acme.example ")).as_deref(), Some("admin.acme.example"));
        for bad in [None, Some(""), Some("https://a.example"), Some("a.example:8443"), Some("a.example/x"), Some("localhost"), Some("10.0.0.5"), Some(".a.example"), Some("a b.example")] { // pii-scan: allow (test IP)
            assert!(valid_host(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn approval_pages_are_only_the_admin_ticket_page() {
        let ok = |s: &str| approval_allowed_for(DEFAULT_APPROVAL_HOST, &url::Url::parse(s).unwrap());
        assert!(ok("https://admin.example.com/tickets/12"));
        assert_eq!(approval_url_for(DEFAULT_APPROVAL_HOST, "12").unwrap().as_str(), "https://admin.example.com/tickets/12");
        for bad in [
            "http://127.0.0.1:8891/admin/approve/12", // pii-scan: allow (test address)
            "https://127.0.0.1/tickets/12",           // pii-scan: allow (test address)
            "https://evil.example/tickets/12",
            "https://admin.example.com.evil.example/tickets/12",
            "http://admin.example.com/tickets/12",
            "https://admin.example.com:8443/tickets/12",
            "https://admin.example.com/tickets/12?x=1",
            "https://admin.example.com/tickets/12#frag",
            "https://admin.example.com/tickets/",
            "https://admin.example.com/tickets/12/edit",
            "https://admin.example.com/tickets/abc",
            "https://admin.example.com/other/12",
            "https://user:pw@admin.example.com/tickets/12", // pii-scan: allow (test input)
            "javascript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
        ] {
            let parsed = url::Url::parse(bad).unwrap();
            assert!(!approval_allowed(&parsed), "{bad}");
            assert!(open_vetted(&parsed).is_err(), "{bad}");
        }
        for bad in ["", "abc", "12/../x", "1 2", "12?x", "1234567890123"] { // pii-scan: allow (test input)
            assert!(approval_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn only_plain_public_http_and_https_links_pass() {
        for ok in ["https://example.com", "http://example.com/a?b=c#d", " https://a.example/x ", "https://8.8.8.8/x"] { // pii-scan: allow (test IPs)
            assert!(checked(ok).is_ok(), "{ok}");
        }
        let creds = format!("https://user{}evil.example", '@');
        let bad_list = [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-msdt:/id x",
            "ftp://x.example",
            "//example.com",
            "C:\\Windows\\notepad.exe",
            "https://",
            "https://exa mple.com",
            "https://a.example/\"&calc",
            "https://a.example/x\ny",
            "HTTPS-not-a-link",
            "",
            creds.as_str(),
        ];
        for bad in bad_list {
            assert!(checked(bad).is_err(), "{bad:?}");
        }
        assert!(checked(&format!("https://a.example/{}", "x".repeat(3000))).is_err());
    }

    #[test]
    fn loopback_private_and_local_hosts_are_refused() {
        for bad in [
            "http://localhost:9333/json", "http://127.0.0.1/x", "http://127.1.2.3/", "http://10.0.0.5/", "http://192.168.1.1/", "http://172.16.0.1/", // pii-scan: allow (test IPs)
            "http://169.254.169.254/latest", "http://0.0.0.0/", "http://[::1]/", "http://[fc00::1]/", "http://[fe80::1]/", "http://[::ffff:127.0.0.1]/", // pii-scan: allow (test IPs)
            "http://printer.local/", "http://intranet/", "http://admin.localhost/", "http://svc.internal/", "http://localhost./", "http://printer.local./", "http://intranet./",
        ] {
            assert!(checked(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_long_query_or_fragment_cannot_carry_data_out() {
        assert!(checked(&format!("https://a.example/p?d={}", "x".repeat(150))).is_ok());
        assert!(checked(&format!("https://a.example/p?d={}", "x".repeat(300))).is_err());
        assert!(checked(&format!("https://a.example/p#{}", "y".repeat(300))).is_err());
    }
}
