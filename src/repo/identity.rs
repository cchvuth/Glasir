//! Commit identity guard: Settings rules map a remote host to the email domain
//! expected there (`ghe.corp.example -> @corp.example`), checked against the
//! repo's effective `user.email`.

/// One rule per line: `host -> @domain` (also `host = @domain` or `host @domain`); `#` comments.
pub fn rules(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (host, dom) = l.split('#').next()?.split_once('@')?;
            let host = host.trim().trim_end_matches("->").trim_end_matches('=').trim().to_lowercase();
            let dom = dom.trim().to_lowercase();
            (!host.is_empty() && !dom.is_empty()).then_some((host, dom))
        })
        .collect()
}

/// `name` is `domain` or one of its subdomains.
fn under(name: &str, domain: &str) -> bool {
    name == domain || name.strip_suffix(domain).is_some_and(|p| p.ends_with('.'))
}

/// The first remote host with a rule `email` breaks: (host, expected domain).
pub fn mismatch(email: &str, hosts: impl IntoIterator<Item = String>, rules: &[(String, String)]) -> Option<(String, String)> {
    let email = email.trim().to_lowercase();
    let have = email.rsplit_once('@').map_or("", |(_, d)| d);
    hosts.into_iter().find_map(|h| rules.iter().find(|(r, _)| under(&h, r)).filter(|(_, d)| !under(have, d)).map(|(_, d)| (h.clone(), d.clone())))
}

/// The address to offer: the current local part at the expected domain.
pub fn suggest(email: &str, domain: &str) -> String {
    let local = email.trim().rsplit_once('@').map_or(email.trim(), |(l, _)| l);
    if local.is_empty() { String::new() } else { format!("{local}@{domain}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_domain_rules() {
        let r = rules("ghe.corp.example -> @corp.example\n# personal\ngithub.com = @users.noreply.github.com\nbad line\n  gitlab.x @X.org  ");
        assert_eq!(r, [("ghe.corp.example".into(), "corp.example".into()), ("github.com".into(), "users.noreply.github.com".into()), ("gitlab.x".into(), "x.org".into())]);
        let hosts = |h: &[&str]| h.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let m = |email: &str, h: &[&str]| mismatch(email, hosts(h), &r);
        assert_eq!(m("me@gmail.com", &["ghe.corp.example"]), Some(("ghe.corp.example".into(), "corp.example".into())));
        assert_eq!(m("Me@Corp.Example", &["ghe.corp.example"]), None);
        assert_eq!(m("me@eu.corp.example", &["ghe.corp.example"]), None, "subdomain of the expected domain");
        assert_eq!(m("me@notcorp.example", &["ghe.corp.example"]), Some(("ghe.corp.example".into(), "corp.example".into())));
        assert_eq!(m("", &["eu.ghe.corp.example"]), Some(("eu.ghe.corp.example".into(), "corp.example".into())), "unset email, host subdomain");
        assert_eq!(m("me@gmail.com", &["bitbucket.org", "ghe.corp.example.evil"]), None, "no rule for these hosts");
        assert_eq!(suggest("chan.vuth@gmail.com", "corp.example"), "chan.vuth@corp.example");
        assert_eq!(suggest("", "corp.example"), "");
    }
}
