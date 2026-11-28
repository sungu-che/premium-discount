//! Score matrices `matrix[field][line]` (logis `ai_utils`): rows are the things being placed (fields, outcomes,
//! spans), columns the slots they compete for (lines, records, domains). Results are per field
//! `Option<(line, score, margin)>`, and each line goes to at most one field.
//!
//! **Absent cells.** A negative cell (`< 0`) means "not a candidate" and is ignored by [`double_center`]; the
//! centered output marks absent cells [`ABSENT`] (−1.0). Residuals can be negative too, so assign centered
//! matrices with `abs_threshold > −1` (usually 0) to keep absent cells out. In the assigners a missing (ragged)
//! cell reads as [`ABSENT`] and a non-finite one as −∞; both deviations from logis only matter for NaN input.

/// Marker of an absent cell (input: any negative value; output of [`double_center`]: exactly this).
pub const ABSENT: f32 = -1.0;

/// Per field: the line it got, with its score and margin, or `None`.
pub type Assignment = Vec<Option<(usize, f32, f32)>>;

/// Removes row and column means (`v − line_mean − field_mean + global_mean`) over present cells, so a field that
/// scores high everywhere (or a line everyone likes) stops winning by level alone. A row or column with a single
/// present cell uses the global mean instead of its own.
pub fn double_center(raw: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let field_count = raw.len();
    if field_count == 0 {
        return Vec::new();
    }
    let line_count = raw.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = vec![vec![ABSENT; line_count]; field_count];
    if line_count == 0 {
        return out;
    }
    let present = |v: f32| v.is_finite() && v >= 0.0;

    let mut line_sum = vec![0.0f32; line_count];
    let mut line_cnt = vec![0usize; line_count];
    let mut field_sum = vec![0.0f32; field_count];
    let mut field_cnt = vec![0usize; field_count];
    let mut global_sum = 0.0f32;
    let mut global_cnt = 0usize;
    for (f, row) in raw.iter().enumerate() {
        for (l, &v) in row.iter().enumerate() {
            if !present(v) {
                continue;
            }
            line_sum[l] += v;
            line_cnt[l] += 1;
            field_sum[f] += v;
            field_cnt[f] += 1;
            global_sum += v;
            global_cnt += 1;
        }
    }
    let global_mean = if global_cnt > 0 { global_sum / (global_cnt as f32) } else { 0.0 };
    for (f, row) in raw.iter().enumerate() {
        for (l, &v) in row.iter().enumerate() {
            if !present(v) {
                continue;
            }
            let lm = if line_cnt[l] > 1 {
                line_sum[l] / (line_cnt[l] as f32)
            } else {
                global_mean
            };
            let fm = if field_cnt[f] > 1 {
                field_sum[f] / (field_cnt[f] as f32)
            } else {
                global_mean
            };
            out[f][l] = v - lm - fm + global_mean;
        }
    }
    out
}

/// [`double_center`] that never wipes a lone candidate. A single present row or column is already centred on
/// its own mean there (1-D), so the one shape that degenerates is a lone present cell: `v − v − v + v = 0.0`.
/// That cell keeps its raw value here; every other matrix gets exactly [`double_center`]. Absent cells stay
/// [`ABSENT`].
pub fn double_center_guarded(raw: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let present = |v: f32| v.is_finite() && v >= 0.0;
    let mut cells = raw
        .iter()
        .enumerate()
        .flat_map(|(f, row)| row.iter().enumerate().filter(|&(_, &v)| present(v)).map(move |(l, &v)| (f, l, v)));
    if let (Some((f, l, v)), None) = (cells.next(), cells.next()) {
        let line_count = raw.iter().map(Vec::len).max().unwrap_or(0);
        let mut out = vec![vec![ABSENT; line_count]; raw.len()];
        out[f][l] = v;
        return out;
    }
    double_center(raw)
}

/// Shape of one line's competition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TieKind {
    /// The top field stands clear of the pack; safe to assign.
    Decisive,
    /// Top two are close, the rest far below: a structural confusion between two fields.
    PairConfusion,
    /// The whole pack is level: no discrimination (or a value fitting several fields).
    Flat,
}

