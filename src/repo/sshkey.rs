use super::Op;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct KeyInfo {
    pub path: String,
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, PartialEq)]
pub enum Active {
    Default,
    Ours(String),
    Custom(String),
}

const SUFFIX: &str = "' -o IdentitiesOnly=yes";
const PREFIX: &str = "ssh -i '";

fn slashed(path: &str) -> String {
    if cfg!(windows) { path.replace('\\', "/") } else { path.to_owned() }
}

pub fn build(path: &str) -> String {
    format!("{PREFIX}{}{SUFFIX}", slashed(path).replace('\'', "'\\''"))
}

pub fn parse(cmd: Option<&str>) -> Active {
    match cmd.map(str::trim).filter(|c| !c.is_empty()) {
        None | Some("ssh") => Active::Default,
        Some(c) => match c.strip_prefix(PREFIX).and_then(|r| r.strip_suffix(SUFFIX)) {
            Some(q) if q.replace("'\\''", "").find('\'').is_none() => Active::Ours(q.replace("'\\''", "'")),
            _ => Active::Custom(c.to_owned()),
        },
    }
}

pub fn set_op(key: Option<&str>, previous: Option<&str>, custom: Option<&str>, inherited: bool) -> Op {
    let restore = |cmd: Option<&str>| match cmd {
        Some(c) => Op::new("Restore SSH key setting", &["config", "core.sshCommand", c]),
        None => Op::new("Restore SSH key setting", &["config", "--unset-all", "core.sshCommand"]),
    };
    let op = match key {
        Some(k) => {
            let name = Path::new(k).file_name().map_or(k.into(), |n| n.to_string_lossy());
            Op::new(format!("Use SSH key {name} for this repository"), &["config", "core.sshCommand", &build(k)])
        }
        None if inherited => Op::new("Use the default SSH key for this repository", &["config", "core.sshCommand", "ssh"]),
        None => Op::new("Use the default SSH key for this repository", &["config", "--unset-all", "core.sshCommand"]),
    };
    let op = op.revert(restore(previous));
    match custom {
        Some(c) => op.confirm(format!("This repository already sets core.sshCommand to:\n\n{}\n\nReplace it?", crate::git::redact_command(c))),
        None => op,
    }
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn quiet(prog: &str) -> Command {
    let mut c = Command::new(prog);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

pub fn list() -> Vec<KeyInfo> {
    let Some(dir) = home().map(|h| h.join(".ssh")) else { return vec![] };
    let mut v: Vec<KeyInfo> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let (p, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
            let public = dir.join(format!("{name}.pub"));
            (!name.ends_with(".pub") && public.is_file() && p.is_file()).then(|| {
                let out = crate::git::spawn(quiet("ssh-keygen").arg("-lf").arg(&public).stdout(Stdio::piped()).stderr(Stdio::null())).and_then(|c| c.wait_with_output()).ok().filter(|o| o.status.success());
                let line = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
                let fingerprint = line.split_once(' ').map_or(line.clone(), |(_, r)| r.to_owned());
                KeyInfo { path: slashed(&p.to_string_lossy()), name, fingerprint }
            })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub fn is_ssh(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && url.split_once(':').is_some_and(|(h, _)| h.len() > 1 && !h.contains(['/', '\\'])))
}

pub fn test(dir: &Path, key: Option<&str>, url: &str) -> (bool, String) {
    if !is_ssh(url) {
        return (false, "Not an SSH remote".into());
    }
    match crate::git::ls_remote_via(dir, url, key.map(build).as_deref()) {
        Ok(_) => (true, "Authenticated: git could reach the remote over SSH".into()),
        Err(e) => (false, e.trim().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_parses_the_command() {
        assert_eq!(build("/home/me/.ssh/id_ed25519"), "ssh -i '/home/me/.ssh/id_ed25519' -o IdentitiesOnly=yes");
        for p in ["/home/me/.ssh/id", "/home/my keys/id", "/home/o'brien/id", "/a/''/b"] {
            assert_eq!(parse(Some(&build(p))), Active::Ours(p.into()), "{p}");
        }
        assert_eq!(parse(None), Active::Default);
        assert_eq!(parse(Some("  ")), Active::Default);
        for c in ["ssh -i ~/.ssh/k", "ssh -i '/a' -o IdentitiesOnly=yes -o Foo=1", "ssh -i '/a'b' -o IdentitiesOnly=yes", "plink -i k"] {
            assert_eq!(parse(Some(c)), Active::Custom(c.into()), "{c}");
        }
    }

    #[test]
    fn the_op_sets_and_restores() {
        let op = set_op(Some("/k/id"), Some("ssh -i x"), None, false);
        assert_eq!(op.cmds[0], ["config", "core.sshCommand", "ssh -i '/k/id' -o IdentitiesOnly=yes"]);
        assert_eq!(op.revert.as_ref().unwrap().cmds[0], ["config", "core.sshCommand", "ssh -i x"]);
        assert!(op.confirm.is_none());
        let op = set_op(None, None, Some("ssh -i x"), false);
        assert_eq!(op.cmds[0], ["config", "--unset-all", "core.sshCommand"]);
        assert!(op.confirm.unwrap().contains("ssh -i x"));
        let asked = set_op(None, None, Some("sshpass -p hunter2 ssh -i x"), false).confirm.unwrap();
        assert!(asked.contains("sshpass -p *** ssh -i x") && !asked.contains("hunter2"));
        let op = set_op(None, None, None, true);
        assert_eq!(op.cmds[0], ["config", "core.sshCommand", "ssh"]);
        assert_eq!(op.revert.as_ref().unwrap().cmds[0], ["config", "--unset-all", "core.sshCommand"]);
        assert_eq!(parse(Some("ssh")), Active::Default);
    }
}
