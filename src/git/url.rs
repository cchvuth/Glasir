//! Web URLs: hosting page of a remote (github.com / GitHub Enterprise),
//! percent-encoding for paths and query strings, `#123` autolinks.

/// `git@ghe.corp:org/repo.git` / `https://user@ghe.corp/org/repo` / `ssh://git@ghe.corp:22/org/repo`
/// -> `https://ghe.corp/org/repo`
pub fn web_url(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let (scheme, rest) = match r.split_once("://") {
        Some(("file", _)) => return None,
        Some(("http", rest)) => ("http", rest),
        Some((_, rest)) => ("https", rest),
        None => {
            // scp-like: [user@]host:path
            let (host, path) = r.split_once(':')?;
            let host = host.rsplit('@').next()?;
            // Not a drive letter (`C:/x`) or a path (`./a:b`).
            if host.len() < 2 || host.contains(['/', '\\']) {
                return None;
            }
            let host = if host == "ssh.github.com" { "github.com" } else { host };
            return Some(format!("https://{host}/{}", path.trim_start_matches('/')));
        }
    };
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit('@').next().filter(|h| !h.is_empty())?;
    let host = if scheme == "https" && remote.starts_with("ssh") { host.split(':').next()? } else { host };
    let host = if host == "ssh.github.com" { "github.com" } else { host };
    Some(format!("{scheme}://{host}/{path}"))
}

/// Host of a remote's web page: `git@ghe.corp:o/r` -> `ghe.corp` (no port).
pub fn host(remote: &str) -> Option<String> {
    let w = web_url(remote)?;
    let h = w.split_once("://")?.1.split('/').next()?;
    Some(h.split(':').next()?.to_lowercase())
}

/// A page of the hosting site (github.com / GitHub Enterprise); `url` joins it to a repo's web URL.
pub enum Page<'a> {
    Commit(&'a str),
    Branch(&'a str),
    Release(&'a str),
    /// A file at a revision, with an optional 1-based line range.
    File { rev: &'a str, path: &'a str, lines: Option<(u32, u32)> },
    Blame { rev: &'a str, path: &'a str },
    History { rev: &'a str, path: &'a str },
    Compare { base: &'a str, head: &'a str },
}

impl Page<'_> {
    pub fn url(&self, web: &str) -> String {
        let e = url_path;
        match self {
            Page::Commit(s) => format!("{web}/commit/{}", e(s)),
            Page::Branch(b) => format!("{web}/tree/{}", e(b)),
            Page::Release(t) => format!("{web}/releases/tag/{}", e(t)),
            Page::File { rev, path, lines } => {
                let mut u = format!("{web}/blob/{}/{}", e(rev), e(path));
                if let Some((a, b)) = lines {
                    // Rendered Markdown has no line anchors: `?plain=1` shows the source.
                    if path.rsplit('.').next().is_some_and(|x| ["md", "markdown", "mdx"].contains(&x.to_lowercase().as_str())) {
                        u += "?plain=1";
                    }
                    u += &if a == b { format!("#L{a}") } else { format!("#L{a}-L{b}") };
                }
                u
            }
            Page::Blame { rev, path } => format!("{web}/blame/{}/{}", e(rev), e(path)),
            Page::History { rev, path } => format!("{web}/commits/{}/{}", e(rev), e(path)),
            Page::Compare { base, head } => format!("{web}/compare/{}...{}", e(base), e(head)),
        }
    }
}

/// `url` is plain http(s) to one of the `hosts` (as `host` returns them), without user info:
/// the only links handed to the OS opener.
pub fn is_web_link(url: &str, hosts: &[String]) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = auth.split(':').next().unwrap_or_default().to_lowercase();
    matches!(scheme, "http" | "https")
        && !host.is_empty()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        && !auth.contains('@')
        && hosts.contains(&host)
}

/// Splits text into plain runs and `#123` / `GH-123` references, given as
/// `(text, Some(number))`. GitHub redirects `/pull/N` to the issue when N is one.
pub fn autolinks(text: &str) -> Vec<(&str, Option<&str>)> {
    let (b, mut out, mut plain) = (text.as_bytes(), vec![], 0);
    let mut i = 0;
    while i < b.len() {
        let pre = if b[i] == b'#' { 1 } else if b[i..].starts_with(b"GH-") { 3 } else { 0 };
        let digits = b[(i + pre).min(b.len())..].iter().take_while(|c| c.is_ascii_digit()).count();
        let end = i + pre + digits;
        // Word boundaries on both sides; `&#123;` is an HTML entity, `a/#1` a URL fragment.
        let left = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || matches!(b[i - 1], b'&' | b'/' | b'_' | b'-'));
        let right = b.get(end).is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'));
        if pre > 0 && digits > 0 && left && right {
            if plain < i {
                out.push((&text[plain..i], None));
            }
            out.push((&text[i..end], Some(&text[i + pre..end])));
            plain = end;
            i = end;
        } else {
            i += 1;
        }
    }
    if plain < text.len() {
        out.push((&text[plain..], None));
    }
    out
}

