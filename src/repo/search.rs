//! ⌘F commit search state: the query and its matching graph rows (no drawing;
//! the bar is `crate::ui::graph_view::search`).

use super::RepoData;

/// ⌘F search over loaded commits (message, author, email, SHA prefix).
#[derive(Default)]
pub struct Search {
    pub query: String,
    /// Matching rows, ascending.
    pub(crate) hits: Vec<usize>,
    pub(crate) cur: usize,
    last: String,
    /// Identity of the data the hits were computed for (rows shift on reload).
    data: (usize, bool),
    pub(crate) focus: bool,
}

impl Search {
    pub fn open() -> Self {
        Search { focus: true, ..Default::default() }
    }

    pub(crate) fn update(&mut self, d: &RepoData) {
        let data = (std::sync::Arc::as_ptr(&d.log) as usize, d.wip);
        if self.query == self.last && self.data == data {
            return;
        }
        self.data = data;
        self.last = self.query.clone();
        self.cur = 0;
        let q = self.query.trim().to_lowercase();
        self.hits = if q.is_empty() {
            vec![]
        } else {
            (0..d.rows())
                .filter(|&r| {
                    d.commit(r).is_some_and(|c| {
                        c.id.starts_with(&q)
                            || c.summary.to_lowercase().contains(&q)
                            || c.author.to_lowercase().contains(&q)
                            || c.email.to_lowercase().contains(&q)
                    })
                })
                .collect()
        };
    }

    pub(crate) fn dims(&self, r: usize) -> bool {
        !self.query.trim().is_empty() && self.hits.binary_search(&r).is_err()
    }
}
