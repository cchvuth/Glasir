//! `git bisect` progress, read from its refs (`refs/bisect/bad`, `good-<sha>`,
//! `skip-<sha>`) plus git's own estimate (`rev-list --bisect-vars`).

use super::{Res, run};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bisect {
    pub bad: Option<String>,
    pub good: Vec<String>,
    pub skip: Vec<String>,
    /// Commits still in range, the bad one included (0 until both ends are known).
    pub left: usize,
    /// git's "roughly N steps" after the commit being tested.
    pub steps: usize,
}

impl Bisect {
    pub fn mark(&self, id: &str) -> Option<&'static str> {
        if self.bad.as_deref() == Some(id) {
            Some("bad")
        } else if self.good.iter().any(|g| g == id) {
            Some("good")
        } else {
            self.skip.iter().any(|s| s == id).then_some("skip")
        }
    }

    /// The first bad commit, once only it is left.
    pub fn found(&self) -> Option<&str> {
        self.bad.as_deref().filter(|_| self.left == 1 && !self.good.is_empty())
    }
}

fn parse_refs(out: &str) -> Bisect {
    let mut b = Bisect::default();
    for (name, id) in out.lines().filter_map(|l| l.split_once(' ')) {
        let id = id.to_owned();
        match name.strip_prefix("refs/bisect/") {
            Some("bad") => b.bad = Some(id),
            Some(n) if n.starts_with("good-") => b.good.push(id),
            Some(n) if n.starts_with("skip-") => b.skip.push(id),
            _ => {}
        }
    }
    b
}

fn var(vars: &str, key: &str) -> usize {
    vars.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')?.trim_matches('\'').parse().ok()).unwrap_or(0)
}

// Skipped commits still count in `left`/`steps`; good enough for an estimate.
pub fn bisect(dir: &Path) -> Res<Bisect> {
    let mut b = parse_refs(&run(dir, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/bisect"])?);
    if let (Some(bad), false) = (&b.bad, b.good.is_empty()) {
        let mut a = vec!["rev-list", "--bisect-vars", bad.as_str(), "--not"];
        a.extend(b.good.iter().map(String::as_str));
        let vars = run(dir, &a)?;
        (b.left, b.steps) = (var(&vars, "bisect_all"), var(&vars, "bisect_steps"));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refs_and_vars() {
        let b = parse_refs("refs/bisect/bad b1\nrefs/bisect/good-g1 g1\nrefs/bisect/good-g2 g2\nrefs/bisect/skip-s1 s1\n");
        assert_eq!((b.bad.as_deref(), b.good.len(), b.skip.as_slice()), (Some("b1"), 2, &["s1".to_owned()][..]));
        assert_eq!((b.mark("b1"), b.mark("g2"), b.mark("s1"), b.mark("x")), (Some("bad"), Some("good"), Some("skip"), None));
        let vars = "bisect_rev='abc'\nbisect_nr=9\nbisect_all=19\nbisect_steps=3\n";
        assert_eq!((var(vars, "bisect_all"), var(vars, "bisect_steps"), var(vars, "nope")), (19, 3, 0));
        assert_eq!(b.found(), None);
        assert_eq!(Bisect { left: 1, ..b }.found(), Some("b1"));
    }
}
