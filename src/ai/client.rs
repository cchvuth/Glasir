//! AI provider settings and token storage (env var, OS keychain, private file),
//! the shared HTTP post, and the Claude Messages API client (streamed SSE).

use super::openai;
use crate::git::Res;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const MODELS: [&str; 4] = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"];
pub const DEFAULT_URL: &str = "https://api.anthropic.com";
const SERVICE: &str = "glasir";

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Provider {
    #[default]
    Claude,
    OpenAi,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::OpenAi];

    /// The assistant's name in UI text ("Asks Claude").
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::OpenAi => "ChatGPT",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude (Anthropic)",
            Provider::OpenAi => "ChatGPT (OpenAI)",
        }
    }

    pub fn models(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &MODELS,
            Provider::OpenAi => &openai::MODELS,
        }
    }

    /// The token's keychain account and private-file name.
    fn account(self) -> &'static str {
        match self {
            Provider::Claude => "claude-token",
            Provider::OpenAi => "openai-token",
        }
    }
}

/// Model, API base URL and saved-token location of the OpenAI provider (Claude's
/// stay top-level fields of `Config`, where older settings have them).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Endpoint {
    pub model: String,
    pub base_url: String,
    pub saved_in: Option<String>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint { model: openai::DEFAULT_MODEL.into(), base_url: openai::DEFAULT_URL.into(), saved_in: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub base_url: String,
    /// Settings → "Enable AI features": off hides every AI button and menu item.
    pub enabled: bool,
    /// Where the token was saved, so Settings can say so without reading the
    /// keychain: `Some("")` = none saved, `None` = not known yet (older settings).
    pub saved_in: Option<String>,
    pub openai: Endpoint,
    /// Settings → AI → "Custom instructions", appended to every AI prompt.
    pub instructions: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { provider: Provider::Claude, model: DEFAULT_MODEL.into(), base_url: DEFAULT_URL.into(), enabled: true, saved_in: None, openai: Endpoint::default(), instructions: String::new() }
    }
}

impl Config {
    /// The selected provider's model, API base URL and saved-token location.
    pub fn current(&mut self) -> (&mut String, &mut String, &mut Option<String>) {
        match self.provider {
            Provider::Claude => (&mut self.model, &mut self.base_url, &mut self.saved_in),
            Provider::OpenAi => (&mut self.openai.model, &mut self.openai.base_url, &mut self.openai.saved_in),
        }
    }
}

pub const IN_KEYCHAIN: &str = "the system keychain";
pub const IN_FILE: &str = "a private file (no system keychain available)";

/// The environment variable the token comes from, if one is set.
pub fn env_source(p: Provider) -> Option<&'static str> {
    env_token(p).map(|_| match p {
        Provider::Claude => "the ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN environment variable",
        Provider::OpenAi => "the OPENAI_API_KEY environment variable",
    })
}

type Saved = Option<Option<(String, &'static str)>>;

/// Each provider's saved token and where it is, looked up at most once per run:
/// macOS can ask for the user's password on every keychain read, so after the
/// first one the token comes from memory (and Save / Remove update the memory too).
static SAVED: std::sync::Mutex<[Saved; 2]> = std::sync::Mutex::new([None, None]);

fn saved(p: Provider) -> Option<(String, &'static str)> {
    let mut s = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    s[p as usize].get_or_insert_with(|| keychain_token(p).map(|t| (t, IN_KEYCHAIN)).or_else(|| plain_token(p).map(|t| (t, IN_FILE)))).clone()
}

fn remember(p: Provider, v: Option<(String, &'static str)>) {
    SAVED.lock().unwrap_or_else(|e| e.into_inner())[p as usize] = Some(v);
}

/// Where a token is saved (reads the keychain if this run hasn't yet).
pub fn find_saved(p: Provider) -> Option<&'static str> {
    saved(p).map(|(_, at)| at)
}

fn env_token(p: Provider) -> Option<(String, bool)> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match p {
        Provider::Claude => get("ANTHROPIC_API_KEY").map(|t| (t, false)).or_else(|| get("ANTHROPIC_AUTH_TOKEN").map(|t| (t, true))),
        Provider::OpenAi => get("OPENAI_API_KEY").map(|t| (t, true)),
    }
}

