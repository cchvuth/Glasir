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

pub fn build(path: &str) -> String {
    let p = if cfg!(windows) { path.replace('\\', "/") } else { path.to_owned() };
    format!("{PREFIX}{}{SUFFIX}", p.replace('\'', "'\\''"))
}

pub fn parse(cmd: Option<&str>) -> Active {
    match cmd.map(str::trim).filter(|c| !c.is_empty()) {
        None => Active::Default,
        Some(c) => match c.strip_prefix(PREFIX).and_then(|r| r.strip_suffix(SUFFIX)) {
            Some(q) if q.replace("'\\''", "").find('\'').is_none() => Active::Ours(q.replace("'\\''", "'")),
            _ => Active::Custom(c.to_owned()),
        },
    }
}

pub fn redact_command(cmd: &str) -> String {
    let cmd = crate::git::redact(cmd);
    let secret = |key: &str| {
        let key = key.to_ascii_lowercase();
        ["pass", "password", "passwd", "pwd", "token", "secret"].iter().any(|s| key.ends_with(s))
    };
    let (mut words, mut hide) = (shell_words(&cmd).into_iter().peekable(), vec![]);
    while let Some((start, end)) = words.next() {
        let word = &cmd[start..end];
        if let Some((key, _)) = word.split_once('=')
            && secret(key)
        {
            hide.push((start + key.len() + 1, end));
        }
        if word == "-pw" {
            hide.extend(words.next());
        }
        if word.rsplit('/').next() == Some("sshpass") {
            while let Some((start, end)) = words.next_if(|&(start, _)| cmd[start..].starts_with('-')) {
                let Some(at) = cmd[start + 1..end].find(['f', 'd', 'p', 'P']).map(|i| start + 1 + i) else { continue };
                let arg = if at + 1 < end { Some((at + 1, end)) } else { words.next() };
                if cmd[at..].starts_with('p') {
                    hide.extend(arg);
                }
            }
        }
    }
    let (mut out, mut from) = (String::new(), 0);
    for (start, end) in hide {
        out += &cmd[from..start];
        out += "***";
        from = end;
    }
    out + &cmd[from..]
}

fn shell_words(s: &str) -> Vec<(usize, usize)> {
    let (mut words, mut start, mut quote, mut escaped) = (vec![], None, None, false);
    for (i, c) in s.char_indices() {
        match (quote, c) {
            _ if escaped => escaped = false,
            (None | Some('"'), '\\') => escaped = true,
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, _) if c.is_whitespace() => {
                words.extend(start.take().map(|from| (from, i)));
                continue;
            }
            _ => {}
        }
        start.get_or_insert(i);
    }
    words.extend(start.map(|from| (from, s.len())));
    words
}

pub fn set_op(key: Option<&str>, previous: Option<&str>, custom: Option<&str>) -> Op {
    let restore = |cmd: Option<&str>| match cmd {
        Some(c) => Op::new("Restore SSH key setting", &["config", "core.sshCommand", c]),
        None => Op::new("Restore SSH key setting", &["config", "--unset-all", "core.sshCommand"]),
    };
    let op = match key {
        Some(k) => {
            let name = Path::new(k).file_name().map_or(k.into(), |n| n.to_string_lossy());
            Op::new(format!("Use SSH key {name} for this repository"), &["config", "core.sshCommand", &build(k)])
        }
        None => Op::new("Use the default SSH key for this repository", &["config", "--unset-all", "core.sshCommand"]),
    };
    let op = op.revert(restore(previous));
    match custom {
        Some(c) => op.confirm(format!("This repository already sets core.sshCommand to:\n\n{}\n\nReplace it?", redact_command(c))),
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
                let out = quiet("ssh-keygen").arg("-lf").arg(&public).output().ok().filter(|o| o.status.success());
                let line = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
                let fingerprint = line.split_once(' ').map_or(line.clone(), |(_, r)| r.to_owned());
                KeyInfo { path: p.to_string_lossy().into_owned(), name, fingerprint }
            })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub fn is_ssh(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && url.split_once(':').is_some_and(|(h, _)| h.len() > 1 && !h.contains(['/', '\\'])))
}

