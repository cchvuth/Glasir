//! Commit-graph lane layout. Input rows are commits ordered children-before-parents
//! (`git log --date-order`); output is a column per node plus one edge per
//! parent link. Edges run vertically in their own lane and bend into the node
//! at either end, which keeps long branches as straight lines. The default
//! branch's first-parent chain (main) has column 0 and `MAIN_COLOR` to itself.

use std::ops::Range;

/// Parent not loaded (history truncated or shallow): the edge runs off the bottom.
pub const NONE: u32 = u32::MAX;
/// Lane colors are `0..COLORS` (the theme's lane palette).
pub const COLORS: u16 = 10;
/// The main chain's color (the theme's accent): no other lane gets it while there is one.
pub const MAIN_COLOR: u16 = 0;
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
    /// Column 0 is the main chain's alone (drawn bolder).
    pub main: bool,
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
    /// The row whose branch names the lane carries (`NONE`: none yet, e.g. a merge's lane).
    owner: u32,
}

/// The leftmost free lane from `from` on (1 with a main chain: column 0 is main's).
fn free_lane(lanes: &mut Vec<Option<Slot>>, from: usize) -> usize {
    (from..lanes.len()).find(|&k| lanes[k].is_none()).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

/// Lane color for a lane starting at `col` in `row`: the one least used by the open lanes (the
/// nearer, the more it counts) and by the rows just above (`last`: each color's latest row), never
/// `avoid`, nor `MAIN_COLOR` while there is a `main` chain; ties go to the first from `seed`'s color
/// on, so a lane's color comes from the commit that starts it.
fn pick(lanes: &[Option<Slot>], col: usize, seed: u32, avoid: Option<u16>, last: &[u32], row: u32, main: bool) -> u16 {
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
    // With a main chain, the other lanes share the colors after MAIN_COLOR (0).
    let (lo, k) = if main { (1, COLORS - 1) } else { (0, COLORS) };
    let c0 = (seed % k as u32) as u16;
    (0..k).map(|d| lo + (c0 + d) % k).min_by_key(|&c| cost(c)).unwrap_or(lo)
}

impl Graph {
    /// `parents[i]`: row indices of row i's parents (`NONE` if not loaded).
    /// `prio[i]`: lower keeps the row's first-parent chain straight when chains
    /// join (main < other branches < merged-in history).
    /// `seed[i]`: stable identity of row i (from its commit id): a lane's color comes from the
    /// seed of the commit that starts it, so it doesn't shift with what is laid out above.
    /// `names[i]`: ids of the branches whose tip is row i (a branch and its remote's share one): a
    /// branch tip under another branch's commits (stacked on it) starts a color of its own.
    /// `main`: the default branch's tip row. Its first-parent chain runs straight down column 0
    /// in `MAIN_COLOR`; no other node or lane takes either.
    pub fn layout(parents: &[Vec<u32>], prio: &[u32], seed: &[u32], names: &[Vec<u32>], main: Option<u32>) -> Graph {
        let n = parents.len();
        let mut on_main = vec![false; n];
        let mut r = main.unwrap_or(NONE);
        while r != NONE {
            on_main[r as usize] = true;
            r = parents[r as usize].first().copied().unwrap_or(NONE);
        }
        let m = main.is_some();
        let mut g = Graph { nodes: Vec::with_capacity(n), edges: Vec::with_capacity(n + n / 8), main: m, ..Default::default() };
        let mut lanes: Vec<Option<Slot>> = vec![None; m as usize];
        // Merge edges running in a lane another edge already holds for the same parent: (parent row, edge, that lane's edge).
        let mut joining: Vec<(u32, u32, u32)> = vec![];
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
            let named = &names[i as usize];
            // A branch tip under another branch's commits (a name its lane hasn't carried) starts a color of its own.
            let stacked = best.is_some_and(|(_, s)| s.owner != NONE && named.iter().any(|b| !names[s.owner as usize].contains(b)));
            let (col, prio_i, color) = match best {
                _ if on_main[i as usize] => (0, prio[i as usize], MAIN_COLOR),
                Some((k, s)) => {
                    // Its own branch coming down another lane too (an upstream ahead of it) lends it that one's,
                    // unless that is the color it leaves.
                    let own = || lanes.iter().flatten().find(|w| w.target == i && w.edge != s.edge && w.color != s.color && w.owner != NONE && names[w.owner as usize].iter().any(|b| named.contains(b)));
                    let color = if stacked { own().map_or_else(|| pick(&lanes, k, seed[i as usize], Some(s.color), &last, i, m), |w| w.color) } else { s.color };
                    (k, s.prio.min(prio[i as usize]), color)
                }
                None => {
                    let k = free_lane(&mut lanes, m as usize);
                    (k, prio[i as usize], pick(&lanes, k, seed[i as usize], None, &last, i, m))
                }
            };
            // The branch its first-parent lane carries on: the lane's, unless it had none or this tip starts its own.
            let owner = match best {
                Some((_, s)) if s.owner != NONE && !stacked => s.owner,
                _ if named.is_empty() => NONE,
                _ => i,
            };
            for s in lanes.iter().flatten().filter(|s| s.target == i) {
                let e = &mut g.edges[s.edge as usize];
                e.to = i;
                e.to_col = col as u16;
                if s.merge {
                    e.color = color;
                }
            }
            joining.retain(|&(target, edge, host)| {
                let done = target == i;
                if done {
                    let host_color = g.edges[host as usize].color;
                    let e = &mut g.edges[edge as usize];
                    (e.to, e.to_col, e.color) = (i, col as u16, host_color);
                }
                !done
            });
            g.nodes.push(Node { col: col as u16, color });
            last[color as usize] = i;
            // A merge's line always has the merged-in branch's color (its parent's, set once placed), also
            // when that branch went on in another lane (a line joining a lane takes that lane's); only
            // diverging branches get a new color.
            for (pi, &p) in parents[i as usize].iter().enumerate() {
                // A parent another lane already heads for (its branch went on after the merge): the
                // merge joins that lane instead of running beside it and bending back.
                if pi > 0
                    && p != NONE
                    && let Some((k, h)) = lanes.iter().enumerate().find_map(|(k, s)| s.filter(|s| s.target == p).map(|s| (k, s)))
                {
                    joining.push((p, g.edges.len() as u32, h.edge));
                    g.edges.push(Edge { from: i, to: NONE, from_col: col as u16, lane: k as u16, to_col: k as u16, color: h.color });
                    continue;
                }
                let (lane, prio, lane_color) = if pi == 0 {
                    (col, prio_i, color)
                } else {
                    // A merged-in lane takes its parent's color: the branch keeps it from its tip down.
                    let k = free_lane(&mut lanes, m as usize);
                    let s = if p == NONE { seed[i as usize].wrapping_add(pi as u32) } else { seed[p as usize] };
                    (k, u32::MAX, pick(&lanes, k, s, Some(color), &last, i, m))
                };
                lanes[lane] = Some(Slot { target: p, edge: g.edges.len() as u32, prio, color: lane_color, merge: pi > 0, owner: if pi == 0 { owner } else { NONE } });
                g.edges.push(Edge { from: i, to: NONE, from_col: col as u16, lane: lane as u16, to_col: lane as u16, color: lane_color });
            }
            // The lanes that ran into the node free up only now: a merge's line leaving along one
            // would read as that line going on.
            for slot in lanes.iter_mut().filter(|s| s.is_some_and(|s| s.target == i)) {
                *slot = None;
            }
            g.width = g.width.max(lanes.len() as u16);
        }
        g.index();
        g
    }

    /// This layout with the uncommitted-changes row on top, its lane ending at `head` (HEAD's
    /// row here, `None` if not loaded). Everything else keeps its column and color: the WIP row
    /// sits in HEAD's column when that is free above it, else the leftmost free one (never main's),
    /// and takes HEAD's color.
    pub fn with_wip(&self, head: Option<usize>, unborn: bool) -> Graph {
        let h = head.unwrap_or(self.nodes.len());
        let hc = head.map(|r| self.nodes[r].col);
        let above = self.edges.partition_point(|e| (e.from as usize) <= h);
        let mut used = vec![false; self.width as usize + 1];
        // Column 0 is main's: WIP takes it only as HEAD's own, right above main's tip.
        used[0] = self.main && hc != Some(0);
        for n in &self.nodes[..h] {
            used[n.col as usize] = true;
        }
        for e in self.edges[..above].iter().filter(|e| (e.from as usize) < h || Some(e.lane) != hc) {
            used[e.lane as usize] = true;
        }
        let w = hc.filter(|&c| !used[c as usize]).unwrap_or_else(|| used.iter().position(|u| !u).unwrap_or(0) as u16);
        // HEAD not loaded: any color but main's.
        let color = head.map_or(self.main as u16, |r| self.nodes[r].color);
        let to = head.map_or(NONE, |r| r as u32 + 1);
        let mut g = Graph { width: self.width.max(w + 1), main: self.main, ..Default::default() };
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

    /// No branch names, no main chain: the layout as it was before either existed.
    fn lay(parents: &[Vec<u32>], prio: &[u32], seed: &[u32]) -> Graph {
        Graph::layout(parents, prio, seed, &vec![vec![]; parents.len()], None)
    }

    /// Edges sharing a lane over overlapping rows have one color.
    fn assert_one_color_per_lane(g: &Graph, what: &str) {
        let end = |e: &Edge| if e.to == NONE { g.nodes.len() as u32 } else { e.to };
        for a in &g.edges {
            for b in &g.edges {
                if a.lane == b.lane && a.from < end(b) && b.from < end(a) {
                    assert_eq!(a.color, b.color, "{what}: {a:?} and {b:?}");
                }
            }
        }
    }

    /// No line leaves a node along the lane another came into it by: it would read as that line going on.
    fn assert_no_line_goes_on_as_another(g: &Graph, what: &str) {
        for a in g.edges.iter().filter(|a| a.to != NONE && a.lane != a.to_col) {
            let on = g.edges.iter().find(|b| b.from == a.to && b.lane == a.lane && b.lane != b.from_col);
            assert!(on.is_none(), "{what}: {a:?} goes on as {on:?}");
        }
    }

    /// Main's rows sit in column 0 in `MAIN_COLOR` and its own line never bends; no other node or
    /// lane has column 0 or that color, except a merge's line into a main commit.
    fn assert_main_alone(g: &Graph, parents: &[Vec<u32>], main: u32, what: &str) {
        let mut on_main = vec![false; parents.len()];
        let mut r = main;
        while r != NONE {
            on_main[r as usize] = true;
            r = parents[r as usize].first().copied().unwrap_or(NONE);
        }
        assert!(g.main, "{what}");
        for (r, n) in g.nodes.iter().enumerate() {
            assert_eq!((n.col == 0, n.color == MAIN_COLOR), (on_main[r], on_main[r]), "{what}: row {r}");
        }
        for (k, e) in g.edges.iter().enumerate() {
            let into_main = e.to != NONE && on_main[e.to as usize];
            let own = on_main[e.from as usize] && e.lane == 0;
            if e.lane == 0 {
                assert!(e.color == MAIN_COLOR && e.from >= main && (own || into_main), "{what}: {e:?}");
            }
            if e.color == MAIN_COLOR {
                assert!(own || into_main, "{what}: {e:?}");
            }
            // A row's first edge (they are in row order) goes to its first parent.
            if on_main[e.from as usize] && (k == 0 || g.edges[k - 1].from != e.from) {
                assert_eq!((e.from_col, e.lane, e.to_col, e.color), (0, 0, 0, MAIN_COLOR), "{what}: main's line bends");
            }
        }
    }

    #[test]
    fn linear() {
        let g = lay(&[vec![1], vec![2], vec![]], &[5; 3], &seeds(3));
        assert_eq!(cols(&g), [0, 0, 0]);
        assert_eq!(g.edges.len(), 2);
        assert!(g.edges.iter().all(|e| e.lane == 0 && e.to_col == 0));
        assert_eq!(g.width, 1);
    }

    #[test]
    fn branch_and_merge() {
        // 0: merge(1, 2)  1: main  2: feature  3: base
        let g = lay(&[vec![1, 2], vec![3], vec![3], vec![]], &[0, 5, 5, 5], &seeds(4));
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
        let g = lay(&[vec![1, 2], vec![3], vec![3], vec![]], &[0, 5, 5, 5], &seeds(4));
        let merge = g.edges.iter().find(|e| e.from == 0 && e.to == 2).unwrap();
        assert_eq!(merge.color, g.nodes[2].color);
        assert_ne!(g.nodes[2].color, g.nodes[0].color, "the feature keeps its own color");
        // The merged-in commit lands in another lane (its branch went on after the merge): the
        // merge's line still has that branch's color.
        // 0: merge(2, 3)  1: a newer child of 3 on its own lane  2: main  3: merged-in  4: base
        let g = lay(&[vec![2, 3], vec![3], vec![4], vec![4], vec![]], &[0, 1, 0, 5, 0], &seeds(5));
        let merge = g.edges.iter().find(|e| e.from == 0 && e.to == 3).unwrap();
        assert_ne!(merge.lane, g.nodes[3].col, "it lands in the other lane");
        assert_eq!(merge.color, g.nodes[3].color);
    }

    #[test]
    fn a_merge_into_a_branch_that_went_on_joins_its_lane() {
        // 0: feature's newer commit (on 2)  1: merge(3, 2) on main  2: feature  3: main base
        let g = lay(&[vec![2], vec![3, 2], vec![3], vec![]], &[5, 0, 5, 0], &seeds(4));
        let merge = g.edges.iter().find(|e| e.from == 1 && e.to == 2).unwrap();
        assert_eq!((merge.lane, merge.to_col), (g.nodes[2].col, g.nodes[2].col), "no extra lane beside the feature's");
        assert_eq!(g.width, 2);
        assert_eq!(merge.color, g.nodes[2].color);
        assert!(g.edges.iter().all(|e| e.to != NONE), "every edge reaches its parent");
    }

    #[test]
    fn edges_sharing_a_lane_have_one_color() {
        let cases: [(&[&[u32]], &[u32]); 4] = [
            (&[&[3], &[3], &[4, 3], &[5], &[5], &[]], &[5, 1, 2, 1, 2, 1]),
            (&[&[2], &[3, 2], &[3], &[]], &[5, 0, 5, 0]),
            (&[&[2], &[3], &[4, 3], &[4], &[]], &[5, 1, 0, 1, 0]),
            (&[&[3], &[4], &[5, 3], &[5], &[5], &[]], &[5, 1, 2, 1, 2, 0]),
        ];
        for (parents, prio) in cases {
            let parents: Vec<Vec<u32>> = parents.iter().map(|p| p.to_vec()).collect();
            let n = parents.len();
            for k in 0..20 {
                let seed: Vec<u32> = (0..n as u32).map(|i| i * 7 + k).collect();
                assert_one_color_per_lane(&lay(&parents, prio, &seed), &format!("{parents:?} seed {k}"));
            }
        }
    }

    #[test]
    fn main_runs_straight_down_column_0_in_its_own_color() {
        // 0: a branch newer than main  1: its base, on 3  2: origin/main, one ahead of  3: main
        // 4: a feature merging main (5) in  5: main merging g (8)  6: the feature  7: main  8: g  9: root
        let parents: Vec<Vec<u32>> = vec![vec![1], vec![3], vec![3], vec![5], vec![6, 5], vec![7, 8], vec![9], vec![9], vec![9], vec![]];
        let names = vec![vec![1], vec![], vec![0], vec![0], vec![2], vec![], vec![], vec![], vec![3], vec![]];
        let prio = [2, 6, 0, 0, 2, 6, 6, 6, 2, 6];
        for k in 0..20 {
            let seed: Vec<u32> = (0..10).map(|i| i * 7 + k).collect();
            let g = Graph::layout(&parents, &prio, &seed, &names, Some(2));
            assert_main_alone(&g, &parents, 2, &format!("seed {k}"));
            assert_eq!(cols(&g)[..2], [1, 1], "the newer branch beside column 0, empty above main's tip");
            let join = g.edges.iter().find(|e| e.from == 4 && e.to == 5).unwrap();
            assert_eq!((join.lane, join.color), (0, MAIN_COLOR), "merging main in joins main's line");
            // WIP continues main's column only right above its tip, else never takes column 0.
            assert_eq!(g.with_wip(Some(2), false).nodes[0].col, 0);
            assert!([Some(0), Some(1), Some(3), Some(6), None].iter().all(|&h| g.with_wip(h, h.is_none()).nodes[0].col != 0));
            assert_ne!(g.with_wip(None, false).nodes[0].color, MAIN_COLOR);
            assert!(g.with_wip(Some(0), false).main);
        }
        // No main chain: the newest branch takes column 0, as before.
        let g = Graph::layout(&parents, &prio, &seeds(10), &names, None);
        assert!(!g.main && g.nodes[0].col == 0);
    }

    #[test]
    fn a_branch_stacked_on_another_gets_its_own_color() {
        // 0: b's tip  1: b  2: a's tip, b's base (also where b was pushed from: origin/b)  3: a  4: main  5: root
        let parents: Vec<Vec<u32>> = vec![vec![1], vec![2], vec![3], vec![4], vec![5], vec![]];
        for a_tip in [vec![2], vec![2, 1]] {
            let names = vec![vec![1], vec![], a_tip, vec![], vec![0], vec![]];
            for main in [Some(4), None] {
                for k in 0..20 {
                    let seed: Vec<u32> = (0..6).map(|i| i * 7 + k).collect();
                    let g = Graph::layout(&parents, &[2, 6, 2, 6, 0, 6], &seed, &names, main);
                    let c = colors(&g);
                    assert_eq!(c[1], c[0]);
                    assert_ne!(c[2], c[1], "a's tip doesn't take b's color ({names:?})");
                    assert_eq!((c[3], g.nodes[2].col), (c[2], g.nodes[0].col), "a keeps its color down the same lane");
                    let line = |from: u32| g.edges.iter().find(|e| e.from == from).unwrap().color;
                    assert_eq!([line(1), line(2), line(3)], [c[1], c[2], c[2]], "b's line into a's tip stays b's");
                }
            }
        }
    }

    #[test]
    fn a_branch_keeps_its_color_past_its_own_names() {
        let lay_named = |parents: &[Vec<u32>], names: &[Vec<u32>], main| Graph::layout(parents, &vec![6; parents.len()], &seeds(parents.len()), names, Some(main));
        // 0: feature  1: origin/feature (same name), behind it  2: main
        let g = lay_named(&[vec![1], vec![2], vec![]], &[vec![5], vec![5], vec![0]], 2);
        assert_eq!((g.nodes[1].color, g.edges[0].color), (g.nodes[0].color, g.nodes[0].color));
        // 0: an unnamed tip (say, a tag's)  1: f's tip under it: f takes the lane, no new color.
        let g = lay_named(&[vec![1], vec![2], vec![]], &[vec![], vec![6], vec![0]], 2);
        assert_eq!(g.nodes[1].color, g.nodes[0].color);
        // 0: main merging g  1: g, built on  2: h  3: root. A merge's lane is no branch's until it
        // reaches one: here g's, so h's tip starts a color; g deleted, h takes the lane on.
        let parents = [vec![3, 1], vec![2], vec![3], vec![]];
        let g = lay_named(&parents, &[vec![0], vec![7], vec![8], vec![]], 0);
        assert_ne!(g.nodes[2].color, g.nodes[1].color);
        let g = lay_named(&parents, &[vec![0], vec![], vec![8], vec![]], 0);
        assert_eq!(g.nodes[2].color, g.nodes[1].color);
        // 0: develop and staging  3: origin/staging  5: origin/develop, down one lane  6: main. Meeting
        // one of its names doesn't make the lane forget the other.
        let line: Vec<Vec<u32>> = (0..7).map(|i| if i < 6 { vec![i + 1] } else { vec![] }).collect();
        let g = lay_named(&line, &[vec![1, 2], vec![], vec![], vec![2], vec![], vec![1], vec![0]], 6);
        assert!(colors(&g)[..6].iter().all(|&c| c == g.nodes[0].color), "{:?}", colors(&g));
        // 0: b, built on a's tip (3)  1: origin/a, one ahead of a  2: b. a's tip goes on in b's lane
        // (stacked: a color of its own), in the color its upstream came down in.
        let g = lay_named(&[vec![2], vec![3], vec![3], vec![4], vec![]], &[vec![1], vec![2], vec![], vec![2], vec![0]], 4);
        assert_eq!(g.nodes[3].col, g.nodes[0].col);
        assert_eq!(g.nodes[3].color, g.nodes[1].color);
        assert_ne!(g.nodes[3].color, g.nodes[0].color);
    }

    #[test]
    fn random_histories_keep_main_alone_and_lanes_one_color() {
        let mut x = 0x2545_f491_4f6c_dd1d_u64;
        let mut rnd = |m: u32| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        // Row-ordered DAGs: mostly near first parents, some merges, roots and unloaded parents;
        // random branch names (shared ones too), priorities and main tip.
        for case in 0..300 {
            let n = 2 + rnd(40) as usize;
            let parents: Vec<Vec<u32>> = (0..n as u32)
                .map(|i| {
                    let below = n as u32 - 1 - i;
                    match rnd(12) {
                        _ if below == 0 => vec![],
                        0 => vec![],
                        1 => vec![NONE],
                        r => {
                            let (p, q) = (i + 1 + rnd(below.min(4)), i + 1 + rnd(below));
                            if r < 5 && q != p { vec![p, q] } else { vec![p] }
                        }
                    }
                })
                .collect();
            let names: Vec<Vec<u32>> = (0..n)
                .map(|_| match rnd(6) {
                    0 => vec![rnd(4)],
                    1 => vec![rnd(4), rnd(4)],
                    _ => vec![],
                })
                .collect();
            let prio: Vec<u32> = (0..n).map(|_| rnd(7)).collect();
            let seed: Vec<u32> = (0..n).map(|_| rnd(1000)).collect();
            let main = rnd(n as u32);
            let what = format!("case {case}: {parents:?} names {names:?} main {main}");
            let g = Graph::layout(&parents, &prio, &seed, &names, Some(main));
            assert_main_alone(&g, &parents, main, &what);
            assert_one_color_per_lane(&g, &what);
            assert_no_line_goes_on_as_another(&g, &what);
            let g = Graph::layout(&parents, &prio, &seed, &names, None);
            assert_one_color_per_lane(&g, &what);
            assert_no_line_goes_on_as_another(&g, &what);
        }
    }

    #[test]
    fn a_merge_leaves_a_node_in_a_lane_of_its_own() {
        // 0: a feature merging main in  1: main's tip, a PR's merge (4)  2: the feature  3: main  4: the PR  5: root.
        // The line from 0 bends into main's tip; the PR's line must not leave along the lane it came down.
        let parents: Vec<Vec<u32>> = vec![vec![2, 1], vec![3, 4], vec![5], vec![5], vec![3], vec![]];
        let g = Graph::layout(&parents, &[2, 0, 6, 6, 6, 6], &seeds(6), &[vec![1], vec![0], vec![], vec![], vec![], vec![]], Some(1));
        let (merged, pr) = (g.edges.iter().find(|e| e.from == 0 && e.to == 1).unwrap(), g.edges.iter().find(|e| e.from == 1 && e.to == 4).unwrap());
        assert_eq!((merged.lane, merged.to_col), (2, 0));
        assert_ne!(pr.lane, merged.lane);
        assert_no_line_goes_on_as_another(&g, "");
    }

    #[test]
    fn a_stacked_tip_never_borrows_the_color_it_leaves() {
        // 0: origin/a, one ahead of a  1..=8: other branches  9: b, built on a's tip  10: a's tip  11: main.
        // With nine lanes open, origin/a's lane has b's color: a's tip must not take it.
        let mut parents = vec![vec![10]];
        parents.extend((0..8).map(|_| vec![11]));
        parents.extend([vec![10], vec![11], vec![]]);
        let mut names = vec![vec![1]];
        names.extend((0..8).map(|j| vec![10 + j]));
        names.extend([vec![2], vec![1], vec![0]]);
        let prio: Vec<u32> = [4].into_iter().chain([2; 10]).chain([0]).collect();
        let g = Graph::layout(&parents, &prio, &(0..12).map(|i| i * 7).collect::<Vec<_>>(), &names, Some(11));
        assert_eq!(g.nodes[0].color, g.nodes[9].color, "the setup: origin/a's lane has b's color");
        assert_eq!(g.nodes[10].col, g.nodes[9].col);
        assert_ne!(g.nodes[10].color, g.nodes[9].color);
    }

    #[test]
    fn diverging_branches_get_different_colors() {
        // 0, 1, 2: three branch tips on the same base 3, far apart from each other.
        let g = lay(&[vec![3], vec![3], vec![3], vec![]], &[5; 4], &[10, 20, 30, 40]);
        let mut c: Vec<u16> = g.nodes[..3].iter().map(|n| n.color).collect();
        c.sort();
        c.dedup();
        assert_eq!(c.len(), 3, "same seed color, still three colors");
    }

    #[test]
    fn priority_keeps_trunk_straight() {
        // 0: newer feature tip, 1: main's tip; both have parent 2.
        let g = lay(&[vec![2], vec![2], vec![]], &[5, 1, 5], &seeds(3));
        assert_eq!(cols(&g), [0, 1, 1]); // base continues main's lane, feature bends in
        assert_eq!(g.nodes[2].color, g.nodes[1].color);
    }

    #[test]
    fn colors_come_from_the_commits_not_the_rows_above() {
        // 0: merge(1, 2)  1: main  2: feature  3: base; then the same under a new tip.
        let g = lay(&[vec![1, 2], vec![3], vec![3], vec![]], &[5; 4], &[17, 23, 31, 47]);
        assert_eq!(colors(&g), [7, 7, 1, 7]); // the merged-in lane takes its parent's color
        let g2 = lay(&[vec![4], vec![2, 3], vec![4], vec![4], vec![]], &[6, 5, 5, 5, 5], &[99, 17, 23, 31, 47]);
        assert_eq!(colors(&g2)[1..], colors(&g)[..]);
        // Side by side, two lanes with the same seed color still differ.
        let g = lay(&[vec![2], vec![2], vec![]], &[5; 3], &[4, 14, 0]);
        assert_eq!(colors(&g)[..2], [4, 5]);
    }

    #[test]
    fn wip_row_keeps_every_column_and_color() {
        // 0: feature tip  1: main tip (HEAD)  2: base
        let g = lay(&[vec![2], vec![2], vec![]], &[5, 1, 5], &seeds(3));
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
        let g = lay(&[vec![1, 2], vec![3], vec![3], vec![], vec![NONE]], &[5; 5], &seeds(5));
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
        let g = lay(&parents, &vec![5; n], &seeds(n));
        assert_eq!(g.edges.last().unwrap().to, NONE);
        let got: std::collections::HashSet<u32> = g.edges_in(100..200).map(|e| e.from).collect();
        assert_eq!(got.len(), g.edges_in(100..200).count()); // no duplicates
        assert!((99..200).all(|i| got.contains(&i)));
        assert_eq!(g.edges_in(0..n).count(), n);
    }
}
