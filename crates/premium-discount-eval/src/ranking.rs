//! Ranking metrics, character error rate, deterministic SplitMix64 paired bootstrap CIs,
//! effective decode timing without double multiplication, and E3 / non-reversal gates.

use std::collections::{BTreeMap, HashSet};

/// Metric optimization direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    HigherIsBetter,
    LowerIsBetter,
}

/// Multi-level equivalence certification tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EquivalenceTier {
    E0,
    E1,
    PartialE2,
    FullE2,
    E3,
}

/// Gate exit codes matching CI scripts: 0 pass, 1 regression, 2 audit error.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateExit {
    Pass = 0,
    Regressed = 1,
    AuditError = 2,
}

impl GateExit {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Mean Reciprocal Rank for one query (`0.0` if no relevant key appears).
pub fn mrr(ranked: &[&str], relevant: &[&str]) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    for (idx, &k) in ranked.iter().enumerate() {
        if rel.contains(k) {
            return 1.0 / (idx as f64 + 1.0);
        }
    }
    0.0
}

/// Recall@k computed identically to `score_search.py`: `hits_in_top_k / min(k, unique_relevant)`.
pub fn recall_at_k(ranked: &[&str], relevant: &[&str], k: usize) -> f64 {
    if k == 0 || relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    let denom = k.min(rel.len()).max(1);
    let hits: HashSet<&str> = ranked.iter().take(k).copied().filter(|x| rel.contains(x)).collect();
    hits.len() as f64 / denom as f64
}

/// Normalized Discounted Cumulative Gain @ k (`1 / log2(rank + 2)`).
pub fn ndcg_at_k(ranked: &[&str], relevant: &[&str], k: usize) -> f64 {
    if k == 0 || relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut dcg = 0.0;
    for (i, &item) in ranked.iter().take(k).enumerate() {
        if rel.contains(item) && seen.insert(item) {
            dcg += 1.0 / ((i + 2) as f64).log2();
        }
    }
    let ideal_len = k.min(rel.len());
    let ideal: f64 = (0..ideal_len).map(|i| 1.0 / ((i + 2) as f64).log2()).sum();
    if ideal > 0.0 {
        dcg / ideal
    } else {
        0.0
    }
}

/// Character-level error rate: Levenshtein edit distance divided by `max(reference_chars, 1)`.
pub fn cer_char_level(reference: &str, candidate: &str) -> f64 {
    let a: Vec<char> = reference.chars().collect();
    let b: Vec<char> = candidate.chars().collect();
    let denom = a.len().max(1) as f64;
    if a.is_empty() {
        return b.len() as f64;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as f64 / denom
}

/// Deterministic SplitMix64 PRNG so bootstrap intervals are bit-identical across OSes.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn next_usize(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        (self.next_u64() % (bound as u64)) as usize
    }
}

/// Paired bootstrap 95% empirical confidence interval `(ci_lo, ci_hi)` over `diffs = candidate - baseline`.
pub fn paired_bootstrap_ci95(diffs: &[f64], n_boot: usize, seed: u64) -> (f64, f64) {
    if diffs.is_empty() {
        return (0.0, 0.0);
    }
    let n = diffs.len();
    if n_boot == 0 {
        let m = diffs.iter().sum::<f64>() / n as f64;
        return (m, m);
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(n_boot);
    for _ in 0..n_boot {
        let mut acc = 0.0;
        for _ in 0..n {
            acc += diffs[rng.next_usize(n)];
        }
        means.push(acc / n as f64);
    }
    means.sort_by(|a, b| a.total_cmp(b));
    let lo_idx = ((n_boot as f64 * 0.025).floor() as usize).min(n_boot - 1);
    let hi_idx = ((n_boot as f64 * 0.975).floor() as usize).min(n_boot - 1);
    (means[lo_idx], means[hi_idx])
}

/// The criteria of one polarity-separated E3 slice (a language, a persona …)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct E3Criteria {
    pub polarity: Polarity,
    /// Paired bootstrap bound: `CI95_lo(d) >= -delta_max` (higher is better) or `CI95_hi(d) <= +delta_max` (lower is better)
    pub delta_max: f64,
    /// Absolute bound on the candidate's mean: `>= tau` (higher is better) or `<= tau` (lower is better); None = no bound
    pub tau: Option<f64>,
    /// Every subgroup's mean difference: `>= floor` (higher is better, e.g. -0.050) or `<= floor` (lower is better, e.g. +0.010)
    pub subgroup_floor: f64,
}

impl E3Criteria {
    /// Higher is better (completion, Recall@10, MRR, nDCG, shortcut win rate, tIoU), subgroup floor -0.050
    pub fn higher(delta_max: f64, tau_min: Option<f64>) -> E3Criteria {
        E3Criteria { polarity: Polarity::HigherIsBetter, delta_max, tau: tau_min, subgroup_floor: -0.050 }
    }

