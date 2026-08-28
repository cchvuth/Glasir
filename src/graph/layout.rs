//! Commit-graph lane layout. Input rows are commits ordered children-before-parents
//! (`git log --date-order`); output is a column per node plus one edge per
//! parent link. Edges run vertically in their own lane and bend into the node
//! at either end, which keeps long branches as straight lines.

use std::ops::Range;

/// Parent not loaded (history truncated or shallow): the edge runs off the bottom.
pub const NONE: u32 = u32::MAX;
const CHUNK: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub col: u16,
    pub color: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub from: u32,
    /// Parent row, or `NONE`.
    pub to: u32,
    pub from_col: u16,
    pub lane: u16,
    pub to_col: u16,
    pub color: u16,
}

#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub width: u16,
    /// Edge ids touching each block of `CHUNK` rows, for O(visible) drawing.
    chunks: Vec<Vec<u32>>,
}

#[derive(Clone, Copy)]
struct Slot {
    target: u32,
    edge: u32,
    prio: u32,
    color: u16,
}

fn free_lane(lanes: &mut Vec<Option<Slot>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

impl Graph {
    /// `parents[i]`: row indices of row i's parents (`NONE` if not loaded).
    /// `prio[i]`: lower keeps the row's first-parent chain straight when chains
    /// join (HEAD < main < other branches < merged-in history).
    pub fn layout(parents: &[Vec<u32>], prio: &[u32]) -> Graph {
        let n = parents.len();
        let mut g = Graph { nodes: Vec::with_capacity(n), edges: Vec::with_capacity(n + n / 8), ..Default::default() };
        let mut lanes: Vec<Option<Slot>> = vec![];
        let mut next_color = 0u16;
        let mut new_color = || {
            next_color = next_color.wrapping_add(1);
            next_color - 1
        };
        for i in 0..n as u32 {
            // The waiting lane with the best chain priority (ties: leftmost) gets the node.
            let mut best: Option<(usize, Slot)> = None;
            for (k, s) in lanes.iter().enumerate() {
                if let Some(s) = s
                    && s.target == i
                    && best.is_none_or(|(_, b)| s.prio < b.prio)
                {
                    best = Some((k, *s));
                }
            }
            let (col, prio_i, color) = match best {
                Some((k, s)) => (k, s.prio.min(prio[i as usize]), s.color),
                None => (free_lane(&mut lanes), prio[i as usize], new_color()),
            };
            for slot in lanes.iter_mut() {
                if let Some(s) = slot
                    && s.target == i
                {
                    let e = &mut g.edges[s.edge as usize];
                    e.to = i;
                    e.to_col = col as u16;
                    *slot = None;
                }
            }
            g.nodes.push(Node { col: col as u16, color });
            for (pi, &p) in parents[i as usize].iter().enumerate() {
                let (lane, prio, color) =
                    if pi == 0 { (col, prio_i, color) } else { (free_lane(&mut lanes), u32::MAX, new_color()) };
                lanes[lane] = Some(Slot { target: p, edge: g.edges.len() as u32, prio, color });
                g.edges.push(Edge { from: i, to: NONE, from_col: col as u16, lane: lane as u16, to_col: lane as u16, color });
            }
            g.width = g.width.max(lanes.len() as u16);
        }
        g.chunks = vec![vec![]; n.div_ceil(CHUNK)];
        for (id, e) in g.edges.iter().enumerate() {
            let last = if e.to == NONE { n - 1 } else { e.to as usize };
            for c in &mut g.chunks[e.from as usize / CHUNK..=last / CHUNK] {
                c.push(id as u32);
            }
        }
        g
    }

    /// Rows reachable from row `tip` by parent links (`tip` included).
    /// One pass: edges are stored in `from` order and parents sit below children.
    pub fn reachable(&self, tip: usize) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        seen[tip] = true;
        let start = self.edges.partition_point(|e| (e.from as usize) < tip);
        for e in &self.edges[start..] {
            if seen[e.from as usize] && e.to != NONE {
                seen[e.to as usize] = true;
            }
        }
        seen
    }

    /// Does any of the rows in `seen` have a parent that isn't loaded?
    pub fn has_unloaded_parent(&self, seen: &[bool]) -> bool {
        self.edges.iter().any(|e| e.to == NONE && seen[e.from as usize])
    }

    /// Every edge touching `rows` (plus neighbours in the same chunks), each once.
    pub fn edges_in(&self, rows: Range<usize>) -> impl Iterator<Item = &Edge> {
        let (k0, k1) = (rows.start / CHUNK, rows.end.div_ceil(CHUNK).min(self.chunks.len()));
        (k0..k1).flat_map(move |k| {
            self.chunks[k]
                .iter()
                .map(|&id| &self.edges[id as usize])
                .filter(move |e| (e.from as usize / CHUNK).max(k0) == k)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(g: &Graph) -> Vec<u16> {
        g.nodes.iter().map(|n| n.col).collect()
    }

    #[test]
    fn linear() {
        let g = Graph::layout(&[vec![1], vec![2], vec![]], &[5; 3]);
        assert_eq!(cols(&g), [0, 0, 0]);
        assert_eq!(g.edges.len(), 2);
        assert!(g.edges.iter().all(|e| e.lane == 0 && e.to_col == 0));
        assert_eq!(g.width, 1);
    }

    #[test]
    fn branch_and_merge() {
        // 0: merge(1, 2)  1: main  2: feature  3: base
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![]], &[0, 5, 5, 5]);
        assert_eq!(cols(&g), [0, 0, 1, 0]);
        let merge = g.edges[1];
        assert_eq!((merge.from, merge.to, merge.lane, merge.to_col), (0, 2, 1, 1));
        // feature joins back into lane 0 at the base commit
        let join = g.edges.iter().find(|e| e.from == 2).unwrap();
        assert_eq!((join.lane, join.to_col), (1, 0));
    }

    #[test]
    fn priority_keeps_head_straight() {
        // 0: newer feature tip, 1: HEAD tip; both have parent 2.
        let g = Graph::layout(&[vec![2], vec![2], vec![]], &[5, 0, 5]);
        assert_eq!(cols(&g), [0, 1, 1]); // base continues HEAD's lane, feature bends in
    }

    #[test]
    fn reachable_follows_both_parents() {
        // 0: merge(1, 2)  1: main  2: feature  3: base  4: unrelated tip  5: its parent (not loaded)
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![], vec![NONE]], &[5; 5]);
        assert_eq!(g.reachable(0), [true, true, true, true, false]);
        assert_eq!(g.reachable(2), [false, false, true, true, false]);
        assert_eq!(g.reachable(4), [false, false, false, false, true]);
        assert!(!g.has_unloaded_parent(&g.reachable(0)));
        assert!(g.has_unloaded_parent(&g.reachable(4)));
    }

    #[test]
    fn missing_parent_and_edges_in() {
        let n = 300;
        let parents: Vec<Vec<u32>> = (0..n).map(|i| if i + 1 < n { vec![i as u32 + 1] } else { vec![NONE] }).collect();
        let g = Graph::layout(&parents, &vec![5; n]);
        assert_eq!(g.edges.last().unwrap().to, NONE);
        let got: std::collections::HashSet<u32> = g.edges_in(100..200).map(|e| e.from).collect();
        assert_eq!(got.len(), g.edges_in(100..200).count()); // no duplicates
        assert!((99..200).all(|i| got.contains(&i)));
        assert_eq!(g.edges_in(0..n).count(), n);
    }
}
