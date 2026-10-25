//! AI through a coding-agent CLI the user is already signed in to (Claude Code, Codex),
//! instead of an API key: the CLI runs with no tools, in a fresh private folder, once per request.

use super::client::{Endpoint, Provider};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

type Res<T> = Result<T, String>;

pub const DEFAULT_MODEL: &str = "default";
pub const CLAUDE_MODELS: [&str; 4] = [DEFAULT_MODEL, "opus", "sonnet", "haiku"];
pub const CODEX_MODELS: [&str; 1] = [DEFAULT_MODEL];

const SILENCE: Duration = Duration::from_secs(if cfg!(test) { 10 } else { 180 });

pub fn endpoint(p: Provider) -> Endpoint {
    Endpoint { model: DEFAULT_MODEL.into(), base_url: if p == Provider::Codex { "codex" } else { "claude" }.into(), saved_in: None }
}

pub(super) fn complete(p: Provider, ep: &Endpoint, system: &str, prompt: &str, schema: Option<&serde_json::Value>, on_text: &mut dyn FnMut(String) -> bool) -> Res<()> {
    let dir = private_dir()?;
    let model = Some(ep.model.trim()).filter(|m| !m.is_empty() && *m != DEFAULT_MODEL);
    let program = Some(ep.base_url.trim()).filter(|c| !c.is_empty()).unwrap_or(if p == Provider::Codex { "codex" } else { "claude" });
    let res = match p {
        Provider::Codex => codex(program, &dir, model, &format!("{system}\n\n{prompt}"), schema, on_text),
        _ => claude(program, &dir, model, system, prompt, schema, on_text),
    };
    let _ = std::fs::remove_dir_all(&dir);
    res
}

fn private_dir() -> Res<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let dir = std::env::temp_dir().join(format!("glasir-ai-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let b = &mut std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(b, 0o700);
        match b.create(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            r => return r.map(|_| dir.clone()).map_err(|e| format!("{}: {e}", dir.display())),
        }
    }
}

