//! Model-free 1D/2D perforated grid scheduling, conservative hole-filling,
//! and anti-inversion expansion budget guard (`ExpansionBudgetGuard`).
//!
//! Pure arithmetic, `wasm32-unknown-unknown` clean, zero domain literals.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Perforation layout for 2D grids (e.g. patch or supercell grids `(rows, cols)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerforatePattern {
    /// Checkerboard `(r + c) % 2 == 0` (~50% cells).
    Checker,
    /// Dense inner fovea (`inner_margin` inward from border) + checkerboard periphery.
    Fovea { border_ring: usize },
}

/// Configuration for `ExpansionBudgetGuard` to prevent 2nd-pass latency inversion
/// when multi-seed expansion opens too much of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExpansionBudgetCfg {
    /// Maximum primary (active-sector) seed cells to expand around.
    pub max_primary_seeds: usize,
    /// Maximum forced priority (e.g. safety/privacy) seed cells always allowed.
    pub max_priority_seeds: usize,
    /// Maximum Chebyshev expansion radius around each selected seed (default `1`).
    pub radius: usize,
    /// Maximum expansion rounds (default `1`).
    pub max_rounds: usize,
    /// Upper bound on fraction of additional cells opened during expansion (default `0.20`).
    pub max_extra_ratio: f32,
    /// Hard cap on total evaluated fraction `(initial + extra) / total` (default `0.65`).
    /// Exceeding this indicates a dense frame where a single full pass is faster.
    pub max_total_ratio: f32,
}

impl Default for ExpansionBudgetCfg {
    fn default() -> Self {
        Self {
            max_primary_seeds: 2,
            max_priority_seeds: 1,
            radius: 1,
            max_rounds: 1,
            max_extra_ratio: 0.20,
            max_total_ratio: 0.65,
        }
    }
}

/// Decision returned by [`ExpansionBudgetGuard::plan_2d`] / [`ExpansionBudgetGuard::plan_1d`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetDecision {
    /// Open these new (previously closed) cell indices in a single sparse extension pass.
    Expand(Vec<usize>),
    /// No additional cells needed or budget already saturated; finish via conservative `fill_holes`.
    StopConservative,
    /// Predicted total evaluated ratio exceeds `max_total_ratio`; run a single full pass instead.
    FallbackFull,
}

/// Guard preventing 2nd-pass expansion from opening so many cells that two sparse passes
/// become slower than one contiguous full pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpansionBudgetGuard {
    pub cfg: ExpansionBudgetCfg,
}

impl Default for ExpansionBudgetGuard {
    fn default() -> Self {
        Self { cfg: ExpansionBudgetCfg::default() }
    }
}

impl ExpansionBudgetGuard {
    pub fn new(cfg: ExpansionBudgetCfg) -> Self {
        Self { cfg }
    }

