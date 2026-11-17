//! WebSocket connections to ElevenLabs (streaming TTS and live speech-to-text).
//! Only `wss://api.elevenlabs.io/` is ever dialled. The key travels in the `xi-api-key` header,
//! never in the URL, and TLS is rustls with the ring provider and the webpki roots.

use crate::secret::ApiKey;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub const HOST: &str = "wss://api.elevenlabs.io/";
/// Largest message accepted from the server (one audio chunk is far below this).
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// True only for a `wss://api.elevenlabs.io/...` URL (no other host, port, user info or scheme).
pub fn allowed_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    u.scheme() == "wss" && u.host_str() == Some("api.elevenlabs.io") && u.port().is_none() && u.username().is_empty() && u.password().is_none() && url.starts_with(HOST)
}

fn tls() -> Arc<rustls::ClientConfig> {
    static CFG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

/// Open a socket, bounded by the HTTP connect timeout. Errors never contain the key or URL.
pub async fn connect(url: &str, key: &ApiKey) -> Result<Ws, String> {
    if !allowed_url(url) {
        return Err("refused to connect: not the ElevenLabs API".into());
    }
    let mut req = url.into_client_request().map_err(|_| "bad socket address".to_string())?;
    let mut v = HeaderValue::from_str(key.expose()).map_err(|_| "the key has characters a header cannot carry".to_string())?;
    v.set_sensitive(true);
    req.headers_mut().insert("xi-api-key", v);
    let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(MAX_MESSAGE);
    cfg.max_frame_size = Some(MAX_MESSAGE);
    let fut = tokio_tungstenite::connect_async_tls_with_config(req, Some(cfg), false, Some(Connector::Rustls(tls())));
    match tokio::time::timeout(crate::eleven::CONNECT_TIMEOUT, fut).await {
        Err(_) => Err("the voice service did not answer in time".into()),
        Ok(Err(e)) => Err(describe(&e)),
        Ok(Ok((ws, _))) => Ok(ws),
    }
}

/// A short, key-free reason for a failed connect (the HTTP status when the server refused).
fn describe(e: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Http(resp) => crate::eleven::status_message(reqwest::StatusCode::from_u16(resp.status().as_u16()).unwrap_or(reqwest::StatusCode::BAD_GATEWAY)),
        E::Io(_) => "could not reach the voice service (network)".into(),
        E::Tls(_) => "a secure connection to the voice service failed".into(),
        _ => "the voice service connection failed".into(),
    }
}

pub async fn send_json(ws: &mut Ws, v: &serde_json::Value) -> Result<(), String> {
    ws.send(Message::text(v.to_string())).await.map_err(|_| "the voice connection dropped".to_string())
}

/// The next JSON object from the server. `Ok(None)` = closed. A close frame's reason (the server
/// puts its error text there) is returned as the error, cut and redacted.
pub async fn next_json(ws: &mut Ws) -> Result<Option<serde_json::Value>, String> {
    loop {
        match ws.next().await {
            None => return Ok(None),
            Some(Err(_)) => return Err("the voice connection dropped".into()),
            Some(Ok(Message::Text(t))) => {
                if let Ok(v) = serde_json::from_str(t.as_str()) {
                    return Ok(Some(v));
                }
            }
            Some(Ok(Message::Close(Some(f)))) if !f.reason.is_empty() => return Err(server_error(&f.reason)),
            Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Ok(_)) => {} // ping/pong/binary: nothing to do
        }
    }
}

/// Server error text, safe to show: redacted, one line, cut.
pub fn server_error(s: &str) -> String {
    let one: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    crate::claude::redact_then_truncate(one.trim(), 160)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_elevenlabs_wss_host_is_ever_dialled() {
        assert!(allowed_url("wss://api.elevenlabs.io/v1/text-to-dialogue/stream-input?model_id=x"));
        assert!(allowed_url("wss://api.elevenlabs.io/v1/speech-to-text/realtime"));
        for bad in [
            "ws://api.elevenlabs.io/v1/x",
            "https://api.elevenlabs.io/v1/x",
            "wss://api.elevenlabs.io.evil.example/v1/x",
            "wss://evil.example/v1/x?h=api.elevenlabs.io",
            "wss://user@api.elevenlabs.io/v1/x", // pii-scan: allow (test URL with user info)
            "wss://api.elevenlabs.io:444/v1/x",
            "not a url",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
    }

    #[test]
    fn tls_config_builds_with_the_ring_provider() {
        let c = tls();
        assert!(!c.alpn_protocols.iter().any(|p| p == b"h2"), "plain http/1.1 upgrade");
    }

    #[test]
    fn server_errors_are_one_short_redacted_line() {
        let secret = "q1w2e3r4t5".repeat(4);
        let e = server_error(&format!("quota exceeded\nkey {secret} {}", "x".repeat(400)));
        assert!(!e.contains('\n') && !e.contains("q1w2e3r4") && e.chars().count() <= 163);
    }
}