/// One gated top claim of [`exclusive_assign_adaptive`], accepted or held back, for logs and the confusion ledger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssignDiag {
    pub field: usize,
    pub rival: usize,
    pub line: usize,
    pub own: f32,
    /// Top-1 minus top-2 on the line.
    pub margin: f32,
    /// Tail σ of the line (2nd place and below).
    pub noise_margin: f32,
    pub tie: TieKind,
    pub accepted: bool,
}

fn line_count(matrix: &[Vec<f32>]) -> usize {
    matrix.iter().map(Vec::len).max().unwrap_or(0)
}

fn cell(matrix: &[Vec<f32>], f: usize, l: usize) -> f32 {
    match matrix.get(f).and_then(|row| row.get(l)) {
        Some(v) if v.is_finite() => *v,
        Some(_) => f32::NEG_INFINITY,
        None => ABSENT,
    }
}

type Claim = (usize, usize, f32, f32);

/// Claims `(field, line, own, margin)` passing both thresholds; `gate` may veto one (and record why).
/// Precomputes per-line `(top1, top1_field, top2)` over cells reaching `abs_threshold` so rival lookup
/// is O(1) instead of O(F), reducing total complexity from O(F² · L) to O(F · L) with identical
/// floating-point arithmetic (`own - rival`, falling back to `abs_threshold` when no rival qualifies).
fn claims(matrix: &[Vec<f32>], abs_threshold: f32, margin_threshold: f32, mut gate: impl FnMut(usize, usize, f32) -> bool) -> Vec<Claim> {
    let lines = line_count(matrix);
    let mut top1_val = vec![f32::MIN; lines];
    let mut top1_f = vec![usize::MAX; lines];
    let mut top2_val = vec![f32::MIN; lines];
    for f in 0..matrix.len() {
        for l in 0..lines {
            let s = cell(matrix, f, l);
            if s < abs_threshold {
                continue;
            }
            if s > top1_val[l] {
                top2_val[l] = top1_val[l];
                top1_val[l] = s;
                top1_f[l] = f;
            } else if s > top2_val[l] {
                top2_val[l] = s;
            }
        }
    }

    let mut out = Vec::new();
    for f in 0..matrix.len() {
        for l in 0..lines {
            let own = cell(matrix, f, l);
            if own < abs_threshold {
                continue;
            }
            let rival = if f == top1_f[l] { top2_val[l] } else { top1_val[l] };
            let margin = own - if rival == f32::MIN { abs_threshold } else { rival };
            if margin < margin_threshold {
                continue;
            }
            if gate(f, l, own) {
                out.push((f, l, own, margin));
            }
        }
    }
    out
}

/// Greedy 1:1: claims in the given order, each field and each line taken once.
fn greedy(claims: Vec<Claim>, field_count: usize, line_count: usize) -> Assignment {
    let mut result = vec![None; field_count];
    let mut claimed = vec![false; line_count];
    for (f, l, own, margin) in claims {
        if result[f].is_some() || claimed[l] {
            continue;
        }
        result[f] = Some((l, own, margin));
        claimed[l] = true;
    }
    result
}

fn by_margin(a: &Claim, b: &Claim) -> std::cmp::Ordering {
    b.3.partial_cmp(&a.3)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
}

fn by_score(a: &Claim, b: &Claim) -> std::cmp::Ordering {
    b.2.partial_cmp(&a.2)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
}

/// 1:1 assignment, strongest margin first: a cell is a claim when it reaches `abs_threshold` and beats every rival
/// on its line by `margin_threshold`.
pub fn exclusive_assign(matrix: &[Vec<f32>], abs_threshold: f32, margin_threshold: f32) -> Assignment {
    let mut c = claims(matrix, abs_threshold, margin_threshold, |_, _, _| true);
    c.sort_by(by_margin);
    greedy(c, matrix.len(), line_count(matrix))
}

/// Like [`exclusive_assign`] but the highest score goes first (margin breaks equal scores).
pub fn exclusive_assign_by_score(matrix: &[Vec<f32>], abs_threshold: f32, margin_threshold: f32) -> Assignment {
    let mut c = claims(matrix, abs_threshold, margin_threshold, |_, _, _| true);
    c.sort_by(by_score);
    greedy(c, matrix.len(), line_count(matrix))
}

