//! Changed files as a folder tree (the inspector's "tree" view): pure data, no drawing.
//! Folders come first, then files, each by name; a chain of folders with a single folder
//! inside is compacted into one row (`src/repo`); a rename sits under its new path.

use crate::git::Change;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

pub enum Node {
    /// `path` is the full folder path (the collapse key), `name` what the row shows
    /// (`src/repo` for a compacted chain); `files` are the changes under it, `end` the
    /// index of the first node after its subtree.
    Dir { path: String, name: String, depth: usize, files: Range<usize>, end: usize },
    File { idx: usize, depth: usize },
}

#[derive(Default)]
pub struct FileTree {
    /// The changes in tree order, so a folder's files are one contiguous range.
    pub files: Vec<Change>,
    /// Pre-order, folders and files interleaved as shown.
    pub nodes: Vec<Node>,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<Change>,
}

fn name_of(c: &Change) -> &str {
    c.path.rsplit('/').next().unwrap_or(&c.path)
}

impl FileTree {
    pub fn build<'a>(changes: impl IntoIterator<Item = &'a Change>) -> Self {
        let mut root = Dir::default();
        for c in changes {
            let mut d = &mut root;
            let mut parts = c.path.split('/');
            parts.next_back();
            for p in parts {
                d = d.dirs.entry(p.to_owned()).or_default();
            }
            d.files.push(c.clone());
        }
        let mut t = FileTree::default();
        t.flatten(root, "", 0);
        t
    }

    fn flatten(&mut self, d: Dir, prefix: &str, depth: usize) {
        let mut dirs: Vec<_> = d.dirs.into_iter().collect();
        dirs.sort_by_cached_key(|(n, _)| n.to_lowercase());
        for (mut name, mut sub) in dirs {
            let mut path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            while sub.files.is_empty() && sub.dirs.len() == 1 {
                let (n, s) = sub.dirs.into_iter().next().unwrap_or_default();
                (name, path, sub) = (format!("{name}/{n}"), format!("{path}/{n}"), s);
            }
            let (at, start) = (self.nodes.len(), self.files.len());
            self.nodes.push(Node::Dir { path: path.clone(), name, depth, files: start..start, end: at + 1 });
            self.flatten(sub, &path, depth + 1);
            let (files, end) = (start..self.files.len(), self.nodes.len());
            if let Node::Dir { files: f, end: e, .. } = &mut self.nodes[at] {
                (*f, *e) = (files, end);
            }
        }
        let mut files = d.files;
        files.sort_by_cached_key(|c| (name_of(c).to_lowercase(), c.path.clone()));
        for c in files {
            self.nodes.push(Node::File { idx: self.files.len(), depth });
            self.files.push(c);
        }
    }

    /// Indices of the nodes shown: everything except what sits inside a `closed` folder.
    pub fn visible(&self, closed: &HashSet<String>) -> Vec<usize> {
        let (mut v, mut i) = (vec![], 0);
        while i < self.nodes.len() {
            v.push(i);
            i = match &self.nodes[i] {
                Node::Dir { path, end, .. } if closed.contains(path) => *end,
                _ => i + 1,
            };
        }
        v
    }

    /// The changes in the shown order (what ⇧-click ranges run over).
    pub fn visible_files(&self, closed: &HashSet<String>) -> Vec<Change> {
        self.visible(closed).into_iter().filter_map(|i| if let Node::File { idx, .. } = self.nodes[i] { Some(self.files[idx].clone()) } else { None }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(path: &str, code: char, old: Option<&str>) -> Change {
        Change { path: path.into(), old_path: old.map(Into::into), code }
    }

    /// One line per node: `dir name/` or `file path`, indented by depth.
    fn shape(t: &FileTree, closed: &HashSet<String>) -> Vec<String> {
        t.visible(closed)
            .into_iter()
            .map(|i| match &t.nodes[i] {
                Node::Dir { name, depth, files, .. } => format!("{}{name}/ ({})", " ".repeat(*depth), files.len()),
                Node::File { idx, depth } => format!("{}{}", " ".repeat(*depth), t.files[*idx].path),
            })
            .collect()
    }

    #[test]
    fn nests_sorts_folders_first_and_compacts_chains() {
        let cs = [ch("z.rs", 'M', None), ch("src/repo/b.rs", 'M', None), ch("README.md", 'M', None), ch("src/repo/a.rs", 'A', None), ch("src/ui/x.rs", 'M', None), ch("docs/deep/er/g.md", 'M', None), ch("src/main.rs", 'M', None)];
        let t = FileTree::build(&cs);
        assert_eq!(
            shape(&t, &HashSet::new()),
            [
                "docs/deep/er/ (1)",
                " docs/deep/er/g.md",
                "src/ (4)",
                " repo/ (2)",
                "  src/repo/a.rs",
                "  src/repo/b.rs",
                " ui/ (1)",
                "  src/ui/x.rs",
                " src/main.rs",
                "README.md",
                "z.rs",
            ]
        );
    }

    #[test]
    fn a_folders_files_are_one_contiguous_range() {
        let cs = [ch("a/x", 'M', None), ch("b.rs", 'M', None), ch("a/b/y", 'M', None)];
        let t = FileTree::build(&cs);
        let Node::Dir { files, .. } = &t.nodes[0] else { panic!() };
        let paths: Vec<_> = t.files[files.clone()].iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["a/b/y", "a/x"]);
    }

    #[test]
    fn collapsed_folders_hide_their_subtree() {
        let cs = [ch("a/b/x", 'M', None), ch("a/c.rs", 'M', None), ch("d.rs", 'M', None)];
        let t = FileTree::build(&cs);
        let closed: HashSet<String> = ["a".to_owned()].into();
        assert_eq!(shape(&t, &closed), ["a/ (2)", "d.rs"]);
        let vis: Vec<_> = t.visible_files(&closed).into_iter().map(|c| c.path).collect();
        assert_eq!(vis, ["d.rs"]);
        // Closing an inner folder keeps its siblings.
        let closed: HashSet<String> = ["a/b".to_owned()].into();
        assert_eq!(shape(&t, &closed), ["a/ (2)", " b/ (1)", " a/c.rs", "d.rs"]);
    }

    #[test]
    fn a_rename_sits_under_its_new_path() {
        let t = FileTree::build(&[ch("new/dir/f.rs", 'R', Some("old/f.rs"))]);
        assert_eq!(shape(&t, &HashSet::new()), ["new/dir/ (1)", " new/dir/f.rs"]);
        assert_eq!(t.files[0].old_path.as_deref(), Some("old/f.rs"));
    }

    #[test]
    fn empty_and_flat_lists() {
        assert!(FileTree::build(&[]).nodes.is_empty());
        let t = FileTree::build(&[ch("b", 'M', None), ch("A", 'M', None)]);
        assert_eq!(shape(&t, &HashSet::new()), ["A", "b"]);
    }
}