    /// Lower is better (CER, ECE, false fault penalty rate, RMSE), subgroup ceiling +0.010
    pub fn lower(delta_max: f64, tau_max: Option<f64>) -> E3Criteria {
        E3Criteria { polarity: Polarity::LowerIsBetter, delta_max, tau: tau_max, subgroup_floor: 0.010 }
    }
}

/// What [`check_e3_polarity_split`] measured
#[derive(Clone, Debug, PartialEq)]
pub struct E3Outcome {
    pub exit: GateExit,
    pub candidate_mean: f64,
    pub ci95: (f64, f64),
    /// The subgroup with the worst mean difference (lowest for higher-is-better, highest for lower-is-better)
    pub worst_subgroup: Option<(String, f64)>,
}

/// Polarity-separated E3 on one slice: the paired bootstrap bound of [`check_e3_non_inferiority`], the absolute bound on
/// the candidate mean and the bound on every subgroup's mean difference. `subgroups[i]` names the subgroup of pair `i`
/// (None = one group). Non-finite input or mismatched lengths are an audit error.
pub fn check_e3_polarity_split(
    baseline: &[f64],
    candidate: &[f64],
    subgroups: Option<&[&str]>,
    criteria: &E3Criteria,
    n_boot: usize,
    seed: u64,
) -> E3Outcome {
    let audit = |m: f64| E3Outcome { exit: GateExit::AuditError, candidate_mean: m, ci95: (0.0, 0.0), worst_subgroup: None };
    if baseline.is_empty()
        || baseline.len() != candidate.len()
        || subgroups.is_some_and(|g| g.len() != baseline.len())
        || baseline.iter().chain(candidate).any(|x| !x.is_finite())
        || !criteria.delta_max.is_finite()
        || criteria.delta_max < 0.0
        || criteria.tau.is_some_and(|t| !t.is_finite())
        || !criteria.subgroup_floor.is_finite()
    {
        return audit(f64::NAN);
    }
    let diffs: Vec<f64> = candidate.iter().zip(baseline).map(|(c, b)| c - b).collect();
    let candidate_mean = candidate.iter().sum::<f64>() / candidate.len() as f64;
    let ci95 = paired_bootstrap_ci95(&diffs, n_boot, seed);
    let higher = criteria.polarity == Polarity::HigherIsBetter;
    let ci_ok = if higher { ci95.0 >= -criteria.delta_max } else { ci95.1 <= criteria.delta_max };
    let tau_ok = criteria.tau.is_none_or(|t| if higher { candidate_mean >= t } else { candidate_mean <= t });
    let mut groups: std::collections::BTreeMap<&str, (f64, usize)> = std::collections::BTreeMap::new();
    for (i, d) in diffs.iter().enumerate() {
        let g = subgroups.map_or("", |s| s[i]);
        let e = groups.entry(g).or_insert((0.0, 0));
        e.0 += d;
        e.1 += 1;
    }
    let means = groups.into_iter().map(|(g, (sum, n))| (g.to_string(), sum / n as f64));
    let worst_subgroup = if higher { means.min_by(|a, b| a.1.total_cmp(&b.1)) } else { means.max_by(|a, b| a.1.total_cmp(&b.1)) };
    let sub_ok = worst_subgroup.as_ref().is_none_or(|(_, m)| if higher { *m >= criteria.subgroup_floor } else { *m <= criteria.subgroup_floor });
    let exit = if ci_ok && tau_ok && sub_ok { GateExit::Pass } else { GateExit::Regressed };
    E3Outcome { exit, candidate_mean, ci95, worst_subgroup }
}

/// Every slice must pass [`check_e3_polarity_split`] (slice `i` uses seed `seed + i`): one regressed slice fails the
/// whole gate even when every other slice improved; the first audit error wins.
pub fn check_e3_slices(slices: &[(&[f64], &[f64], Option<&[&str]>, E3Criteria)], n_boot: usize, seed: u64) -> GateExit {
    let mut exit = GateExit::Pass;
    for (i, (base, cand, groups, c)) in slices.iter().enumerate() {
        match check_e3_polarity_split(base, cand, *groups, c, n_boot, seed.wrapping_add(i as u64)).exit {
            GateExit::AuditError => return GateExit::AuditError,
            GateExit::Regressed => exit = GateExit::Regressed,
            GateExit::Pass => {}
        }
    }
    if slices.is_empty() {
        GateExit::AuditError
    } else {
        exit
    }
}