fn keychain_token(p: Provider) -> Option<String> {
    keychain::get(SERVICE, p.account()).or_else(|| {
        if p != Provider::Claude {
            return None;
        }
        // Carry over a token saved under the app's former name ("cuttle").
        let t = keychain::get("cuttle", p.account())?;
        if keychain::set(SERVICE, p.account(), &t).is_ok() {
            let _ = keychain::delete("cuttle", p.account());
        }
        Some(t)
    })
}

/// macOS: the login keychain, via Apple's `security` tool. The keychain trusts an
/// item's creator by code signature, which changes with every Glasir build; one
/// "Always Allow" for `security` lasts.
#[cfg(target_os = "macos")]
mod keychain {
    use std::io::Write;
    use std::process::{Command, Stdio};

    const SECURITY: &str = "/usr/bin/security";

    pub fn get(service: &str, account: &str) -> Option<String> {
        let o = Command::new(SECURITY).args(["find-generic-password", "-s", service, "-a", account, "-w"]).stderr(Stdio::null()).output().ok()?;
        let t = String::from_utf8(o.stdout).ok()?.trim().to_owned();
        (o.status.success() && !t.is_empty()).then_some(t)
    }

    pub fn set(service: &str, account: &str, token: &str) -> Result<(), String> {
        if !token.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return Err("the token contains unexpected characters".into());
        }
        delete(service, account)?; // a fresh item, owned by `security`
        // The secret goes over stdin (`security -i`), never on a command line.
        let mut c = Command::new(SECURITY).arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let cmd = format!("add-generic-password -s {service} -a {account} -w \"{token}\"\n");
        c.stdin.take().ok_or("no stdin")?.write_all(cmd.as_bytes()).map_err(|e| e.to_string())?;
        let o = c.wait_with_output().map_err(|e| e.to_string())?;
        // `security -i` exits 0 even when a command fails: read the item back.
        match get(service, account) {
            Some(t) if t == token => Ok(()),
            _ => Err(format!("keychain: {}", String::from_utf8_lossy(&o.stderr).trim())),
        }
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        let o = Command::new(SECURITY).args(["delete-generic-password", "-s", service, "-a", account]).output().map_err(|e| e.to_string())?;
        // 44: no such item.
        match o.status.code() {
            Some(0 | 44) => Ok(()),
            _ => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        }
    }
}

/// Windows Credential Manager / Linux Secret Service.
#[cfg(not(target_os = "macos"))]
mod keychain {
    pub fn get(service: &str, account: &str) -> Option<String> {
        keyring::Entry::new(service, account).ok()?.get_password().ok()
    }

