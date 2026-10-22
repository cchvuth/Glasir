//! AI through a coding-agent CLI the user is already signed in to (Claude Code, Codex),
//! instead of an API key: the CLI runs with no tools, in an empty folder, once per request.

use super::client::{Endpoint, Provider};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

type Res<T> = Result<T, String>;

pub const DEFAULT_MODEL: &str = "default";
pub const CLAUDE_MODELS: [&str; 4] = [DEFAULT_MODEL, "opus", "sonnet", "haiku"];
pub const CODEX_MODELS: [&str; 1] = [DEFAULT_MODEL];

pub fn endpoint(p: Provider) -> Endpoint {
    Endpoint { model: DEFAULT_MODEL.into(), base_url: if p == Provider::Codex { "codex" } else { "claude" }.into(), saved_in: None }
}

pub(super) fn complete(p: Provider, ep: &Endpoint, system: &str, prompt: &str, schema: Option<&serde_json::Value>, on_text: &mut dyn FnMut(String) -> bool) -> Res<()> {
    let dir = std::env::temp_dir().join(format!("glasir-ai-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let model = Some(ep.model.trim()).filter(|m| !m.is_empty() && *m != DEFAULT_MODEL);
    let program = Some(ep.base_url.trim()).filter(|c| !c.is_empty()).unwrap_or(if p == Provider::Codex { "codex" } else { "claude" });
    match p {
        Provider::Codex => codex(program, &dir, model, &format!("{system}\n\n{prompt}"), schema, on_text),
        _ => claude(program, &dir, model, system, prompt, schema, on_text),
    }
}

fn spawn(program: &str, dir: &Path, args: &[String]) -> Res<Child> {
    let run = |exe: &str| {
        let mut c = Command::new(exe);
        c.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::git::spawn(&mut c)
    };
    let res = run(program);
    #[cfg(windows)]
    let res = res.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound && !program.contains('.') { run(&format!("{program}.cmd")) } else { Err(e) });
    res.map_err(|e| format!("Could not run {program}: {e}. Install it, sign in once in a terminal, or set its full path in Settings → AI."))
}

fn feed(child: &mut Child, input: String) {
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
}

fn stderr_of(child: &mut Child) -> std::thread::JoinHandle<String> {
    let mut err = child.stderr.take();
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    })
}

fn failed(program: &str, status: std::process::ExitStatus, err: &str) -> String {
    let err = err.trim();
    if err.is_empty() { format!("{program} exited with {status}") } else { format!("{program}: {err}") }
}

fn claude(program: &str, dir: &Path, model: Option<&str>, system: &str, prompt: &str, schema: Option<&serde_json::Value>, on_text: &mut dyn FnMut(String) -> bool) -> Res<()> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--tools", "", "--no-session-persistence", "--strict-mcp-config", "--setting-sources", "project", "--system-prompt", system]
        .map(String::from)
        .to_vec();
    if let Some(m) = model {
        args.extend(["--model".into(), m.into()]);
    }
    if let Some(s) = schema {
        args.extend(["--json-schema".into(), s.to_string()]);
    }
    let mut child = spawn(program, dir, &args)?;
    feed(&mut child, prompt.to_owned());
    let err = stderr_of(&mut child);
    let (mut streamed, mut result) = (false, None);
    for line in BufReader::new(child.stdout.take().ok_or("no output")?).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line.map_err(|e| e.to_string())?) else { continue };
        let delta = &v["event"]["delta"];
        let text = match (v["type"].as_str(), delta["type"].as_str()) {
            (Some("stream_event"), Some("text_delta")) => delta["text"].as_str().unwrap_or(""),
            (Some("stream_event"), Some("input_json_delta")) => delta["partial_json"].as_str().unwrap_or(""),
            (Some("result"), _) => {
                result = Some(v.clone());
                ""
            }
            _ => "",
        };
        streamed |= !text.is_empty();
        if !on_text(text.into()) {
            let _ = child.kill();
            return Ok(());
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err.join().unwrap_or_default();
    let Some(r) = result else { return Err(failed(program, status, &err)) };
    let reply = r["result"].as_str().unwrap_or_default();
    if r["is_error"].as_bool().unwrap_or(false) {
        return Err(format!("Claude Code: {}", if reply.is_empty() { err.trim() } else { reply }));
    }
    if !streamed {
        let text = if schema.is_some() && r["structured_output"].is_object() { r["structured_output"].to_string() } else { reply.to_owned() };
        on_text(text);
    }
    Ok(())
}