pub fn target(url: &str) -> Option<(String, String, Option<u16>)> {
    if !is_ssh(url) {
        return None;
    }
    let (auth, port) = match url.strip_prefix("ssh://") {
        Some(r) => {
            let a = r.split('/').next()?;
            let (a, port) = match a.rsplit_once(':') {
                Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse().ok()),
                _ => (a, None),
            };
            (a, port)
        }
        None => (url.split_once(':')?.0, None),
    };
    let (user, host) = auth.split_once('@').unwrap_or(("git", auth));
    Some((user.into(), host.into(), port))
}

pub fn test(key: Option<&str>, url: &str) -> (bool, String) {
    let Some((user, host, port)) = target(url) else { return (false, "Not an SSH remote".into()) };
    let mut c = quiet("ssh");
    c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    if let Some(k) = key {
        c.args(["-i", k, "-o", "IdentitiesOnly=yes"]);
    }
    if let Some(p) = port {
        c.args(["-p", &p.to_string()]);
    }
    match c.arg(format!("{user}@{host}")).output() {
        Ok(o) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_owned();
            (o.status.success() || text.to_lowercase().contains("successfully authenticated"), text)
        }
        Err(e) => (false, format!("Could not run ssh: {e}")),
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
    fn hides_passwords_in_an_ssh_command() {
        for (cmd, shown) in [
            ("sshpass -p secret ssh -p 2222 git@h", "sshpass -p *** ssh -p 2222 git@h"),
            ("sshpass -psecret ssh -p 2222 git@h", "sshpass -p*** ssh -p 2222 git@h"),
            ("/usr/bin/sshpass -v -p 'two words' ssh -p 22 h", "/usr/bin/sshpass -v -p *** ssh -p 22 h"),
            ("sshpass -P pass -p \"a \\\" b\" ssh h", "sshpass -P pass -p *** ssh h"),
            ("sshpass -vpsecret ssh h", "sshpass -vp*** ssh h"),
            ("sshpass -p 'unclosed ssh h", "sshpass -p ***"),
            ("SSHPASS=hunter2 sshpass -e ssh h", "SSHPASS=*** sshpass -e ssh h"),
            ("env GH_TOKEN=ghp_x ssh --password='a b' h", "env GH_TOKEN=*** ssh --password=*** h"),
            ("plink -batch -pw hunter2 -i k.ppk", "plink -batch -pw *** -i k.ppk"),
            ("ssh -o ProxyCommand='nc -x http://u:pw@px:3128 %h %p' h", "ssh -o ProxyCommand='nc -x http://***@px:3128 %h %p' h"),
        ] {
            assert_eq!(redact_command(cmd), shown, "{cmd}");
        }
        for same in ["ssh -p 2222 -i '/k/id' -o IdentitiesOnly=yes -o PasswordAuthentication=no git@h", "sshpass -f /etc/pw -P assword -d 3 ssh -p 22 h", "sshpass -e ssh -p 22 h", "sshpass -p", "ssh (default)", ""] {
            assert_eq!(redact_command(same), same);
        }
    }

    #[test]
    fn finds_the_ssh_target() {
        assert_eq!(target("git@github.com:o/r.git"), Some(("git".into(), "github.com".into(), None)));
        assert_eq!(target("ssh://me@ghe.corp:2222/o/r"), Some(("me".into(), "ghe.corp".into(), Some(2222))));
        assert_eq!(target("ssh://ghe.corp/o/r"), Some(("git".into(), "ghe.corp".into(), None)));
        assert_eq!(target("https://github.com/o/r"), None);
    }

    #[test]
    fn the_op_sets_and_restores() {
        let op = set_op(Some("/k/id"), Some("ssh -i x"), None);
        assert_eq!(op.cmds[0], ["config", "core.sshCommand", "ssh -i '/k/id' -o IdentitiesOnly=yes"]);
        assert_eq!(op.revert.as_ref().unwrap().cmds[0], ["config", "core.sshCommand", "ssh -i x"]);
        assert!(op.confirm.is_none());
        let op = set_op(None, None, Some("ssh -i x"));
        assert_eq!(op.cmds[0], ["config", "--unset-all", "core.sshCommand"]);
        assert!(op.confirm.unwrap().contains("ssh -i x"));
        let asked = set_op(None, None, Some("sshpass -p hunter2 ssh -i x")).confirm.unwrap();
        assert!(asked.contains("sshpass -p *** ssh -i x") && !asked.contains("hunter2"));
    }
}
