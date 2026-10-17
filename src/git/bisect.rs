//! `git bisect` progress, read from its refs (`refs/bisect/bad`, `good-<sha>`,
//! `skip-<sha>`) plus git's own estimate (`rev-list --bisect-vars`).

use super::{Res, git_dir, run};
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
    pub terms: (String, String),
}

impl Bisect {
    pub fn term<'a>(&'a self, what: &'a str) -> &'a str {
        let t = match what {
            "bad" => &self.terms.0,
            "good" => &self.terms.1,
            _ => return what,
        };
        if t.is_empty() { what } else { t }
    }

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

fn parse_refs(out: &str, terms: (String, String)) -> Bisect {
    let mut b = Bisect { terms, ..Bisect::default() };
    let (bad, good) = (b.term("bad").to_owned(), format!("{}-", b.term("good")));
    for (name, id) in out.lines().filter_map(|l| l.split_once(' ')) {
        let id = id.to_owned();
        match name.strip_prefix("refs/bisect/") {
            Some(n) if n == bad => b.bad = Some(id),
            Some(n) if n.starts_with(&good) => b.good.push(id),
            Some(n) if n.starts_with("skip-") => b.skip.push(id),
            _ => {}
        }
    }
    b
}

/// git's own estimate (`estimate_bisect_steps`), from the number of commits in range.
fn steps(all: usize) -> usize {
    if all < 3 {
        return 0;
    }
    let (n, e) = (all.ilog2() as usize, 1 << all.ilog2());
    if e < 3 * (all - e) { n } else { n - 1 }
}

// Skipped commits still count in `left`/`steps`; good enough for an estimate. `prev`: the
// last result, reused while the marks are the same (counting a huge range takes seconds).
pub fn bisect(dir: &Path, prev: Option<&Bisect>) -> Res<Bisect> {
    let terms = std::fs::read_to_string(git_dir(dir)?.join("BISECT_TERMS")).unwrap_or_default();
    let mut terms = terms.lines().map(str::to_owned);
    let terms = (terms.next().unwrap_or_default(), terms.next().unwrap_or_default());
    let mut b = parse_refs(&run(dir, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/bisect"])?, terms);
    if let Some(p) = prev.filter(|p| (&p.bad, &p.good, &p.skip) == (&b.bad, &b.good, &b.skip)) {
        return Ok(p.clone());
    }
    if let (Some(bad), false) = (&b.bad, b.good.is_empty()) {
        let mut a = vec!["rev-list", "--count", bad.as_str(), "--not"];
        a.extend(b.good.iter().map(String::as_str));
        b.left = run(dir, &a)?.trim().parse().unwrap_or(0);
        b.steps = steps(b.left);
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refs_and_vars() {
        let b = parse_refs("refs/bisect/bad b1\nrefs/bisect/good-g1 g1\nrefs/bisect/good-g2 g2\nrefs/bisect/skip-s1 s1\n", Default::default());
        assert_eq!((b.bad.as_deref(), b.good.len(), b.skip.as_slice()), (Some("b1"), 2, &["s1".to_owned()][..]));
        assert_eq!((b.mark("b1"), b.mark("g2"), b.mark("s1"), b.mark("x")), (Some("bad"), Some("good"), Some("skip"), None));
        // git's own numbers: all=19 -> 3 steps (`--bisect-vars`).
        assert_eq!([0, 1, 2, 3, 4, 5, 6, 7, 8, 19, 46013].map(steps), [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 15]);
        assert_eq!(b.found(), None);
        assert_eq!(Bisect { left: 1, ..b }.found(), Some("b1"));
        let c = parse_refs("refs/bisect/new n1\nrefs/bisect/old-o1 o1\nrefs/bisect/bad x\n", ("new".into(), "old".into()));
        assert_eq!((c.bad.as_deref(), c.good.as_slice(), c.term("bad"), c.term("good"), c.term("skip")), (Some("n1"), &["o1".to_owned()][..], "new", "old", "skip"));
    }
}