/// Effective decode ms per reference token `sum(decode_ms_1) / sum(n_gen_0)` without double-multiplying.
pub fn effective_ms_1(decode_ms_1: &[f64], n_gen_0: &[u64]) -> Result<f64, GateExit> {
    if decode_ms_1.is_empty() || decode_ms_1.len() != n_gen_0.len() {
        return Err(GateExit::AuditError);
    }
    let mut tot_ms = 0.0f64;
    let mut tot_tok = 0u64;
    for (&ms, &tok) in decode_ms_1.iter().zip(n_gen_0.iter()) {
        if !ms.is_finite() || ms < 0.0 {
            return Err(GateExit::AuditError);
        }
        tot_ms += ms;
        tot_tok = tot_tok.saturating_add(tok);
    }
    if tot_tok == 0 {
        return Err(GateExit::AuditError);
    }
    Ok(tot_ms / tot_tok as f64)
}

/// Checks paired E3 non-inferiority respecting [`Polarity`].
///
/// For [`Polarity::HigherIsBetter`], requires `ci_lo >= -delta_max`.
/// For [`Polarity::LowerIsBetter`], prevents sign inversion by requiring `ci_hi <= +delta_max`.
pub fn check_e3_non_inferiority(
    baseline: &[f64],
    candidate: &[f64],
    polarity: Polarity,
    delta_max: f64,
    n_boot: usize,
    seed: u64,
) -> GateExit {
    if baseline.is_empty() || baseline.len() != candidate.len() || !delta_max.is_finite() || delta_max < 0.0 {
        return GateExit::AuditError;
    }
    let mut diffs = Vec::with_capacity(baseline.len());
    for (&b, &c) in baseline.iter().zip(candidate.iter()) {
        if !b.is_finite() || !c.is_finite() {
            return GateExit::AuditError;
        }
        diffs.push(c - b);
    }
    let (ci_lo, ci_hi) = paired_bootstrap_ci95(&diffs, n_boot, seed);
    let ok = match polarity {
        Polarity::HigherIsBetter => ci_lo >= -delta_max,
        Polarity::LowerIsBetter => ci_hi <= delta_max,
    };
    if ok {
        GateExit::Pass
    } else {
        GateExit::Regressed
    }
}

/// Ensures candidate neither expands beyond `max_ratio` relative tolerance nor reverses polarity on mean.
pub fn check_expansion_and_non_reversal(
    baseline: &[f64],
    candidate: &[f64],
    polarity: Polarity,
    max_ratio: f64,
) -> GateExit {
    if baseline.is_empty() || baseline.len() != candidate.len() || !max_ratio.is_finite() || max_ratio < 0.0 {
        return GateExit::AuditError;
    }
    let mut sum_b = 0.0;
    let mut sum_c = 0.0;
    for (&b, &c) in baseline.iter().zip(candidate.iter()) {
        if !b.is_finite() || !c.is_finite() {
            return GateExit::AuditError;
        }
        sum_b += b;
        sum_c += c;
    }
    let mean_b = sum_b / baseline.len() as f64;
    let mean_c = sum_c / candidate.len() as f64;
    let scale = mean_b.abs().max(1e-12);
    let ok = match polarity {
        Polarity::HigherIsBetter => mean_c >= mean_b || ((mean_b - mean_c) / scale) <= max_ratio,
        Polarity::LowerIsBetter => mean_c <= mean_b || ((mean_c - mean_b) / scale) <= max_ratio,
    };
    if ok {
        GateExit::Pass
    } else {
        GateExit::Regressed
    }
}

/// Groups slice metrics in deterministic [`BTreeMap`] key order.
pub fn group_means(rows: &[(&str, f64)]) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for &(k, v) in rows {
        if v.is_finite() {
            let e = acc.entry(k.to_string()).or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
    }
    acc.into_iter().map(|(k, (s, n))| (k, s / n.max(1) as f64)).collect()
}

#[cfg(test)]
mod e3_split_tests {
    use super::*;