/// Competition shape of `line`: `(kind, top field, second field, top1 − top2, noise)` where noise is the σ of the
/// tail (2nd place and below) among cells reaching `abs_threshold`. `None` below three candidates (no noise
/// estimate; callers keep the plain margin rule). A tail without spread has noise 0: [`TieKind::Decisive`] when the
/// top stands above it, [`TieKind::Flat`] when every candidate is level.
pub fn line_tie_shape(matrix: &[Vec<f32>], line: usize, abs_threshold: f32) -> Option<(TieKind, usize, usize, f32, f32)> {
    let mut cands: Vec<(usize, f32)> = Vec::new();
    for (f, row) in matrix.iter().enumerate() {
        let Some(&v) = row.get(line) else { continue };
        if !v.is_finite() || v < abs_threshold {
            continue;
        }
        cands.push((f, v));
    }
    if cands.len() < 3 {
        return None;
    }
    cands.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let m12 = cands[0].1 - cands[1].1;
    let m23 = cands[1].1 - cands[2].1;
    let tail: Vec<f32> = cands[1..].iter().map(|(_, s)| *s).collect();
    let n = tail.len() as f32;
    let mean = tail.iter().sum::<f32>() / n;
    let var = tail.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
    let noise = var.max(0.0).sqrt();
    if noise <= 1e-6 {
        // A tail without spread: decisive when the top stands above it, a flat tie when it does not (every field level).
        let kind = if m12 > 1e-6 { TieKind::Decisive } else { TieKind::Flat };
        return Some((kind, cands[0].0, cands[1].0, m12, 0.0));
    }
    let kind = if m12 >= noise {
        TieKind::Decisive
    } else if m23 > noise {
        TieKind::PairConfusion
    } else {
        TieKind::Flat
    };
    Some((kind, cands[0].0, cands[1].0, m12, noise))
}

