//! ElevenLabs API key handling. The key lives only in Rust memory.
//! It is never serialised, logged, put in an error string, or sent to the webview.

use std::fmt;
use std::path::PathBuf;

pub const KEYRING_SERVICE: &str = "all-for-one-one";
pub const KEYRING_USER: &str = "elevenlabs";

/// A secret string whose `Debug`/`Display` always redact.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(s: impl Into<String>) -> Self {
        ApiKey(s.into())
    }
    /// The only way to read the raw value: used when building the request header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn key_file_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join("secrets").join("elevenlabs.json"))
}

/// Pull `api_key` out of the secrets JSON. Errors never echo the content.
pub fn parse_key_json(raw: &str) -> Result<ApiKey, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|_| "key file is not valid JSON".to_string())?;
    match v.get("api_key").and_then(|k| k.as_str()).map(str::trim) {
        Some(k) if !k.is_empty() => Ok(ApiKey::new(k)),
        _ => Err("key file has no api_key field".to_string()),
    }
}

fn load_from_file() -> Option<ApiKey> {
    let raw = std::fs::read_to_string(key_file_path()?).ok()?;
    parse_key_json(&raw).ok()
}

fn load_from_keyring() -> Option<ApiKey> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()?;
    entry.get_password().ok().filter(|p| !p.trim().is_empty()).map(ApiKey::new)
}

/// Secrets file first, then the OS keychain.
pub fn load_key() -> Result<ApiKey, String> {
    load_from_file()
        .or_else(load_from_keyring)
        .ok_or_else(|| "No ElevenLabs key found (secrets file or OS keychain)".to_string())
}

/// Which source would be used, for the UI status line. Never returns the key.
pub fn key_source() -> &'static str {
    if load_from_file().is_some() {
        "file"
    } else if load_from_keyring().is_some() {
        "keychain"
    } else {
        "none"
    }
}

pub fn store_in_keyring(key: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|e| e.set_password(key))
        .map_err(|e| format!("keychain write failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_redact() {
        let k = ApiKey::new("sk_super_secret_value"); // pii-scan: allow (test fixture)
        assert!(!format!("{k:?}").contains("super_secret"));
        assert!(!format!("{k}").contains("super_secret"));
        assert!(!format!("{:?}", Some(&k)).contains("super_secret"));
    }

    #[test]
    fn parse_key_json_ok_and_errors_do_not_leak() {
        assert_eq!(parse_key_json(r#"{"api_key":" abc "}"#).unwrap().expose(), "abc");
        let e = parse_key_json(r#"{"other":"sk_leak_me"}"#).unwrap_err();
        assert!(!e.contains("sk_leak_me"));
        let e = parse_key_json("not json sk_leak_me").unwrap_err();
        assert!(!e.contains("sk_leak_me"));
    }

    /// Run with: cargo test keyring_roundtrip -- --ignored
    /// Uses a throwaway service name so it never touches the real key entry.
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let entry = keyring::Entry::new("all-for-one-one-test", "roundtrip").unwrap();
        entry.set_password("test-value-123").unwrap();
        assert_eq!(entry.get_password().unwrap(), "test-value-123");
        entry.delete_credential().unwrap();
        assert!(entry.get_password().is_err());
    }
}