    /// Plan 2D neighbor expansion on a `(rows, cols)` grid, strictly respecting `excluded` masks
    /// (e.g. redacted/masked regions that must never be opened) and budget caps.
    pub fn plan_2d(
        &self,
        rows: usize,
        cols: usize,
        initial_open: &[usize],
        primary_seeds: &[usize],
        priority_seeds: &[usize],
        excluded: &[usize],
        round: usize,
    ) -> BudgetDecision {
        let total = rows.saturating_mul(cols);
        if total == 0 || round >= self.cfg.max_rounds {
            return BudgetDecision::StopConservative;
        }
        let open_set: BTreeSet<usize> = initial_open.iter().copied().filter(|&i| i < total).collect();
        let excl_set: BTreeSet<usize> = excluded.iter().copied().filter(|&i| i < total).collect();

        let mut selected_seeds = Vec::new();
        for &s in priority_seeds.iter().take(self.cfg.max_priority_seeds) {
            if s < total && !excl_set.contains(&s) {
                selected_seeds.push(s);
            }
        }
        for &s in primary_seeds.iter().take(self.cfg.max_primary_seeds) {
            if s < total && !excl_set.contains(&s) && !selected_seeds.contains(&s) {
                selected_seeds.push(s);
            }
        }
        if selected_seeds.is_empty() {
            return BudgetDecision::StopConservative;
        }

        let mut candidates = BTreeSet::new();
        let rad = self.cfg.radius as isize;
        for seed in selected_seeds {
            let sr = (seed / cols) as isize;
            let sc = (seed % cols) as isize;
            for dr in -rad..=rad {
                for dc in -rad..=rad {
                    let nr = sr + dr;
                    let nc = sc + dc;
                    if nr >= 0 && nr < rows as isize && nc >= 0 && nc < cols as isize {
                        let idx = (nr as usize) * cols + (nc as usize);
                        if !open_set.contains(&idx) && !excl_set.contains(&idx) {
                            candidates.insert(idx);
                        }
                    }
                }
            }
        }
        if candidates.is_empty() {
            return BudgetDecision::StopConservative;
        }

        let projected_total_ratio = (open_set.len() + candidates.len()) as f32 / total as f32;
        if projected_total_ratio > self.cfg.max_total_ratio {
            return BudgetDecision::FallbackFull;
        }

        let max_extra = ((total as f32) * self.cfg.max_extra_ratio).floor().max(1.0) as usize;
        let mut out: Vec<usize> = candidates.into_iter().collect();
        if out.len() > max_extra {
            out.truncate(max_extra);
        }
        BudgetDecision::Expand(out)
    }

    /// Whether a first sparse pass over `initial_open` of `total` cells already exceeds `max_total_ratio`
    /// (e.g. after forced seeds were merged by [`initial_cells_2d_seeded`]); then one full pass is cheaper
    /// than any sparse schedule and the caller should skip straight to it.
    pub fn initial_over_budget(&self, total: usize, initial_open: usize) -> bool {
        total > 0 && initial_open as f32 / total as f32 > self.cfg.max_total_ratio
    }

    /// Plan 1D neighbor expansion (e.g. long-document paragraph grid or time-window grid).
    pub fn plan_1d(
        &self,
        len: usize,
        initial_open: &[usize],
        primary_seeds: &[usize],
        priority_seeds: &[usize],
        excluded: &[usize],
        round: usize,
    ) -> BudgetDecision {
        self.plan_2d(1, len, initial_open, primary_seeds, priority_seeds, excluded, round)
    }
}

/// Compute initial open cell indices on a 2D `(rows, cols)` grid, skipping `excluded` indices.
pub fn initial_cells_2d(
    rows: usize,
    cols: usize,
    pattern: PerforatePattern,
    excluded: &[usize],
) -> Vec<usize> {
    let total = rows.saturating_mul(cols);
    let excl: BTreeSet<usize> = excluded.iter().copied().collect();
    let mut out = Vec::with_capacity(total / 2 + 1);
    for r in 0..rows {
        for c in 0..cols {
            let idx = r * cols + c;
            if excl.contains(&idx) {
                continue;
            }
            let keep = match pattern {
                PerforatePattern::Checker => (r + c) % 2 == 0,
                PerforatePattern::Fovea { border_ring } => {
                    let in_core = r >= border_ring
                        && r + border_ring < rows
                        && c >= border_ring
                        && c + border_ring < cols;
                    in_core || (r + c) % 2 == 0
                }
            };
            if keep {
                out.push(idx);
            }
        }
    }
    out
}

/// [`initial_cells_2d`] united with forced `priority_seeds` (e.g. a predicted next target, cells that just changed,
/// the last peak of a protected class), so small targets on the border never fall into a checkerboard hole.
/// Seeds out of the grid or in `excluded` are dropped: excluded cells stay closed in every pass. Sorted, unique.
pub fn initial_cells_2d_seeded(
    rows: usize,
    cols: usize,
    pattern: PerforatePattern,
    excluded: &[usize],
    priority_seeds: &[usize],
) -> Vec<usize> {
    let total = rows.saturating_mul(cols);
    let excl: BTreeSet<usize> = excluded.iter().copied().collect();
    let mut keep: BTreeSet<usize> = initial_cells_2d(rows, cols, pattern, excluded).into_iter().collect();
    for &s in priority_seeds {
        if s < total && !excl.contains(&s) {
            keep.insert(s);
        }
    }
    keep.into_iter().collect()
}