/// [`exclusive_assign_by_score`] with a noise-margin gate (logis `exclusive_assign_by_score_adaptive`, T-2): when a
/// field tops a line whose shape is not [`TieKind::Decisive`], `allow_tie(field, rival, kind)` decides whether the
/// claim stands — typically `kind != Flat && ledger.tie_break_allows(..)`; always `false` holds every tie back.
/// Every gated top claim (decisive ones too) is reported in the diagnostics. Lines with fewer than three candidates
/// are not gated.
pub fn exclusive_assign_adaptive(
    matrix: &[Vec<f32>],
    abs_threshold: f32,
    margin_threshold: f32,
    allow_tie: impl Fn(usize, usize, TieKind) -> bool,
) -> (Assignment, Vec<AssignDiag>) {
    let lines = line_count(matrix);
    let shapes: Vec<_> = (0..lines).map(|l| line_tie_shape(matrix, l, abs_threshold)).collect();
    let mut diags = Vec::new();
    let mut c = claims(matrix, abs_threshold, margin_threshold, |f, l, own| {
        let Some((tie, top, rival, margin, noise_margin)) = shapes[l] else {
            return true;
        };
        if top != f {
            return true;
        }
        let accepted = tie == TieKind::Decisive || allow_tie(f, rival, tie);
        diags.push(AssignDiag {
            field: f,
            rival,
            line: l,
            own,
            margin,
            noise_margin,
            tie,
            accepted,
        });
        accepted
    });
    c.sort_by(by_score);
    (greedy(c, matrix.len(), lines), diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn guarded_double_center_keeps_a_lone_candidate_and_matches_elsewhere() {
        // two or more present rows and columns: exactly double_center
        let full = vec![vec![0.9, 0.9, 0.9], vec![0.6, 0.2, 0.2], vec![0.2, 0.2, 0.6]];
        assert_eq!(double_center_guarded(&full), double_center(&full));
        // a single row or column: centred on its own mean, as double_center already does
        let row = vec![vec![0.7, 0.4, ABSENT, 0.1]];
        assert_eq!(double_center_guarded(&row), double_center(&row));
        assert!(near(double_center_guarded(&row)[0][0], 0.3), "{:?}", double_center_guarded(&row));
        let col = vec![vec![0.5], vec![0.9]];
        assert_eq!(double_center_guarded(&col), double_center(&col));
        // a lone present cell: double_center turns it into 0.0, the guard keeps it
        let lone = vec![vec![0.83]];
        assert_eq!(double_center(&lone)[0][0], 0.0);
        assert_eq!(double_center_guarded(&lone), vec![vec![0.83]]);
        let sparse = vec![vec![ABSENT, ABSENT], vec![ABSENT, 0.61], vec![ABSENT, ABSENT]];
        assert_eq!(double_center(&sparse)[1][1], 0.0);
        let g = double_center_guarded(&sparse);
        assert_eq!(g[1][1], 0.61);
        assert!(g.iter().flatten().filter(|&&v| v != 0.61).all(|&v| v == ABSENT));
        assert!(double_center_guarded(&[]).is_empty());
    }

    #[test]
    fn double_center_removes_row_and_column_means() {
        // Field 0 scores high on every line; fields 1 and 2 each prefer one line.
        let raw = vec![vec![0.9, 0.9, 0.9], vec![0.6, 0.2, 0.2], vec![0.2, 0.2, 0.6]];
        let c = double_center(&raw);
        for row in &c {
            assert!(near(row.iter().sum::<f32>() / 3.0, 0.0), "{row:?}");
        }
        for l in 0..3 {
            assert!(near(c.iter().map(|r| r[l]).sum::<f32>() / 3.0, 0.0));
        }
        assert!(c[0].iter().all(|v| v.abs() < 0.1), "the uniform elevation is gone: {:?}", c[0]);
        assert!(near(c[1][0], 0.2222) && near(c[2][2], 0.2222) && near(c[0][1], 0.0889), "{c:?}");

        // Raw scores: the always-high field takes a line and starves the others.
        assert_eq!(
            exclusive_assign(&raw, 0.0, 0.0).iter().map(|r| r.map(|x| x.0)).collect::<Vec<_>>(),
            vec![Some(1), None, None]
        );
        // Centered: everyone gets the line they actually stand out on.
        assert_eq!(
            exclusive_assign(&c, 0.0, 0.0).iter().map(|r| r.map(|x| x.0)).collect::<Vec<_>>(),
            vec![Some(1), Some(0), Some(2)]
        );
    }

    #[test]
    fn double_center_keeps_absent_cells_absent() {
        let raw = vec![vec![0.5, -1.0, 0.7], vec![0.3, 0.4], vec![f32::NAN, 0.2, 0.1]];
        let c = double_center(&raw);
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|r| r.len() == 3), "ragged rows padded to the widest");
        assert_eq!((c[0][1], c[1][2], c[2][0]), (ABSENT, ABSENT, ABSENT));
        assert!(c.iter().flatten().all(|v| v.is_finite()));
        // Present: 0.5, 0.7, 0.3, 0.4, 0.2, 0.1 → global 0.3667; line 0 has {0.5, 0.3}, field 0 has {0.5, 0.7}.
        assert!(near(c[0][0], 0.5 - 0.4 - 0.6 + 2.2 / 6.0), "{c:?}");
        assert!(double_center(&[]).is_empty());
        assert_eq!(double_center(&[vec![], vec![]]), vec![Vec::<f32>::new(), Vec::new()]);
    }

    #[test]
    fn exclusive_assign_is_one_to_one_with_margin_threshold() {
        let m = vec![vec![0.9, 0.1, 0.2], vec![0.2, 0.8, 0.1], vec![0.1, 0.3, 0.7]];
        let r = exclusive_assign(&m, 0.0, 0.0);
        assert_eq!(r.iter().map(|x| x.unwrap().0).collect::<Vec<_>>(), vec![0, 1, 2]);
        let (l, own, margin) = r[1].unwrap();
        assert_eq!(l, 1);
        assert!(near(own, 0.8) && near(margin, 0.5));

        let strict = exclusive_assign(&m, 0.0, 0.6);
        assert!(strict[0].is_some() && strict[1].is_none() && strict[2].is_none(), "{strict:?}");

        // Below abs_threshold a cell is neither a claim nor a rival: a lone candidate's margin is own − abs.
        let lone = exclusive_assign(&[vec![0.6], vec![0.3]], 0.5, 0.0);
        assert_eq!(lone[1], None);
        assert!(near(lone[0].unwrap().2, 0.1));

        // Both fields want line 0; each line goes to one field only.
        let contested = vec![vec![0.9, 0.5], vec![0.8, 0.1]];
        let by_m = exclusive_assign(&contested, 0.0, -1.0);
        let by_s = exclusive_assign_by_score(&contested, 0.0, -1.0);
        assert_eq!(
            by_m.iter().map(|x| x.map(|y| y.0)).collect::<Vec<_>>(),
            vec![Some(1), Some(0)],
            "margin first: f0's 0.4 on line 1"
        );
        assert_eq!(
            by_s.iter().map(|x| x.map(|y| y.0)).collect::<Vec<_>>(),
            vec![Some(0), Some(1)],
            "score first: f0's 0.9 on line 0"
        );
        assert!(exclusive_assign(&[], 0.0, 0.0).is_empty());
    }

    fn column(xs: &[f32]) -> Vec<Vec<f32>> {
        xs.iter().map(|x| vec![*x]).collect()
    }

    #[test]
    fn line_tie_shapes() {
        let (kind, top, second, m12, noise) = line_tie_shape(&column(&[0.84, 0.85, 0.12, 0.09]), 0, 0.0).unwrap();
        assert_eq!((kind, top, second), (TieKind::PairConfusion, 1, 0));
        assert!(near(m12, 0.01) && noise > m12);
        assert_eq!(
            line_tie_shape(&column(&[0.60, 0.595, 0.58, 0.57, 0.55]), 0, 0.0).unwrap().0,
            TieKind::Flat
        );
        assert_eq!(
            line_tie_shape(&column(&[0.9, 0.5, 0.45, 0.4]), 0, 0.0).unwrap().0,
            TieKind::Decisive
        );
        let uniform = line_tie_shape(&column(&[0.9, 0.5, 0.5]), 0, 0.0).unwrap();
        assert_eq!((uniform.0, uniform.4), (TieKind::Decisive, 0.0));
        // every candidate level (e.g. same-label cells with no other context): a flat tie, not a decisive top
        let level = line_tie_shape(&column(&[0.0, 0.0, 0.0, 0.0]), 0, 0.0).unwrap();
        assert_eq!((level.0, level.3, level.4), (TieKind::Flat, 0.0, 0.0));
        assert_eq!(
            line_tie_shape(&column(&[0.9, 0.5, -0.2]), 0, 0.0),
            None,
            "fewer than three candidates"
        );
        assert_eq!(line_tie_shape(&column(&[0.9, 0.5, 0.4]), 3, 0.0), None, "line out of range");
    }

    #[test]
    fn adaptive_assign_defers_ties_to_the_closure() {
        // Line 0: f0 and f1 neck and neck, the rest far below (pair confusion). Line 1: f2 clearly ahead.
        let m = vec![vec![0.85, 0.1], vec![0.84, 0.2], vec![0.12, 0.9], vec![0.09, 0.3]];
        let calls = RefCell::new(Vec::new());
        let (held, diags) = exclusive_assign_adaptive(&m, 0.0, 0.0, |f, r, k| {
            calls.borrow_mut().push((f, r, k));
            false
        });
        assert_eq!(
            *calls.borrow(),
            vec![(0, 1, TieKind::PairConfusion)],
            "only the non-decisive top claim is asked"
        );
        assert_eq!(held[0], None, "tie held back");
        assert_eq!(held[2].map(|x| x.0), Some(1));
        assert_eq!(diags.len(), 2);
        assert_eq!(
            (diags[0].field, diags[0].rival, diags[0].line, diags[0].tie, diags[0].accepted),
            (0, 1, 0, TieKind::PairConfusion, false)
        );
        assert!(near(diags[0].margin, 0.01) && diags[0].noise_margin > 0.01);
        assert_eq!((diags[1].field, diags[1].tie, diags[1].accepted), (2, TieKind::Decisive, true));

        let (allowed, diags) = exclusive_assign_adaptive(&m, 0.0, 0.0, |_, _, k| k != TieKind::Flat);
        assert_eq!(allowed[0].map(|x| x.0), Some(0), "tie broken toward the top field");
        assert!(diags[0].accepted);

        let flat = column(&[0.60, 0.595, 0.58, 0.57, 0.55]);
        let seen = RefCell::new(None);
        let (r, _) = exclusive_assign_adaptive(&flat, 0.0, 0.0, |_, _, k| {
            *seen.borrow_mut() = Some(k);
            k != TieKind::Flat
        });
        assert_eq!(*seen.borrow(), Some(TieKind::Flat));
        assert!(r.iter().all(Option::is_none));
    }
}
