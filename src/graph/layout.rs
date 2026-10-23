//! Commit-graph lane layout. Input rows are commits ordered children-before-parents
//! (`git log --date-order`); output is a column per node plus one edge per
//! parent link. Edges run vertically in their own lane and bend into the node
//! at either end, which keeps long branches as straight lines.

use std::ops::Range;

/// Parent not loaded (history truncated or shallow): the edge runs off the bottom.
pub const NONE: u32 = u32::MAX;
/// Lane colors are `0..COLORS` (the theme's lane palette).
pub const COLORS: u16 = 10;
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
    /// A merge's edge (2nd+ parent): it takes its parent's color once that is placed.
    merge: bool,
}

fn free_lane(lanes: &mut Vec<Option<Slot>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

/// Lane color for a lane starting at `col` in `row`: the one least used by the open lanes (the
/// nearer, the more it counts) and by the rows just above (`last`: each color's latest row), never
/// `avoid`; ties go to the first from `seed`'s color on, so a lane's color comes from the commit
/// that starts it.
fn pick(lanes: &[Option<Slot>], col: usize, seed: u32, avoid: Option<u16>, last: &[u32], row: u32) -> u16 {
    let cost = |c: u16| -> u32 {
        let recent = match row.wrapping_sub(last[c as usize]) {
            0..10 => 3,
            10..30 => 2,
            30..60 => 1,
            _ => 0,
        };
        let used: u32 = lanes.iter().enumerate().filter(|(_, s)| s.is_some_and(|s| s.color == c)).map(|(k, _)| match k.abs_diff(col) {
            0 | 1 => 4,
            2 => 2,
            _ => 1,
        }).sum();
        used + recent + if Some(c) == avoid { 1000 } else { 0 }
    };
    let c0 = (seed % COLORS as u32) as u16;
    (0..COLORS).map(|d| (c0 + d) % COLORS).min_by_key(|&c| cost(c)).unwrap_or(c0)
}

impl Graph {
    /// `parents[i]`: row indices of row i's parents (`NONE` if not loaded).
    /// `prio[i]`: lower keeps the row's first-parent chain straight when chains
    /// join (main < other branches < merged-in history).
    /// `seed[i]`: stable identity of row i (from its commit id): a lane's color comes from the
    /// seed of the commit that starts it, so it doesn't shift with what is laid out above.
    pub fn layout(parents: &[Vec<u32>], prio: &[u32], seed: &[u32]) -> Graph {
        let n = parents.len();
        let mut g = Graph { nodes: Vec::with_capacity(n), edges: Vec::with_capacity(n + n / 8), ..Default::default() };
        let mut lanes: Vec<Option<Slot>> = vec![];
        // Merge edges running in a lane another edge already holds for the same parent: (parent row, edge).
        let mut joining: Vec<(u32, u32)> = vec![];
        let mut last = [u32::MAX / 2; COLORS as usize];
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
                None => {
                    let k = free_lane(&mut lanes);
                    (k, prio[i as usize], pick(&lanes, k, seed[i as usize], None, &last, i))
                }
            };
            for slot in lanes.iter_mut() {
                if let Some(s) = slot
                    && s.target == i
                {
                    let e = &mut g.edges[s.edge as usize];
                    e.to = i;
                    e.to_col = col as u16;
                    if s.merge {
                        e.color = color;
                    }
                    *slot = None;
                }
            }
            joining.retain(|&(target, edge)| {
                let done = target == i;
                if done {
                    let e = &mut g.edges[edge as usize];
                    (e.to, e.to_col, e.color) = (i, col as u16, color);
                }
                !done
            });
            g.nodes.push(Node { col: col as u16, color });
            last[color as usize] = i;
            // A merge's line always has the merged-in branch's color (its parent's, set once placed), also
            // when that branch went on in another lane; only diverging branches get a new color.
            for (pi, &p) in parents[i as usize].iter().enumerate() {
                // A parent another lane already heads for (its branch went on after the merge): the
                // merge joins that lane instead of running beside it and bending back.
                if pi > 0
                    && p != NONE
                    && let Some(k) = lanes.iter().position(|s| s.is_some_and(|s| s.target == p))
                {
                    joining.push((p, g.edges.len() as u32));
                    let lane_color = lanes[k].map_or(color, |s| s.color);
                    g.edges.push(Edge { from: i, to: NONE, from_col: col as u16, lane: k as u16, to_col: k as u16, color: lane_color });
                    continue;
                }
                let (lane, prio, lane_color) = if pi == 0 {
                    (col, prio_i, color)
                } else {
                    // A merged-in lane takes its parent's color: the branch keeps it from its tip down.
                    let k = free_lane(&mut lanes);
                    let s = if p == NONE { seed[i as usize].wrapping_add(pi as u32) } else { seed[p as usize] };
                    (k, u32::MAX, pick(&lanes, k, s, Some(color), &last, i))
                };
                lanes[lane] = Some(Slot { target: p, edge: g.edges.len() as u32, prio, color: lane_color, merge: pi > 0 });
                g.edges.push(Edge { from: i, to: NONE, from_col: col as u16, lane: lane as u16, to_col: lane as u16, color: lane_color });
            }
            g.width = g.width.max(lanes.len() as u16);
        }
        g.index();
        g
    }

    /// This layout with the uncommitted-changes row on top, its lane ending at `head` (HEAD's
    /// row here, `None` if not loaded). Everything else keeps its column and color: the WIP row
    /// sits in HEAD's column when that is free above it, else the leftmost free one, and takes
    /// HEAD's color.
    pub fn with_wip(&self, head: Option<usize>, unborn: bool) -> Graph {
        let h = head.unwrap_or(self.nodes.len());
        let hc = head.map(|r| self.nodes[r].col);
        let above = self.edges.partition_point(|e| (e.from as usize) <= h);
        let mut used = vec![false; self.width as usize + 1];
        for n in &self.nodes[..h] {
            used[n.col as usize] = true;
        }
        for e in self.edges[..above].iter().filter(|e| (e.from as usize) < h || Some(e.lane) != hc) {
            used[e.lane as usize] = true;
        }
        let w = hc.filter(|&c| !used[c as usize]).unwrap_or_else(|| used.iter().position(|u| !u).unwrap_or(0) as u16);
        let color = head.map_or(0, |r| self.nodes[r].color);
        let to = head.map_or(NONE, |r| r as u32 + 1);
        let mut g = Graph { width: self.width.max(w + 1), ..Default::default() };
        g.nodes = std::iter::once(Node { col: w, color }).chain(self.nodes.iter().copied()).collect();
        g.edges = (!unborn)
            .then_some(Edge { from: 0, to, from_col: w, lane: w, to_col: hc.unwrap_or(w), color })
            .into_iter()
            .chain(self.edges.iter().map(|e| Edge { from: e.from + 1, to: if e.to == NONE { NONE } else { e.to + 1 }, ..*e }))
            .collect();
        g.index();
        g
    }

    fn index(&mut self) {
        let n = self.nodes.len();
        self.chunks = vec![vec![]; n.div_ceil(CHUNK)];
        for (id, e) in self.edges.iter().enumerate() {
            let last = if e.to == NONE { n - 1 } else { e.to as usize };
            for c in &mut self.chunks[e.from as usize / CHUNK..=last / CHUNK] {
                c.push(id as u32);
            }
        }
    }

    /// Rows reachable from any of rows `tips` by parent links (`tips` included).
    /// One pass: edges are stored in `from` order and parents sit below children.
    pub fn reachable(&self, tips: &[usize]) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        tips.iter().for_each(|&t| seen[t] = true);
        let first = tips.iter().copied().min().unwrap_or(usize::MAX);
        let start = self.edges.partition_point(|e| (e.from as usize) < first);
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

    fn colors(g: &Graph) -> Vec<u16> {
        g.nodes.iter().map(|n| n.color).collect()
    }

    fn seeds(n: usize) -> Vec<u32> {
        (0..n as u32).map(|i| i * 3).collect()
    }

    #[test]
    fn linear() {
        let g = Graph::layout(&[vec![1], vec![2], vec![]], &[5; 3], &seeds(3));
        assert_eq!(cols(&g), [0, 0, 0]);
        assert_eq!(g.edges.len(), 2);
        assert!(g.edges.iter().all(|e| e.lane == 0 && e.to_col == 0));
        assert_eq!(g.width, 1);
    }

    #[test]
    fn branch_and_merge() {
        // 0: merge(1, 2)  1: main  2: feature  3: base
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![]], &[0, 5, 5, 5], &seeds(4));
        assert_eq!(cols(&g), [0, 0, 1, 0]);
        let merge = g.edges[1];
        assert_eq!((merge.from, merge.to, merge.lane, merge.to_col), (0, 2, 1, 1));
        // feature joins back into lane 0 at the base commit
        let join = g.edges.iter().find(|e| e.from == 2).unwrap();
        assert_eq!((join.lane, join.to_col), (1, 0));
    }

    #[test]
    fn a_merge_line_has_the_merged_branch_color() {
        // 0: merge(1, 2) on main  1: main  2: feature  3: base
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![]], &[0, 5, 5, 5], &seeds(4));
        let merge = g.edges.iter().find(|e| e.from == 0 && e.to == 2).unwrap();
        assert_eq!(merge.color, g.nodes[2].color);
        assert_ne!(g.nodes[2].color, g.nodes[0].color, "the feature keeps its own color");
        // The merged-in commit lands in another lane (its branch went on after the merge): the
        // merge's line still has that branch's color.
        // 0: merge(2, 3)  1: a newer child of 3 on its own lane  2: main  3: merged-in  4: base
        let g = Graph::layout(&[vec![2, 3], vec![3], vec![4], vec![4], vec![]], &[0, 1, 0, 5, 0], &seeds(5));
        let merge = g.edges.iter().find(|e| e.from == 0 && e.to == 3).unwrap();
        assert_ne!(merge.lane, g.nodes[3].col, "it lands in the other lane");
        assert_eq!(merge.color, g.nodes[3].color);
    }

    #[test]
    fn a_merge_into_a_branch_that_went_on_joins_its_lane() {
        // 0: feature's newer commit (on 2)  1: merge(3, 2) on main  2: feature  3: main base
        let g = Graph::layout(&[vec![2], vec![3, 2], vec![3], vec![]], &[5, 0, 5, 0], &seeds(4));
        let merge = g.edges.iter().find(|e| e.from == 1 && e.to == 2).unwrap();
        assert_eq!((merge.lane, merge.to_col), (g.nodes[2].col, g.nodes[2].col), "no extra lane beside the feature's");
        assert_eq!(g.width, 2);
        assert_eq!(merge.color, g.nodes[2].color);
        assert!(g.edges.iter().all(|e| e.to != NONE), "every edge reaches its parent");
    }

    #[test]
    fn diverging_branches_get_different_colors() {
        // 0, 1, 2: three branch tips on the same base 3, far apart from each other.
        let g = Graph::layout(&[vec![3], vec![3], vec![3], vec![]], &[5; 4], &[10, 20, 30, 40]);
        let mut c: Vec<u16> = g.nodes[..3].iter().map(|n| n.color).collect();
        c.sort();
        c.dedup();
        assert_eq!(c.len(), 3, "same seed color, still three colors");
    }

    #[test]
    fn priority_keeps_trunk_straight() {
        // 0: newer feature tip, 1: main's tip; both have parent 2.
        let g = Graph::layout(&[vec![2], vec![2], vec![]], &[5, 1, 5], &seeds(3));
        assert_eq!(cols(&g), [0, 1, 1]); // base continues main's lane, feature bends in
        assert_eq!(g.nodes[2].color, g.nodes[1].color);
    }

    #[test]
    fn colors_come_from_the_commits_not_the_rows_above() {
        // 0: merge(1, 2)  1: main  2: feature  3: base; then the same under a new tip.
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![]], &[5; 4], &[17, 23, 31, 47]);
        assert_eq!(colors(&g), [7, 7, 1, 7]); // the merged-in lane takes its parent's color
        let g2 = Graph::layout(&[vec![4], vec![2, 3], vec![4], vec![4], vec![]], &[6, 5, 5, 5, 5], &[99, 17, 23, 31, 47]);
        assert_eq!(colors(&g2)[1..], colors(&g)[..]);
        // Side by side, two lanes with the same seed color still differ.
        let g = Graph::layout(&[vec![2], vec![2], vec![]], &[5; 3], &[4, 14, 0]);
        assert_eq!(colors(&g)[..2], [4, 5]);
    }

    #[test]
    fn wip_row_keeps_every_column_and_color() {
        // 0: feature tip  1: main tip (HEAD)  2: base
        let g = Graph::layout(&[vec![2], vec![2], vec![]], &[5, 1, 5], &seeds(3));
        let w = g.with_wip(Some(1), false);
        assert_eq!(cols(&w)[1..], cols(&g)[..]);
        assert_eq!(colors(&w), [g.nodes[1].color, g.nodes[0].color, g.nodes[1].color, g.nodes[2].color]);
        assert_eq!(w.nodes[0].col, 1); // right above HEAD: its column is free there
        assert_eq!((w.edges[0].from, w.edges[0].to, w.edges[0].lane, w.edges[0].to_col), (0, 2, 1, 1));
        assert!(w.edges[1..].iter().zip(&g.edges).all(|(a, b)| (a.from, a.to, a.lane, a.color) == (b.from + 1, b.to + 1, b.lane, b.color)));
        assert_eq!(w.edges_in(0..4).count(), 3);
        // HEAD on the base: both lanes pass above it, so WIP takes the next free column.
        let w = g.with_wip(Some(2), false);
        assert_eq!((w.nodes[0].col, w.edges[0].to_col, w.width), (2, 1, 3));
        // Unborn branch: WIP alone.
        let w = Graph::default().with_wip(None, true);
        assert_eq!((cols(&w), w.edges.len(), w.width), (vec![0], 0, 1));
        let w = g.with_wip(None, true);
        assert_eq!(w.edges.len(), g.edges.len());
        assert!(w.edges.iter().all(|e| e.from > 0));
    }

    #[test]
    fn reachable_follows_both_parents() {
        // 0: merge(1, 2)  1: main  2: feature  3: base  4: unrelated tip  5: its parent (not loaded)
        let g = Graph::layout(&[vec![1, 2], vec![3], vec![3], vec![], vec![NONE]], &[5; 5], &seeds(5));
        assert_eq!(g.reachable(&[0]), [true, true, true, true, false]);
        assert_eq!(g.reachable(&[2]), [false, false, true, true, false]);
        assert_eq!(g.reachable(&[4]), [false, false, false, false, true]);
        assert_eq!(g.reachable(&[4, 2]), [false, false, true, true, true]);
        assert_eq!(g.reachable(&[]), [false; 5]);
        assert!(!g.has_unloaded_parent(&g.reachable(&[0])));
        assert!(g.has_unloaded_parent(&g.reachable(&[4])));
    }

    #[test]
    fn missing_parent_and_edges_in() {
        let n = 300;
        let parents: Vec<Vec<u32>> = (0..n).map(|i| if i + 1 < n { vec![i as u32 + 1] } else { vec![NONE] }).collect();
        let g = Graph::layout(&parents, &vec![5; n], &seeds(n));
        assert_eq!(g.edges.last().unwrap().to, NONE);
        let got: std::collections::HashSet<u32> = g.edges_in(100..200).map(|e| e.from).collect();
        assert_eq!(got.len(), g.edges_in(100..200).count()); // no duplicates
        assert!((99..200).all(|i| got.contains(&i)));
        assert_eq!(g.edges_in(0..n).count(), n);
    }
}