    pub fn set(service: &str, account: &str, token: &str) -> Result<(), String> {
        keyring::Entry::new(service, account).and_then(|e| e.set_password(token)).map_err(|e| e.to_string())
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        match keyring::Entry::new(service, account).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `(token, bearer)`: OAuth tokens (`sk-ant-oat…`) authenticate with
/// `Authorization: Bearer`, as do all OpenAI keys.
fn token(p: Provider) -> Option<(String, bool)> {
    env_token(p).or_else(|| saved(p).map(|(t, _)| {
        let bearer = p == Provider::OpenAi || t.starts_with("sk-ant-oat");
        (t, bearer)
    }))
}

/// Fallback when there is no keychain (e.g. Linux without Secret Service):
/// a file readable only by the user.
fn plain_path(p: Provider) -> Option<PathBuf> {
    eframe::storage_dir("Glasir").map(|d| d.join(p.account()))
}

fn plain_token(p: Provider) -> Option<String> {
    std::fs::read_to_string(plain_path(p)?).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

pub fn save_plain_token(pr: Provider, token: &str) -> Res<()> {
    let p = plain_path(pr).ok_or("no config directory")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    use std::io::Write;
    o.open(&p).and_then(|mut f| f.write_all(token.trim().as_bytes())).map_err(|e| e.to_string())?;
    remember(pr, Some((token.trim().to_owned(), IN_FILE)));
    Ok(())
}

pub fn remove_plain_token(pr: Provider) {
    if let Some(p) = plain_path(pr) {
        let _ = std::fs::remove_file(p);
    }
    if find_saved_cached(pr) == Some(IN_FILE) {
        remember(pr, None);
    }
}

/// Where the token is saved, only if this run already looked (never reads).
fn find_saved_cached(p: Provider) -> Option<&'static str> {
    SAVED.lock().unwrap_or_else(|e| e.into_inner())[p as usize].as_ref().and_then(|v| v.as_ref().map(|(_, at)| *at))
}

/// Stores the token in the system keychain (empty = remove).
pub fn save_token(p: Provider, token: &str) -> Res<()> {
    if token.trim().is_empty() {
        keychain::delete(SERVICE, p.account())?;
        remember(p, None);
        return Ok(());
    }
    keychain::set(SERVICE, p.account(), token.trim())?;
    remember(p, Some((token.trim().to_owned(), IN_KEYCHAIN)));
    Ok(())
}

/// The OS TLS stack and trust store (corporate CAs); rustls + the platform verifier on Linux.
pub fn tls() -> ureq::tls::TlsConfig {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier);
    #[cfg(not(target_os = "linux"))]
    let tls = tls.provider(ureq::tls::TlsProvider::NativeTls);
    tls.build()
}

pub(super) enum Ev {
    Text(String),
    Done(Res<()>),
}

pub(super) fn need_token(p: Provider) -> Res<(String, bool)> {
    token(p).ok_or_else(|| match p {
        Provider::Claude => "No Claude token. Add one in Settings → AI, or set ANTHROPIC_API_KEY.".into(),
        Provider::OpenAi => "No OpenAI API key. Add one in Settings → AI, or set OPENAI_API_KEY.".into(),
    })
}

/// POSTs a JSON `payload`, retrying twice on network errors, timeouts, rate
/// limits and server errors. A non-200 reply becomes an error naming `api`.
pub(super) fn post(url: &str, headers: &[(&str, String)], payload: &str, api: &str) -> Res<ureq::Body> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls())
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(180)))
        .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
        // Never follow redirects: they would carry the token to another host.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();

    let mut attempt = 0;
    let resp = loop {
        let mut req = agent.post(url).header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let res = req.send(payload);
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
            401 | 403 => format!("{api} rejected the token ({status}): {msg}"),
            429 => format!("Rate limited by the {api} API: {msg}"),
            _ => format!("{api} API error {status}: {msg}"),
        });
    }
    Ok(body)
}

/// Streams one completion from the selected provider; `on_text` returns false
/// to cancel. `schema`: the reply is JSON matching this JSON Schema (structured output).
pub(super) fn complete(
    cfg: &Config,
    (token, bearer): (String, bool),
    system: &str,
    prompt: &str,
    effort: &str,
    schema: Option<&serde_json::Value>,
    on_text: &mut dyn FnMut(String) -> bool,
) -> Res<()> {
    if cfg.provider == Provider::OpenAi {
        return openai::complete(&cfg.openai, &token, system, prompt, effort, schema, on_text);
    }
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
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let mut headers = vec![("anthropic-version", "2023-06-01".to_owned())];
    headers.push(if bearer { ("authorization", format!("Bearer {token}")) } else { ("x-api-key", token) });
    if !betas.is_empty() {
        headers.push(("anthropic-beta", betas.join(",")));
    }
    let body = post(&url, &headers, &body.to_string(), "Claude")?;
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
pub(crate) mod tests {
    /// Real login keychain, under a throwaway service name: `cargo test -- --ignored keychain`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        use super::keychain::{delete, get, set};
        let (svc, acc) = ("glasir-selftest", "openai-token");
        set(svc, acc, "sk-ant-test-123_abc").unwrap();
        assert_eq!(get(svc, acc).as_deref(), Some("sk-ant-test-123_abc"));
        set(svc, acc, "sk-ant-test-456").unwrap();
        assert_eq!(get(svc, acc).as_deref(), Some("sk-ant-test-456"));
        assert!(set(svc, acc, "bad\" token").is_err());
        delete(svc, acc).unwrap();
        delete(svc, acc).unwrap(); // already gone: still fine
        assert_eq!(get(svc, acc), None);
    }

    use super::*;
    use std::io::{Read, Write};

    /// One-shot HTTP server: returns its URL and the raw request it received.
    pub(crate) fn serve(response: String) -> (String, std::thread::JoinHandle<String>) {
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
