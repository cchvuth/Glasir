//! Claude Messages API client: token lookup/storage (env var -> OS keychain ->
//! private file) and one streamed (SSE) completion over ureq.

use crate::git::Res;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const MODELS: [&str; 4] = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"];
pub const DEFAULT_URL: &str = "https://api.anthropic.com";
const KEYCHAIN: (&str, &str) = ("glasir", "claude-token");

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub base_url: String,
    /// Settings → "Enable AI features": off hides every AI button and menu item.
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { model: DEFAULT_MODEL.into(), base_url: DEFAULT_URL.into(), enabled: true }
    }
}

/// Where the token comes from, for the settings screen.
pub fn token_source() -> Option<&'static str> {
    if env_token().is_some() {
        Some("the ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN environment variable")
    } else if keychain_token().is_some() {
        Some("the system keychain")
    } else if plain_token().is_some() {
        Some("a private file (no system keychain available)")
    } else {
        None
    }
}

fn env_token() -> Option<(String, bool)> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    get("ANTHROPIC_API_KEY").map(|t| (t, false)).or_else(|| get("ANTHROPIC_AUTH_TOKEN").map(|t| (t, true)))
}

fn keychain_token() -> Option<String> {
    let entry = keyring::Entry::new(KEYCHAIN.0, KEYCHAIN.1).ok()?;
    entry.get_password().ok().or_else(|| {
        // Carry over a token saved under the app's former name ("cuttle").
        let old = keyring::Entry::new("cuttle", KEYCHAIN.1).ok()?;
        let t = old.get_password().ok()?;
        if entry.set_password(&t).is_ok() {
            let _ = old.delete_credential();
        }
        Some(t)
    })
}

/// `(token, bearer)`: OAuth tokens (`sk-ant-oat…`) authenticate with `Authorization: Bearer`.
fn token() -> Option<(String, bool)> {
    let stored = keychain_token().or_else(plain_token);
    env_token().or_else(|| stored.map(|t| {
        let bearer = t.starts_with("sk-ant-oat");
        (t, bearer)
    }))
}

/// Fallback when there is no keychain (e.g. Linux without Secret Service):
/// a file readable only by the user.
fn plain_path() -> Option<PathBuf> {
    eframe::storage_dir("Glasir").map(|d| d.join("claude-token"))
}