/// Compute 1D strided initial indices (`i % stride == 0`) union `pinned` indices, skipping `excluded`.
pub fn initial_cells_1d(len: usize, stride: usize, pinned: &[usize], excluded: &[usize]) -> Vec<usize> {
    let step = stride.max(1);
    let excl: BTreeSet<usize> = excluded.iter().copied().collect();
    let mut open = BTreeSet::new();
    for i in (0..len).step_by(step) {
        if !excl.contains(&i) {
            open.insert(i);
        }
    }
    for &p in pinned {
        if p < len && !excl.contains(&p) {
            open.insert(p);
        }
    }
    open.into_iter().collect()
}

/// Conservative 2D hole-filling: every unopened cell receives the **minimum** score among its
/// 8-connected evaluated neighbors (or `floor_value` if isolated), so unopened holes never fabricate false peaks.
pub fn fill_holes_2d(
    rows: usize,
    cols: usize,
    open_cells: &[usize],
    open_scores: &[f32],
    floor_value: f32,
) -> Vec<f32> {
    let total = rows.saturating_mul(cols);
    let mut grid = vec![f32::NAN; total];
    for (&idx, &score) in open_cells.iter().zip(open_scores.iter()) {
        if idx < total {
            grid[idx] = score;
        }
    }
    let mut filled = grid.clone();
    for r in 0..rows {
        for c in 0..cols {
            let idx = r * cols + c;
            if !grid[idx].is_nan() {
                continue;
            }
            let mut min_neighbor = f32::INFINITY;
            for dr in -1isize..=1 {
                for dc in -1isize..=1 {
                    if dr == 0 && dc == 0 {
                        continue;
                    }
                    let nr = r as isize + dr;
                    let nc = c as isize + dc;
                    if nr >= 0 && nr < rows as isize && nc >= 0 && nc < cols as isize {
                        let v = grid[(nr as usize) * cols + (nc as usize)];
                        if v.is_finite() && v < min_neighbor {
                            min_neighbor = v;
                        }
                    }
                }
            }
            filled[idx] = if min_neighbor.is_finite() { min_neighbor } else { floor_value };
        }
    }
    filled
}