    #[test]
    fn polarity_split_e3_catches_a_worse_cer_and_a_hidden_minority_slice() {
        // lower is better: candidate CER 0.03 worse on every pair
        let base = vec![0.004; 40];
        let worse: Vec<f64> = base.iter().map(|b| b + 0.03).collect();
        let cer = E3Criteria::lower(0.005, Some(0.005));
        assert_eq!(check_e3_polarity_split(&base, &worse, None, &cer, 2_000, 7).exit, GateExit::Regressed);
        assert_eq!(check_e3_polarity_split(&base, &base, None, &cer, 2_000, 7).exit, GateExit::Pass);
        // higher is better: 14 slices improve, one minority slice drops by 0.04 (inside the -0.05 subgroup floor but
        // outside delta_max 0.010)
        let b = vec![0.80; 30];
        let up: Vec<f64> = b.iter().map(|x| x + 0.02).collect();
        let down: Vec<f64> = b.iter().map(|x| x - 0.04).collect();
        let crit = E3Criteria::higher(0.010, Some(0.70));
        let mut slices: Vec<(&[f64], &[f64], Option<&[&str]>, E3Criteria)> = (0..14).map(|_| (&b[..], &up[..], None, crit)).collect();
        assert_eq!(check_e3_slices(&slices, 2_000, 11), GateExit::Pass);
        slices.push((&b[..], &down[..], None, crit));
        assert_eq!(check_e3_slices(&slices, 2_000, 11), GateExit::Regressed);
        // the subgroup floor: the slice mean is fine but one subgroup falls 0.06
        let groups: Vec<&str> = (0..30).map(|i| if i < 5 { "p1" } else { "rest" }).collect();
        let mixed: Vec<f64> = (0..30).map(|i| if i < 5 { 0.74 } else { 0.83 }).collect();
        let o = check_e3_polarity_split(&b, &mixed, Some(&groups), &E3Criteria::higher(0.05, None), 2_000, 3);
        assert_eq!(o.exit, GateExit::Regressed);
        assert_eq!(o.worst_subgroup.as_ref().map(|(g, _)| g.as_str()), Some("p1"));
        // the absolute bound
        assert_eq!(check_e3_polarity_split(&b, &b, None, &E3Criteria::higher(0.01, Some(0.9)), 2_000, 3).exit, GateExit::Regressed);
        // audit errors
        assert_eq!(check_e3_polarity_split(&b, &b[..3], None, &crit, 10, 1).exit, GateExit::AuditError);
        assert_eq!(check_e3_polarity_split(&[f64::NAN], &[0.0], None, &crit, 10, 1).exit, GateExit::AuditError);
        assert_eq!(check_e3_slices(&[], 10, 1), GateExit::AuditError);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cer_and_lower_is_better_non_inferiority_polarity() {
        assert_eq!(cer_char_level("hello", "hello"), 0.0);
        assert!((cer_char_level("abcd", "abxd") - 0.25).abs() < 1e-9);

        // Regression on LowerIsBetter (CER rises 0.002 -> 0.250) MUST be caught as Regressed!
        let base_bad = vec![0.002; 16];
        let cand_bad = vec![0.250; 16];
        assert_eq!(
            check_e3_non_inferiority(&base_bad, &cand_bad, Polarity::LowerIsBetter, 0.01, 2000, 7),
            GateExit::Regressed
        );

        // Improvement on LowerIsBetter (CER drops 0.015 -> 0.005) MUST Pass!
        let base_ok = vec![0.015; 16];
        let cand_ok = vec![0.005; 16];
        assert_eq!(
            check_e3_non_inferiority(&base_ok, &cand_ok, Polarity::LowerIsBetter, 0.01, 2000, 7),
            GateExit::Pass
        );

        // Sample count mismatch MUST return AuditError (2), not Regressed (1)
        assert_eq!(
            check_e3_non_inferiority(&[0.1, 0.2], &[0.1], Polarity::HigherIsBetter, 0.01, 2000, 7),
            GateExit::AuditError
        );
    }

    #[test]
    fn effective_ms_and_ranking_metrics_are_exact() {
        // 300ms + 300ms over 100 + 200 baseline tokens = 2.0 ms/tok (no double multiplication)
        let eff = effective_ms_1(&[300.0, 300.0], &[100, 200]).unwrap();
        assert!((eff - 2.0).abs() < 1e-12);
        assert_eq!(effective_ms_1(&[300.0], &[100, 200]), Err(GateExit::AuditError));
        assert_eq!(effective_ms_1(&[300.0], &[0]), Err(GateExit::AuditError));

        assert!((mrr(&["b", "a", "c"], &["a"]) - 0.5).abs() < 1e-12);
        assert!((recall_at_k(&["a", "x", "b"], &["a", "b"], 3) - 1.0).abs() < 1e-12);
        assert!(ndcg_at_k(&["a", "b"], &["a", "b"], 2) > 0.999);
    }

    #[test]
    fn bootstrap_is_bit_deterministic_and_expansion_checks_polarity() {
        let diffs = [0.02, -0.01, 0.03, 0.01, 0.0, 0.04, -0.005, 0.015];
        let a = paired_bootstrap_ci95(&diffs, 2000, 7);
        let b = paired_bootstrap_ci95(&diffs, 2000, 7);
        assert_eq!(a.0.to_bits(), b.0.to_bits());
        assert_eq!(a.1.to_bits(), b.1.to_bits());

        assert_eq!(
            check_expansion_and_non_reversal(&[10.0, 10.0], &[10.4, 10.4], Polarity::LowerIsBetter, 0.05),
            GateExit::Pass
        );
        assert_eq!(
            check_expansion_and_non_reversal(&[10.0, 10.0], &[12.0, 12.0], Polarity::LowerIsBetter, 0.05),
            GateExit::Regressed
        );
    }
}