fn codex(program: &str, dir: &Path, model: Option<&str>, prompt: &str, schema: Option<&serde_json::Value>, on_text: &mut dyn FnMut(String) -> bool) -> Res<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = |what: &str| -> PathBuf { dir.join(format!("codex-{n}-{what}")) };
    let (last, schema_file) = (file("reply.txt"), file("schema.json"));
    let mut args: Vec<String> = vec!["exec".into(), "--skip-git-repo-check".into(), "--sandbox".into(), "read-only".into(), "--json".into(), "-o".into(), last.to_string_lossy().into_owned()];
    if let Some(m) = model {
        args.extend(["-m".into(), m.into()]);
    }
    if let Some(s) = schema {
        std::fs::write(&schema_file, s.to_string()).map_err(|e| e.to_string())?;
        args.extend(["--output-schema".into(), schema_file.to_string_lossy().into_owned()]);
    }
    args.push("-".into());
    let mut child = spawn(program, dir, &args)?;
    feed(&mut child, prompt.to_owned());
    let err = stderr_of(&mut child);
    let mut problem = None;
    for line in BufReader::new(child.stdout.take().ok_or("no output")?).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            let msg = v["error"]["message"].as_str().or(v["message"].as_str());
            if matches!(v["type"].as_str(), Some("error" | "turn.failed")) {
                problem = msg.map(str::to_owned).or(problem);
            }
        }
        if !on_text(String::new()) {
            let _ = child.kill();
            return Ok(());
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err.join().unwrap_or_default();
    let reply = std::fs::read_to_string(&last).unwrap_or_default();
    let _ = (std::fs::remove_file(&last), std::fs::remove_file(&schema_file));
    if !status.success() || reply.trim().is_empty() {
        return Err(problem.map_or_else(|| failed(program, status, &err), |p| format!("Codex: {p}")));
    }
    on_text(reply);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fake(name: &str, script: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("glasir-fake-cli-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn ask(p: Provider, program: &Path, schema: Option<&serde_json::Value>) -> Res<String> {
        let ep = Endpoint { model: "haiku".into(), base_url: program.to_string_lossy().into_owned(), saved_in: None };
        let mut out = String::new();
        complete(p, &ep, "SYS", "PROMPT", schema, &mut |t| {
            out += &t;
            true
        })?;
        Ok(out)
    }

    #[test]
    fn claude_code_streams_text_and_passes_its_flags() {
        let argv = std::env::temp_dir().join(format!("glasir-fake-claude-argv-{}", std::process::id()));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > /dev/null\necho '{{\"type\":\"system\",\"subtype\":\"init\"}}'\necho '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"Fix \"}}}}}}'\necho '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"parser\"}}}}}}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"Fix parser\"}}'\n",
            argv.display()
        );
        let p = fake("claude", &script);
        assert_eq!(ask(Provider::ClaudeCode, &p, None).unwrap(), "Fix parser");
        let args = std::fs::read_to_string(&argv).unwrap();
        for a in ["-p", "stream-json", "--no-session-persistence", "--strict-mcp-config", "SYS", "haiku"] {
            assert!(args.lines().any(|l| l == a), "{a} in {args}");
        }
        let _ = std::fs::remove_file(argv);
    }

    #[test]
    fn claude_code_reports_errors_and_structured_output() {
        let p = fake("claude", "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"Not logged in\"}'\n");
        assert!(ask(Provider::ClaudeCode, &p, None).is_err_and(|e| e.contains("Not logged in")));
        let p = fake("claude", "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"x\",\"structured_output\":{\"a\":1}}'\n");
        assert_eq!(ask(Provider::ClaudeCode, &p, Some(&serde_json::json!({"type": "object"}))).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn codex_reads_the_last_message_and_gets_the_schema_file() {
        let script = "#!/bin/sh\nout=''; schema=''\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; --output-schema) schema=\"$2\"; shift;; esac; shift; done\nprompt=$(cat)\ncase \"$prompt\" in *SYS*PROMPT*) ;; *) exit 3;; esac\n[ -n \"$schema\" ] && [ -s \"$schema\" ] || exit 4\necho '{\"type\":\"turn.started\"}'\nprintf '{\"a\":2}' > \"$out\"\n";
        let p = fake("codex", script);
        assert_eq!(ask(Provider::Codex, &p, Some(&serde_json::json!({"type": "object"}))).unwrap(), r#"{"a":2}"#);
        let p = fake("codex", "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"turn.failed\",\"error\":{\"message\":\"not signed in\"}}'\nexit 1\n");
        assert!(ask(Provider::Codex, &p, None).is_err_and(|e| e.contains("not signed in")));
    }

    #[test]
    fn a_missing_cli_says_how_to_fix_it() {
        let p = Path::new("/nonexistent/glasir-no-such-cli");
        assert!(ask(Provider::ClaudeCode, p, None).is_err_and(|e| e.contains("Settings → AI")));
    }
}