/// Conservative 1D hole-filling: unopened slots receive the minimum evaluated neighbor score.
pub fn fill_holes_1d(len: usize, open_cells: &[usize], open_scores: &[f32], floor_value: f32) -> Vec<f32> {
    fill_holes_2d(1, len, open_cells, open_scores, floor_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_grid_respects_exclusions_and_budget_guard_prevents_inversion() {
        let excluded = vec![0, 5];
        let open = initial_cells_2d(8, 8, PerforatePattern::Checker, &excluded);
        assert!(!open.contains(&0));
        assert!(!open.contains(&5));
        assert!(open.len() <= 32);

        let guard = ExpansionBudgetGuard::default();
        // Normal sparse expansion: 1 primary seed + 1 priority seed -> expands small neighborhood
        let dec = guard.plan_2d(8, 8, &open, &[27], &[18], &excluded, 0);
        match dec {
            BudgetDecision::Expand(extra) => {
                assert!(!extra.is_empty());
                assert!(!extra.contains(&0) && !extra.contains(&5));
                assert!((open.len() + extra.len()) as f32 / 64.0 <= 0.65 + 1e-6);
            }
            other => panic!("expected Expand, got {other:?}"),
        }

        // Dense initial coverage (> 65% once expanded) triggers FallbackFull
        let dense_open: Vec<usize> = (0..40).collect();
        let dense_dec = guard.plan_2d(8, 8, &dense_open, &[42, 45], &[50], &[], 0);
        assert_eq!(dense_dec, BudgetDecision::FallbackFull);

        // Round >= max_rounds stops conservatively
        assert_eq!(
            guard.plan_2d(8, 8, &open, &[27], &[], &[], 1),
            BudgetDecision::StopConservative
        );
    }

    #[test]
    fn conservative_fill_holes_never_invents_false_peak() {
        let open = vec![0, 2, 4, 6, 8];
        let scores = vec![0.2, 0.9, 0.5, 0.1, 0.3];
        let filled = fill_holes_2d(3, 3, &open, &scores, 0.0);
        // Hole at idx 1 (neighbors: 0=0.2, 2=0.9, 4=0.5) must get min=0.2, never 0.9
        assert!((filled[1] - 0.2).abs() < 1e-6, "filled[1]={}", filled[1]);
        // Hole at idx 7 (neighbors: 4=0.5, 6=0.1, 8=0.3) must get min=0.1
        assert!((filled[7] - 0.1).abs() < 1e-6, "filled[7]={}", filled[7]);
    }

    #[test]
    fn seeded_initial_cells_open_every_valid_seed_and_never_an_excluded_cell() {
        // 8 × 8 checkerboard: border cell 7 (row 0, col 7) and 56 (row 7, col 0) are holes of the plain pattern
        let plain = initial_cells_2d(8, 8, PerforatePattern::Checker, &[3]);
        assert!(!plain.contains(&7) && !plain.contains(&56));
        let seeded = initial_cells_2d_seeded(8, 8, PerforatePattern::Checker, &[3, 56], &[7, 56, 3, 999]);
        assert!(seeded.contains(&7), "a valid seed is always opened");
        assert!(!seeded.contains(&56) && !seeded.contains(&3), "excluded seeds stay closed");
        assert!(seeded.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
        assert_eq!(seeded.len(), plain.len() + 1);
        // no seeds: identical to the plain pattern
        assert_eq!(initial_cells_2d_seeded(8, 8, PerforatePattern::Fovea { border_ring: 2 }, &[], &[]),
                   initial_cells_2d(8, 8, PerforatePattern::Fovea { border_ring: 2 }, &[]));
        assert!(initial_cells_2d_seeded(0, 5, PerforatePattern::Checker, &[], &[0]).is_empty());
    }

    #[test]
    fn seeds_that_push_the_first_pass_over_budget_mean_one_full_pass() {
        let guard = ExpansionBudgetGuard::default();
        // Fovea(2) on 6 × 6 opens the 2 × 2 core + half the ring = 20 / 36 ≈ 0.56 ≤ 0.65
        let base = initial_cells_2d(6, 6, PerforatePattern::Fovea { border_ring: 2 }, &[]);
        assert!(!guard.initial_over_budget(36, base.len()), "{} / 36", base.len());
        // five forced ring seeds lift it to 25 / 36 ≈ 0.69 > 0.65
        let holes: Vec<usize> = (0..36).filter(|i| !base.contains(i)).take(5).collect();
        let seeded = initial_cells_2d_seeded(6, 6, PerforatePattern::Fovea { border_ring: 2 }, &[], &holes);
        assert_eq!(seeded.len(), base.len() + 5);
        assert!(guard.initial_over_budget(36, seeded.len()));
        assert!(!guard.initial_over_budget(0, 0));
        // and the second-pass planner agrees once anything more would be opened
        // (seed 35 = row 5, col 5 still has closed neighbours 29 and 34)
        assert_eq!(guard.plan_2d(6, 6, &seeded, &[35], &[], &[], 0), BudgetDecision::FallbackFull);
    }

    #[test]
    fn initial_1d_keeps_all_pinned_cells() {
        let open = initial_cells_1d(10, 2, &[3, 7], &[4]);
        assert!(open.contains(&3) && open.contains(&7));
        assert!(!open.contains(&4));
    }
}
