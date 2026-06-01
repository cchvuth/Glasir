//! Web URLs: hosting page of a remote (github.com / GitHub Enterprise),
//! percent-encoding for paths and query strings.

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
    fn encodes_urls() {
        assert_eq!(url_encode("Fix: a&b é"), "Fix%3A%20a%26b%20%C3%A9");
    }
}