fn spawn(program: &str, dir: &Path, args: &[String]) -> Res<Child> {
    let run = |exe: &str| {
        let mut c = Command::new(exe);
        c.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::git::own_group(&mut c);
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

fn stop(child: &mut Child) {
    crate::git::kill_tree(child.id());
    let _ = child.wait();
}

fn read(program: &str, child: &mut Child, on_text: &mut dyn FnMut(String) -> bool, mut each: impl FnMut(&serde_json::Value) -> Res<String>) -> Res<bool> {
    let out = child.stdout.take().ok_or("no output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || BufReader::new(out).split(b'\n').map_while(Result::ok).try_for_each(|l| tx.send(l)));
    let (start, mut heard) = (Instant::now(), false);
    loop {
        let text = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                heard = true;
                match serde_json::from_slice(&line).map(|v| each(&v)) {
                    Ok(Err(e)) => {
                        stop(child);
                        return Err(e);
                    }
                    r => r.ok().and_then(Result::ok).unwrap_or_default(),
                }
            }
            Err(RecvTimeoutError::Timeout) if !heard && start.elapsed() > SILENCE => {
                stop(child);
                return Err(format!("{program} did not answer in {} s", SILENCE.as_secs()));
            }
            Err(RecvTimeoutError::Timeout) => String::new(),
            Err(RecvTimeoutError::Disconnected) => return Ok(true),
        };
        if !on_text(text) {
            stop(child);
            return Ok(false);
        }
    }
}

fn failed(program: &str, status: std::process::ExitStatus, err: &str) -> String {
    let err = err.trim();
    if err.is_empty() { format!("{program} exited with {status}") } else { format!("{program}: {err}") }
}

fn claude(program: &str, dir: &Path, model: Option<&str>, system: &str, prompt: &str, schema: Option<&serde_json::Value>, on_text: &mut dyn FnMut(String) -> bool) -> Res<()> {
    let system_file = dir.join("system.txt");
    std::fs::write(&system_file, system).map_err(|e| e.to_string())?;
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--tools", "", "--no-session-persistence", "--strict-mcp-config", "--setting-sources", "user", "--system-prompt-file"]
        .map(String::from)
        .to_vec();
    args.push(system_file.to_string_lossy().into_owned());
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
    let done = read(program, &mut child, on_text, |v| {
        let delta = &v["event"]["delta"];
        if matches!(delta["stop_reason"].as_str(), Some("max_tokens" | "model_context_window_exceeded")) {
            return Err("The answer was cut off.".into());
        }
        let text = match (v["type"].as_str(), delta["type"].as_str()) {
            (Some("stream_event"), Some("text_delta")) if schema.is_none() => delta["text"].as_str().unwrap_or(""),
            (Some("result"), _) => {
                result = Some(v.clone());
                ""
            }
            _ => "",
        };
        streamed |= !text.is_empty();
        Ok(text.to_owned())
    })?;
    if !done {
        return Ok(());
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
    let (last, schema_file) = (dir.join("reply.txt"), dir.join("schema.json"));
    let mut args: Vec<String> = vec!["exec".into(), "--skip-git-repo-check".into(), "--sandbox".into(), "read-only".into(), "-c".into(), "mcp_servers={}".into(), "--json".into(), "-o".into(), last.to_string_lossy().into_owned()];
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
    let done = read(program, &mut child, on_text, |v| {
        let msg = v["error"]["message"].as_str().or(v["message"].as_str());
        if matches!(v["type"].as_str(), Some("error" | "turn.failed")) {
            problem = msg.map(str::to_owned).or(problem.take());
        }
        Ok(String::new())
    })?;
    if !done {
        return Ok(());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err.join().unwrap_or_default();
    let reply = std::fs::read_to_string(&last).unwrap_or_default();
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

    fn scratch(what: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("glasir-fake-{what}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn claude_code_streams_text_and_passes_its_flags() {
        let (argv, sys) = (scratch("claude-argv"), scratch("claude-system"));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nwhile [ $# -gt 0 ]; do [ \"$1\" = --system-prompt-file ] && cat \"$2\" > '{}'; shift; done\ncat > /dev/null\necho '{{\"type\":\"system\",\"subtype\":\"init\"}}'\necho '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"Fix \"}}}}}}'\necho '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"parser\"}}}}}}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"Fix parser\"}}'\n",
            argv.display(),
            sys.display()
        );
        let p = fake("claude", &script);
        assert_eq!(ask(Provider::ClaudeCode, &p, None).unwrap(), "Fix parser");
        let args = std::fs::read_to_string(&argv).unwrap();
        for a in ["-p", "stream-json", "--no-session-persistence", "--strict-mcp-config", "user", "haiku"] {
            assert!(args.lines().any(|l| l == a), "{a} in {args}");
        }
        assert!(!args.lines().any(|l| l == "SYS"), "{args}");
        assert_eq!(std::fs::read_to_string(&sys).unwrap(), "SYS");
        let _ = (std::fs::remove_file(argv), std::fs::remove_file(sys));
    }

    #[test]
    fn claude_code_reports_errors_and_structured_output() {
        let p = fake("claude", "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"Not logged in\"}'\n");
        assert!(ask(Provider::ClaudeCode, &p, None).is_err_and(|e| e.contains("Not logged in")));
        let p = fake("claude", "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"x\",\"structured_output\":{\"a\":1}}'\n");
        assert_eq!(ask(Provider::ClaudeCode, &p, Some(&serde_json::json!({"type": "object"}))).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn claude_code_schema_reply_is_only_the_structured_output() {
        let ev = |d: &str| format!("echo '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{d}}}}}'\n");
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\n{}{}{}echo '{{\"type\":\"result\",\"is_error\":false,\"result\":\"\",\"structured_output\":{{\"commits\":[]}}}}'\n",
            ev(r#"{"type":"text_delta","text":"Here is the plan."}"#),
            ev(r#"{"type":"input_json_delta","partial_json":"{\"commits\":"}"#),
            ev(r#"{"type":"input_json_delta","partial_json":"[]}"}"#)
        );
        let p = fake("claude", &script);
        assert_eq!(ask(Provider::ClaudeCode, &p, Some(&serde_json::json!({"type": "object"}))).unwrap(), r#"{"commits":[]}"#);
    }

    #[test]
    fn claude_code_refuses_a_reply_cut_off_at_the_output_cap() {
        let script = "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"74\\n75\"}}}'\necho '{\"type\":\"stream_event\",\"event\":{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}}'\necho '{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"76\\n77\"}}}'\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"76\\n77\"}'\n";
        let p = fake("claude", script);
        assert!(ask(Provider::ClaudeCode, &p, None).is_err_and(|e| e.contains("cut off")));
    }

    #[test]
    fn runs_in_a_fresh_private_folder_removed_afterwards() {
        let seen = scratch("cli-cwd");
        let script = format!("#!/bin/sh\ncat > /dev/null\n{{ pwd; ls -ld . | cut -c1-10; }} > '{}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\"}}'\n", seen.display());
        let p = fake("claude", &script);
        assert_eq!(ask(Provider::ClaudeCode, &p, None).unwrap(), "ok");
        let out = std::fs::read_to_string(&seen).unwrap();
        let mut lines = out.lines();
        let dir = PathBuf::from(lines.next().unwrap());
        assert_eq!(lines.next(), Some("drwx------"));
        assert!(!dir.exists(), "{} left behind", dir.display());
        let _ = std::fs::remove_file(seen);
    }

    #[test]
    fn cancel_stops_a_silent_cli_and_its_children() {
        let marker = scratch("cli-orphan");
        let script = format!("#!/bin/sh\ncat > /dev/null\nsh -c 'sleep 3; touch {}' &\nsleep 20\n", marker.display());
        let p = fake("codex", &script);
        let ep = Endpoint { model: DEFAULT_MODEL.into(), base_url: p.to_string_lossy().into_owned(), saved_in: None };
        let t = Instant::now();
        assert!(complete(Provider::Codex, &ep, "S", "P", None, &mut |_| false).is_ok());
        assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());
        std::thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists());
    }

    #[test]
    fn a_silent_cli_times_out() {
        let p = fake("claude", "#!/bin/sh\ncat > /dev/null\nsleep 60\n");
        let t = Instant::now();
        assert!(ask(Provider::ClaudeCode, &p, None).is_err_and(|e| e.contains("did not answer")));
        assert!(t.elapsed() < Duration::from_secs(30), "{:?}", t.elapsed());
    }

    #[test]
    fn codex_reads_the_last_message_and_gets_the_schema_file() {
        let script = "#!/bin/sh\nout=''; schema=''; mcp=''\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; --output-schema) schema=\"$2\"; shift;; -c) [ \"$2\" = 'mcp_servers={}' ] && mcp=1; shift;; esac; shift; done\nprompt=$(cat)\ncase \"$prompt\" in *SYS*PROMPT*) ;; *) exit 3;; esac\n[ -n \"$schema\" ] && [ -s \"$schema\" ] || exit 4\n[ -n \"$mcp\" ] || exit 5\necho '{\"type\":\"turn.started\"}'\nprintf '{\"a\":2}' > \"$out\"\n";
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
