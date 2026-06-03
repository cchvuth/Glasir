//! Web URLs: hosting page of a remote (github.com / GitHub Enterprise),
//! percent-encoding for paths and query strings, `#123` autolinks.

/// `git@ghe.corp:org/repo.git` / `https://user@ghe.corp/org/repo` / `ssh://git@ghe.corp:22/org/repo`
/// -> `https://ghe.corp/org/repo`
pub fn web_url(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let (scheme, rest) = match r.split_once("://") {
        Some(("http", rest)) => ("http", rest),
        Some((_, rest)) => ("https", rest),
        None => {
            // scp-like: [user@]host:path
            let (host, path) = r.split_once(':')?;
            let host = host.rsplit('@').next()?;
            let host = if host == "ssh.github.com" { "github.com" } else { host };
            return Some(format!("https://{host}/{}", path.trim_start_matches('/')));
        }
    };
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit('@').next()?;
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
    fn encodes_urls() {
        assert_eq!(url_encode("Fix: a&b é"), "Fix%3A%20a%26b%20%C3%A9");
    }
}