fn plain_token() -> Option<String> {
    std::fs::read_to_string(plain_path()?).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

pub fn save_plain_token(token: &str) -> Res<()> {
    let p = plain_path().ok_or("no config directory")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    use std::io::Write;
    o.open(&p).and_then(|mut f| f.write_all(token.trim().as_bytes())).map_err(|e| e.to_string())
}

pub fn remove_plain_token() {
    if let Some(p) = plain_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Stores the token in the system keychain (empty = remove).
pub fn save_token(token: &str) -> Res<()> {
    let e = keyring::Entry::new(KEYCHAIN.0, KEYCHAIN.1).map_err(|e| e.to_string())?;
    if token.trim().is_empty() {
        return match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        };
    }
    e.set_password(token.trim()).map_err(|e| e.to_string())
}

pub(super) enum Ev {
    Text(String),
    Done(Res<()>),
}

/// Streams one completion. Returns early (dropping the connection) once `tx` is closed.
pub(super) fn need_token() -> Res<(String, bool)> {
    token().ok_or_else(|| "No Claude token. Add one in Settings → AI, or set ANTHROPIC_API_KEY.".into())
}

/// Streams one completion; `on_text` returns false to cancel. `schema`: the
/// reply is JSON matching this JSON Schema (structured output).
pub(super) fn complete(
    cfg: &Config,
    (token, bearer): (String, bool),
    system: &str,
    prompt: &str,
    effort: &str,
    schema: Option<&serde_json::Value>,
    on_text: &mut dyn FnMut(String) -> bool,
) -> Res<()> {
    let first_party = cfg.base_url.trim_end_matches('/') == DEFAULT_URL;
    let model = cfg.model.trim();
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": 64000,
        "stream": true,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
    });
    let mut output = serde_json::Map::new();
    if !model.starts_with("claude-3") && !model.contains("4-5") {
        output.insert("effort".into(), effort.into());
    }
    if let Some(s) = schema {
        output.insert("format".into(), serde_json::json!({ "type": "json_schema", "schema": s }));
    }
    if !output.is_empty() {
        body["output_config"] = output.into();
    }
    let mut betas = vec![];
    // Server-side fallback when a safety classifier declines (not offered for Haiku).
    if first_party && ["claude-opus-5", "claude-fable-5", "claude-sonnet-5-5"].iter().any(|p| model.starts_with(p)) {
        body["fallbacks"] = "default".into();
        betas.push("server-side-fallback-2026-07-01");
    }
    if bearer {
        betas.push("oauth-2025-04-20");
    }
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier);
    #[cfg(not(target_os = "linux"))]
    let tls = tls.provider(ureq::tls::TlsProvider::NativeTls);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls.build())
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(180)))
        .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
        // Never follow redirects: they would carry the token to another host.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let payload = body.to_string();

    let mut attempt = 0;
    let resp = loop {
        let mut req = agent
            .post(&url)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01");
        req = if bearer { req.header("authorization", format!("Bearer {token}")) } else { req.header("x-api-key", &token) };
        if !betas.is_empty() {
            req = req.header("anthropic-beta", betas.join(","));
        }
        let res = req.send(payload.as_str());
        let retry = match &res {
            Ok(r) => matches!(r.status().as_u16(), 408 | 429 | 500..=599),
            Err(_) => true,
        };
        if retry && attempt < 2 {
            attempt += 1;
            std::thread::sleep(Duration::from_secs(2 * attempt));
            continue;
        }
        break res.map_err(|e| format!("Could not reach {url}: {e}"))?;
    };
    let status = resp.status().as_u16();
    let mut body = resp.into_body();
    if status != 200 {
        let text = body.read_to_string().unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(text);
        return Err(match status {
            401 | 403 => format!("Claude rejected the token ({status}): {msg}"),
            429 => format!("Rate limited by the Claude API: {msg}"),
            _ => format!("Claude API error {status}: {msg}"),
        });
    }
    let mut stop = None;
    for line in BufReader::new(body.into_reader()).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) else { continue };
        match v["type"].as_str() {
            Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
                if !on_text(v["delta"]["text"].as_str().unwrap_or("").into()) {
                    return Ok(());
                }
            }
            Some("message_delta") => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    stop = Some(s.to_owned());
                }
            }
            Some("error") => return Err(v["error"]["message"].as_str().unwrap_or("stream error").into()),
            _ => {}
        }
    }
    match stop.as_deref() {
        Some("end_turn" | "stop_sequence") => Ok(()),
        Some("refusal") => Err("Claude declined this request.".into()),
        Some("max_tokens" | "model_context_window_exceeded") => Err("The answer was cut off.".into()),
        None => Err("Claude's response ended early (connection dropped?). Try again.".into()),
        Some(o) => Err(format!("Unexpected stop reason: {o}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// One-shot HTTP server: returns its URL and the raw request it received.
    fn serve(response: String) -> (String, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let (mut req, mut buf) = (vec![], [0u8; 8192]);
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&req).to_string();
                if let Some(i) = t.find("\r\n\r\n") {
                    let len = t[..i]
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if req.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        (url, h)
    }

    fn sse(events: &[&str]) -> String {
        let body: String = events.iter().map(|e| format!("event: x\ndata: {e}\n\n")).collect();
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
    }

    fn run(url: String, tok: (&str, bool)) -> (Res<()>, String) {
        let cfg = Config { base_url: url, ..Default::default() };
        let mut out = String::new();
        let r = complete(&cfg, (tok.0.into(), tok.1), "sys", "hi", "low", None, &mut |t| {
            out += &t;
            true
        });
        (r, out)
    }

    const DELTA: &str = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Fix bug"}}"#;
    const THINK: &str = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#;

    #[test]
    fn streams_text_with_api_key() {
        let (url, h) = serve(sse(&[THINK, DELTA, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#]));
        let (r, out) = run(url, ("sk-ant-api-test", false));
        assert_eq!((r, out.as_str()), (Ok(()), "Fix bug"));
        let req = h.join().unwrap();
        let lower = req.to_lowercase();
        assert!(req.starts_with("POST /v1/messages "), "{req}");
        assert!(lower.contains("x-api-key: sk-ant-api-test") && lower.contains("anthropic-version: 2023-06-01"));
        assert!(req.contains(r#""stream":true"#) && req.contains(r#""model":"claude-opus-5-5""#));
        assert!(req.contains(r#""effort":"low""#) && !req.contains("fallbacks"), "gateway URL: no server-side fallback");
    }

    #[test]
    fn structured_output_schema() {
        let (url, h) = serve(sse(&[DELTA, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#]));
        let cfg = Config { base_url: url, ..Default::default() };
        let schema = serde_json::json!({"type": "object", "additionalProperties": false});
        assert!(complete(&cfg, ("k".into(), false), "sys", "hi", "medium", Some(&schema), &mut |_| true).is_ok());
        let req = h.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["output_config"], serde_json::json!({"effort": "medium", "format": {"type": "json_schema", "schema": schema}}));
    }

    #[test]
    fn oauth_tokens_use_bearer() {
        let (url, h) = serve(sse(&[DELTA, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#]));
        assert!(run(url, ("sk-ant-oat01-x", true)).0.is_ok());
        let req = h.join().unwrap().to_lowercase();
        assert!(req.contains("authorization: bearer sk-ant-oat01-x") && req.contains("anthropic-beta: oauth-2025-04-20"));
        assert!(!req.contains("x-api-key"));
    }

    #[test]
    fn refusal_and_http_errors() {
        let (url, _) = serve(sse(&[DELTA, r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#]));
        assert_eq!(run(url, ("k", false)).0, Err("Claude declined this request.".into()));
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let (url, _) = serve(format!("HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len()));
        let err = run(url, ("k", false)).0.unwrap_err();
        assert!(err.contains("rejected the token") && err.contains("invalid x-api-key"), "{err}");
    }
}
