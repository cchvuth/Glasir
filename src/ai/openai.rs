//! OpenAI Chat Completions client (the ChatGPT provider): one streamed (SSE)
//! completion, over `client::post` like the Claude client.

use super::client::{Endpoint, post};
use crate::git::Res;
use std::io::{BufRead, BufReader};

pub const DEFAULT_MODEL: &str = "gpt-5";
pub const MODELS: [&str; 2] = ["gpt-5", "gpt-5-mini"];
pub const DEFAULT_URL: &str = "https://api.openai.com/v1";

pub(super) fn complete(
    cfg: &Endpoint,
    token: &str,
    system: &str,
    prompt: &str,
    effort: &str,
    schema: Option<&serde_json::Value>,
    on_text: &mut dyn FnMut(String) -> bool,
) -> Res<()> {
    let model = cfg.model.trim();
    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
    });
    // Reasoning models only; other models (and most gateways) reject the field.
    if model.starts_with("gpt-5") && !model.contains("chat") {
        body["reasoning_effort"] = effort.into();
    }
    if let Some(s) = schema {
        body["response_format"] = serde_json::json!({"type": "json_schema", "json_schema": {"name": "reply", "strict": true, "schema": s}});
    }
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = post(&url, &[("authorization", format!("Bearer {token}"))], &body.to_string(), "OpenAI")?;
    let mut finish = None;
    // `lines()` buffers, so an event split across network chunks arrives whole.
    for line in BufReader::new(body.into_reader()).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        if let Some(e) = v["error"]["message"].as_str() {
            return Err(format!("OpenAI: {e}"));
        }
        let choice = &v["choices"][0];
        if choice["delta"]["refusal"].is_string() {
            return Err("ChatGPT declined this request.".into());
        }
        if !on_text(choice["delta"]["content"].as_str().unwrap_or("").into()) {
            return Ok(());
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            finish = Some(f.to_owned());
        }
    }
    match finish.as_deref() {
        Some("stop") => Ok(()),
        Some("length") => Err("The answer was cut off (the model's output limit was reached).".into()),
        Some("content_filter") => Err("OpenAI's content filter blocked the answer.".into()),
        None => Err("ChatGPT's response ended early (connection dropped?). Try again.".into()),
        Some(o) => Err(format!("Unexpected finish reason: {o}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::client::tests::serve;

    fn run(response: String, schema: Option<&serde_json::Value>) -> (Res<()>, String, String) {
        let (url, h) = serve(response);
        let cfg = Endpoint { base_url: format!("{url}/v1"), ..Default::default() };
        let mut out = String::new();
        let r = complete(&cfg, "sk-test", "sys", "hi", "low", schema, &mut |t| {
            out += &t;
            true
        });
        (r, out, h.join().unwrap())
    }

    fn chunk(text: &str, finish: Option<&str>) -> String {
        let f = finish.map_or("null".into(), |f| format!("\"{f}\""));
        format!(r#"data: {{"choices":[{{"index":0,"delta":{{"content":{}}},"finish_reason":{f}}}]}}"#, serde_json::json!(text))
    }

    /// The SSE body sent with chunked encoding, cut into pieces mid-event.
    fn sse(events: &[String]) -> String {
        let body: String = events.iter().map(|e| format!("{e}\n\n")).collect::<String>() + "data: [DONE]\n\n";
        let mid = body.len() / 2;
        let pieces: String = [&body[..7], &body[7..mid], &body[mid..]].iter().map(|p| format!("{:x}\r\n{p}\r\n", p.len())).collect();
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{pieces}0\r\n\r\n")
    }

    #[test]
    fn streams_text() {
        let (r, out, req) = run(sse(&[chunk("", None), chunk("Fix ", None), chunk("bug", None), chunk("", Some("stop"))]), None);
        assert_eq!((r, out.as_str()), (Ok(()), "Fix bug"));
        assert!(req.starts_with("POST /v1/chat/completions "), "{req}");
        assert!(req.to_lowercase().contains("authorization: bearer sk-test"));
        let body: serde_json::Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!((&body["model"], &body["stream"], &body["reasoning_effort"]), (&"gpt-5".into(), &true.into(), &"low".into()));
        assert_eq!(body["messages"], serde_json::json!([{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]));
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn structured_output_request() {
        let schema = serde_json::json!({"type": "object", "additionalProperties": false});
        let (r, _, req) = run(sse(&[chunk("{}", Some("stop"))]), Some(&schema));
        assert!(r.is_ok());
        let body: serde_json::Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["response_format"], serde_json::json!({"type": "json_schema", "json_schema": {"name": "reply", "strict": true, "schema": schema}}));
    }

    #[test]
    fn finish_reasons_and_errors() {
        let cut = run(sse(&[chunk("Fix", Some("length"))]), None);
        assert!(cut.0.unwrap_err().contains("cut off"));
        assert!(run(sse(&[chunk("", Some("content_filter"))]), None).0.unwrap_err().contains("content filter"));
        let refusal = r#"data: {"choices":[{"index":0,"delta":{"refusal":"I can't"},"finish_reason":null}]}"#;
        assert_eq!(run(sse(&[refusal.into()]), None).0, Err("ChatGPT declined this request.".into()));
        assert!(run(sse(&[chunk("Fix", None)]), None).0.unwrap_err().contains("ended early"));
        let body = r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        let resp = format!("HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
        let err = run(resp, None).0.unwrap_err();
        assert!(err.contains("OpenAI rejected the token (401)") && err.contains("Incorrect API key"), "{err}");
    }
}