/// Percent-encode a branch name for a URL path, keeping `/` separators.
pub fn url_path(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `https://…?k=v` query-string encoding.
pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `s` with the user info of each `scheme://user[:secret]@host` hidden as `***@host`
/// (a token can be the user name); ssh's plain `ssh://git@host` stays.
pub fn redact(s: &str) -> String {
    let (mut out, mut rest) = (String::new(), s);
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out += head;
        let auth = &tail[..tail.find(|c: char| c == '/' || c.is_whitespace()).unwrap_or(tail.len())];
        rest = match auth.rfind('@') {
            Some(at) if !head.ends_with("ssh://") || auth[..at].contains(':') => {
                out += "***";
                &tail[at..]
            }
            _ => tail,
        };
    }
    out + rest
}

pub fn redact_command(cmd: &str) -> String {
    let cmd = redact(cmd);
    let mut hide = vec![];
    secrets(&cmd, 0, &mut hide);
    let (mut out, mut from) = (String::new(), 0);
    for (start, end) in hide {
        out += &cmd[from..start];
        out += "***";
        from = end;
    }
    out + &cmd[from..]
}

fn secrets(cmd: &str, at: usize, hide: &mut Vec<(usize, usize)>) {
    let secret = |key: &str| {
        let key = key.to_ascii_lowercase();
        ["pass", "password", "passwd", "pwd", "token", "secret"].iter().any(|s| key.ends_with(s))
    };
    let mut words = shell_words(cmd).into_iter().peekable();
    while let Some((start, end)) = words.next() {
        let word = &cmd[start..end];
        let eq = word.find('=');
        match eq {
            Some(i) if secret(&word[..i]) => hide.push((at + start + i + 1, at + end)),
            _ => {
                let v = eq.map_or(start, |i| start + i + 1);
                let quoted = |q: u8| cmd.as_bytes()[v] == q && cmd.as_bytes()[end - 1] == q;
                let (s, e) = if end - v > 1 && (quoted(b'\'') || quoted(b'"')) { (v + 1, end - 1) } else { (v, end) };
                if (s, e) != (start, end) && cmd[s..e].contains(char::is_whitespace) {
                    secrets(&cmd[s..e], at + s, hide);
                }
            }
        }
        if word == "-pw" {
            hide.extend(words.next().map(|(s, e)| (at + s, at + e)));
        }
        if word.rsplit('/').next() == Some("sshpass") {
            while let Some((start, end)) = words.next_if(|&(start, _)| cmd[start..].starts_with('-')) {
                let Some(p) = cmd[start + 1..end].find(['f', 'd', 'p', 'P']).map(|i| start + 1 + i) else { continue };
                let arg = if p + 1 < end { Some((p + 1, end)) } else { words.next() };
                if cmd[p..].starts_with('p') {
                    hide.extend(arg.map(|(s, e)| (at + s, at + e)));
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls() {
        for (i, o) in [
            ("git@github.com:org/repo.git", "https://github.com/org/repo"),
            ("https://ghe.corp.se/org/repo.git", "https://ghe.corp.se/org/repo"),
            ("https://me@ghe.corp.se/org/repo", "https://ghe.corp.se/org/repo"),
            ("ssh://git@ghe.corp.se:7999/org/repo.git", "https://ghe.corp.se/org/repo"),
            ("http://local:3000/a/b", "http://local:3000/a/b"),
        ] {
            assert_eq!(web_url(i).as_deref(), Some(o), "{i}");
        }
    }

    #[test]
    fn local_remotes_have_no_web_page() {
        for i in ["file:///srv/x.git", "C:/repos/x.git", "C:\\repos\\x.git", "/srv/x.git", "./a:b"] {
            assert_eq!(web_url(i), None, "{i}");
        }
        assert!(!is_web_link("https:///srv/x/commit/a", &[String::new()]));
    }

    #[test]
    fn pages() {
        for web in ["https://github.com/o/r", "https://ghe.corp.se/o/r"] {
            let u = |p: Page| p.url(web);
            assert_eq!(u(Page::Commit("abc")), format!("{web}/commit/abc"));
            assert_eq!(u(Page::Branch("feat/a b")), format!("{web}/tree/feat/a%20b"));
            assert_eq!(u(Page::Release("v1.0")), format!("{web}/releases/tag/v1.0"));
            assert_eq!(u(Page::Blame { rev: "abc", path: "src/é #1.rs" }), format!("{web}/blame/abc/src/%C3%A9%20%231.rs"));
            assert_eq!(u(Page::History { rev: "abc", path: "a.rs" }), format!("{web}/commits/abc/a.rs"));
            assert_eq!(u(Page::Compare { base: "a", head: "b" }), format!("{web}/compare/a...b"));
            assert_eq!(u(Page::File { rev: "abc", path: "a.rs", lines: None }), format!("{web}/blob/abc/a.rs"));
            assert_eq!(u(Page::File { rev: "abc", path: "a.rs", lines: Some((10, 20)) }), format!("{web}/blob/abc/a.rs#L10-L20"));
            assert_eq!(u(Page::File { rev: "abc", path: "a.rs", lines: Some((7, 7)) }), format!("{web}/blob/abc/a.rs#L7"));
            assert_eq!(u(Page::File { rev: "abc", path: "docs/README.MD", lines: Some((1, 2)) }), format!("{web}/blob/abc/docs/README.MD?plain=1#L1-L2"));
            assert_eq!(u(Page::File { rev: "abc", path: "README.md", lines: None }), format!("{web}/blob/abc/README.md"));
        }
    }

    #[test]
    fn opens_only_web_links_on_the_remote_host() {
        let hosts = vec!["ghe.corp.se".to_owned(), "github.com".to_owned()];
        for ok in ["https://github.com/o/r", "http://GHE.corp.se:8443/o/r?x#L1"] {
            assert!(is_web_link(ok, &hosts), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "javascript://github.com/%0aalert(1)",
            "ssh://github.com/o/r",
            "https://github.com.evil.com/o/r",
            "https://github.com@evil.com/o/r",
            "https://evil.com/github.com",
            "https://github.com\\@evil.com/",
            "https://github.com/a b",
            "github.com/o/r",
            "",
        ] {
            assert!(!is_web_link(bad, &hosts), "{bad}");
        }
    }

    #[test]
    fn hosts() {
        assert_eq!(host("ssh://git@GHE.corp.se:7999/o/r.git").as_deref(), Some("ghe.corp.se"));
        assert_eq!(host("git@github.com:o/r").as_deref(), Some("github.com"));
        assert_eq!(host("http://local:3000/a/b").as_deref(), Some("local"));
        assert_eq!(host("/srv/repo.git"), None);
    }

    #[test]
    fn finds_references() {
        assert_eq!(autolinks("Fix crash (#12)"), [("Fix crash (", None), ("#12", Some("12")), (")", None)]);
        assert_eq!(autolinks("#7 and GH-8."), [("#7", Some("7")), (" and ", None), ("GH-8", Some("8")), (".", None)]);
        for plain in ["issue#5", "&#123;", "see a/#3", "#12ab", "# 4", "#", "C#", "vGH-1", "é#"] {
            assert_eq!(autolinks(plain), [(plain, None)], "{plain}");
        }
        assert_eq!(autolinks("é #9"), [("é ", None), ("#9", Some("9"))]);
        assert!(autolinks("").is_empty());
    }

    #[test]
    fn redacts_credentials() {
        assert_eq!(redact("https://chan:ghp_X@ghe.corp:9/a/x.git"), "https://***@ghe.corp:9/a/x.git");
        assert_eq!(redact("http://u:p@proxy:8080 and https://ghp_Y@github.com"), "http://***@proxy:8080 and https://***@github.com");
        for keep in ["ssh://git@ghe.corp/o/r", "git@ghe.corp:o/r", "https://ghe.corp/a@b", "localhost,.corp"] {
            assert_eq!(redact(keep), keep);
        }
        assert_eq!(redact("ssh://git:pw@h/r"), "ssh://***@h/r");
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
            ("ssh -o ProxyCommand='sshpass -p hunter2 ssh -W %h:%p jump' git@h", "ssh -o ProxyCommand='sshpass -p *** ssh -W %h:%p jump' git@h"),
            ("ssh -o 'ProxyCommand=sshpass -p hunter2 ssh -W %h:%p jump' git@h", "ssh -o 'ProxyCommand=sshpass -p *** ssh -W %h:%p jump' git@h"),
            ("ssh -o \"ProxyCommand plink -pw hunter2 jump\" h", "ssh -o \"ProxyCommand plink -pw *** jump\" h"),
            ("sh -c 'sshpass -phunter2 ssh \"$@\"' ssh h", "sh -c 'sshpass -p*** ssh \"$@\"' ssh h"),
        ] {
            assert_eq!(redact_command(cmd), shown, "{cmd}");
        }
        for same in ["ssh -p 2222 -i '/k/id' -o IdentitiesOnly=yes -o PasswordAuthentication=no git@h", "sshpass -f /etc/pw -P assword -d 3 ssh -p 22 h", "sshpass -e ssh -p 22 h", "sshpass -p", "ssh (default)", "", "ssh -o 'ProxyJump=a b' -i \"C:\\Program Files\\k\" ''"] {
            assert_eq!(redact_command(same), same);
        }
    }

    #[test]
    fn encodes_urls() {
        assert_eq!(url_encode("Fix: a&b é"), "Fix%3A%20a%26b%20%C3%A9");
    }
}
