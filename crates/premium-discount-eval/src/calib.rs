//! AUROC, average precision, ECE, Platt scaling with sparse-positive safe fallback, and threshold calibration.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const ECE_BINS: usize = 15;

pub fn auroc(scores: &[f32], hits: &[bool]) -> Option<f32> {
    let mut v: Vec<(f32, bool)> = scores
        .iter()
        .copied()
        .zip(hits.iter().copied())
        .filter(|(s, _)| s.is_finite())
        .collect();
    let pos = v.iter().filter(|x| x.1).count();
    let neg = v.len() - pos;
    if pos == 0 || neg == 0 {
        return None;
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut rank_sum = 0.0f64;
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1].0 == v[i].0 {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j {
            if v[k].1 {
                rank_sum += avg;
            }
        }
        i = j + 1;
    }
    let u = rank_sum - (pos * (pos + 1)) as f64 / 2.0;
    Some((u / (pos * neg) as f64) as f32)
}

pub fn average_precision(scores: &[f32], hits: &[bool]) -> Option<f32> {
    let mut v: Vec<(f32, bool)> = scores
        .iter()
        .copied()
        .zip(hits.iter().copied())
        .filter(|(s, _)| s.is_finite())
        .collect();
    let pos = v.iter().filter(|x| x.1).count();
    if pos == 0 {
        return None;
    }
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (mut tp, mut ap) = (0usize, 0.0f64);
    for (k, (_, h)) in v.iter().enumerate() {
        if *h {
            tp += 1;
            ap += tp as f64 / (k + 1) as f64;
        }
    }
    Some((ap / pos as f64) as f32)
}

pub fn ece(probs: &[f32], hits: &[bool], bins: usize) -> Option<f32> {
    let n = probs.len().min(hits.len());
    if n == 0 || bins == 0 {
        return None;
    }
    let mut sum_p = vec![0.0f64; bins];
    let mut sum_h = vec![0.0f64; bins];
    let mut cnt = vec![0usize; bins];
    for (p, h) in probs.iter().zip(hits) {
        let clamped = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 };
        let b = ((clamped * bins as f32) as usize).min(bins - 1);
        sum_p[b] += clamped as f64;
        sum_h[b] += *h as u8 as f64;
        cnt[b] += 1;
    }
    let e: f64 = (0..bins)
        .filter(|&b| cnt[b] > 0)
        .map(|b| (sum_h[b] - sum_p[b]).abs())
        .sum::<f64>()
        / n as f64;
    Some(e as f32)
}

/// One label's ranking and calibration quality in a [`MultilabelRankBook`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelRank {
    pub n: usize,
    pub positives: usize,
    pub auroc: Option<f32>,
    pub average_precision: Option<f32>,
    /// [`ECE_BINS`]-bin expected calibration error of the scores read as probabilities (clamped to 0 ..= 1)
    pub ece: Option<f32>,
}

/// Multi-label AUROC · AP · 15-bin ECE: per label, their macro means over the labels where each is defined, and the
/// pooled (micro) values over every row
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultilabelRankBook {
    pub labels: BTreeMap<String, LabelRank>,
    pub macro_auroc: Option<f32>,
    pub macro_average_precision: Option<f32>,
    pub macro_ece: Option<f32>,
    pub micro_auroc: Option<f32>,
    pub micro_average_precision: Option<f32>,
    pub micro_ece: Option<f32>,
}

fn mean_of(values: impl Iterator<Item = Option<f32>>) -> Option<f32> {
    let v: Vec<f32> = values.flatten().collect();
    (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
}

/// Rank book of `rows = (label, score, hit)`: one row per (item, label) decision. A label with only positives or only
/// negatives has no AUROC / AP (None), and the macro means skip it.
pub fn evaluate_multilabel_rank_book(rows: &[(&str, f32, bool)]) -> MultilabelRankBook {
    let mut by_label: BTreeMap<&str, (Vec<f32>, Vec<bool>)> = BTreeMap::new();
    for &(label, score, hit) in rows {
        let e = by_label.entry(label).or_default();
        e.0.push(score);
        e.1.push(hit);
    }
    let rank = |scores: &[f32], hits: &[bool]| LabelRank {
        n: scores.len(),
        positives: hits.iter().filter(|h| **h).count(),
        auroc: auroc(scores, hits),
        average_precision: average_precision(scores, hits),
        ece: ece(scores, hits, ECE_BINS),
    };
    let labels: BTreeMap<String, LabelRank> = by_label.iter().map(|(l, (s, h))| (l.to_string(), rank(s, h))).collect();
    let scores: Vec<f32> = rows.iter().map(|r| r.1).collect();
    let hits: Vec<bool> = rows.iter().map(|r| r.2).collect();
    MultilabelRankBook {
        macro_auroc: mean_of(labels.values().map(|r| r.auroc)),
        macro_average_precision: mean_of(labels.values().map(|r| r.average_precision)),
        macro_ece: mean_of(labels.values().map(|r| r.ece)),
        micro_auroc: auroc(&scores, &hits),
        micro_average_precision: average_precision(&scores, &hits),
        micro_ece: ece(&scores, &hits, ECE_BINS),
        labels,
    }
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

pub fn platt_fit(scores: &[f32], hits: &[bool]) -> Option<(f32, f32)> {
    let data: Vec<(f64, bool)> = scores
        .iter()
        .zip(hits)
        .filter(|(s, _)| s.is_finite())
        .map(|(s, h)| (*s as f64, *h))
        .collect();
    let pos = data.iter().filter(|d| d.1).count() as f64;
    let neg = data.len() as f64 - pos;
    if pos == 0.0 || neg == 0.0 {
        return None;
    }
    let (t1, t0) = ((pos + 1.0) / (pos + 2.0), 1.0 / (neg + 2.0));
    let (mut a, mut b) = (0.0f64, ((pos + 1.0) / (neg + 1.0)).ln());
    for _ in 0..100 {
        let (mut ga, mut gb, mut haa, mut hab, mut hbb) = (0.0, 0.0, 1e-9, 0.0, 1e-9);
        for &(s, h) in &data {
            let p = sigmoid(a * s + b);
            let t = if h { t1 } else { t0 };
            let d = p - t;
            let w = p * (1.0 - p);
            ga += d * s;
            gb += d;
            haa += w * s * s;
            hab += w * s;
            hbb += w;
        }
        let det = haa * hbb - hab * hab;
        if det.abs() < 1e-18 {
            break;
        }
        let da = (hbb * ga - hab * gb) / det;
        let db = (haa * gb - hab * ga) / det;
        a -= da;
        b -= db;
        if da.abs() < 1e-9 && db.abs() < 1e-9 {
            break;
        }
    }
    (a.is_finite() && b.is_finite()).then_some((a as f32, b as f32))
}

/// Fits a monotone-increasing Platt logistic `(a, b)`, falling back to a flat Laplace-smoothed
/// prior `(0.0, logit(rate))` whenever positives `< min_pos` or fitted slope `a <= 0.0`.
pub fn platt_fit_or_prior(scores: &[f32], hits: &[bool], min_pos: usize) -> Option<(f32, f32)> {
    let finite: Vec<(f32, bool)> = scores
        .iter()
        .copied()
        .zip(hits.iter().copied())
        .filter(|(s, _)| s.is_finite())
        .collect();
    if finite.is_empty() {
        return None;
    }
    let pos = finite.iter().filter(|x| x.1).count();
    let rate = (pos as f64 + 0.5) / (finite.len() as f64 + 1.0);
    let flat = (0.0f32, (rate / (1.0 - rate)).ln() as f32);
    if pos < min_pos.max(1) {
        return Some(flat);
    }
    let (fs, fh): (Vec<f32>, Vec<bool>) = finite.into_iter().unzip();
    match platt_fit(&fs, &fh) {
        Some((a, b)) if a > 0.0 => Some((a, b)),
        _ => Some(flat),
    }
}

pub fn threshold_for_precision(scores: &[f32], hits: &[bool], precision: f32) -> Option<(f32, f32)> {
    let mut v: Vec<(f32, bool)> = scores
        .iter()
        .copied()
        .zip(hits.iter().copied())
        .filter(|(s, _)| s.is_finite())
        .collect();
    let pos = v.iter().filter(|x| x.1).count();
    if pos == 0 {
        return None;
    }
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (mut tp, mut best) = (0usize, None);
    for k in 0..v.len() {
        tp += v[k].1 as usize;
        if k + 1 < v.len() && v[k + 1].0 == v[k].0 {
            continue;
        }
        if tp as f32 / (k + 1) as f32 >= precision {
            best = Some((v[k].0, tp as f32 / pos as f32));
        }
    }
    best
}

pub fn threshold_for_fpr(scores: &[f32], hits: &[bool], fpr: f32) -> Option<f32> {
    let mut neg: Vec<f32> = scores
        .iter()
        .zip(hits)
        .filter(|(s, h)| !**h && s.is_finite())
        .map(|(s, _)| *s)
        .collect();
    if neg.is_empty() {
        return None;
    }
    neg.sort_by(|a, b| b.total_cmp(a));
    let k = ((fpr.clamp(0.0, 1.0) * neg.len() as f32).floor() as usize).min(neg.len() - 1);
    Some(next_up(neg[k]))
}

fn next_up(x: f32) -> f32 {
    if x.is_finite() {
        f32::from_bits(if x >= 0.0 { x.to_bits() + 1 } else { x.to_bits() - 1 })
    } else {
        x
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelCalib {
    pub threshold: f32,
    #[serde(default)]
    pub platt: Option<(f32, f32)>,
    pub positives: u32,
    pub negatives: u32,
    #[serde(default)]
    pub precision: Option<f32>,
    #[serde(default)]
    pub recall: Option<f32>,
}

impl LabelCalib {
    pub fn probability(&self, score: f32) -> Option<f32> {
        self.platt.map(|(a, b)| sigmoid((a * score + b) as f64) as f32)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalibTable {
    pub mode: String,
    #[serde(default)]
    pub statistic: String,
    #[serde(default)]
    pub note: String,
    pub labels: BTreeMap<String, LabelCalib>,
}

impl CalibTable {
    pub fn threshold(&self, label: &str) -> Option<f32> {
        self.labels.get(label).map(|c| c.threshold)
    }

    pub fn load(path: &Path) -> Result<Vec<CalibTable>> {
        let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(raw: &str) -> Result<Vec<CalibTable>> {
        Ok(serde_json::from_str(raw)?)
    }
}

pub fn fit_label(scores: &[f32], hits: &[bool], precision: f32, fpr: f32) -> Option<LabelCalib> {
    let positives = hits.iter().filter(|h| **h).count() as u32;
    let negatives = hits.len() as u32 - positives;
    let threshold = match threshold_for_precision(scores, hits, precision) {
        Some((t, _)) => t,
        None => threshold_for_fpr(scores, hits, fpr)?,
    };
    let (mut tp, mut fp) = (0u32, 0u32);
    for (s, h) in scores.iter().zip(hits) {
        if *s >= threshold {
            if *h {
                tp += 1;
            } else {
                fp += 1;
            }
        }
    }
    Some(LabelCalib {
        threshold,
        platt: platt_fit(scores, hits),
        positives,
        negatives,
        precision: (tp + fp > 0).then(|| tp as f32 / (tp + fp) as f32),
        recall: (positives > 0).then(|| tp as f32 / positives as f32),
    })
}

#[cfg(test)]
mod multilabel_tests {
    use super::*;

    #[test]
    fn the_rank_book_scores_each_label_and_skips_undefined_ones_in_the_macro_mean() {
        let rows = [
            ("a", 0.9, true),
            ("a", 0.8, true),
            ("a", 0.2, false),
            ("b", 0.7, false),
            ("b", 0.6, true),
            ("b", 0.1, false),
            ("c", 0.5, true), // only positives: no AUROC / AP
        ];
        let book = evaluate_multilabel_rank_book(&rows);
        assert_eq!(book.labels.len(), 3);
        assert_eq!(book.labels["a"].auroc, Some(1.0));
        assert_eq!(book.labels["b"].auroc, Some(0.5));
        assert_eq!(book.labels["c"].auroc, None);
        assert_eq!((book.labels["c"].n, book.labels["c"].positives), (1, 1));
        assert_eq!(book.macro_auroc, Some(0.75), "a and b only");
        assert!(book.micro_auroc.is_some() && book.macro_ece.is_some());
        let empty = evaluate_multilabel_rank_book(&[]);
        assert!(empty.labels.is_empty() && empty.macro_auroc.is_none() && empty.micro_ece.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_metrics_on_a_known_case() {
        let s = [0.9, 0.8, 0.7, 0.6, 0.5, 0.4];
        let h = [true, false, true, false, false, false];
        assert!((auroc(&s, &h).unwrap() - 7.0 / 8.0).abs() < 1e-6);
        assert!((average_precision(&s, &h).unwrap() - (1.0 + 2.0 / 3.0) / 2.0).abs() < 1e-6);
        assert!(auroc(&s, &[true; 6]).is_none());
        assert!((auroc(&[0.5, 0.5], &[true, false]).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn platt_fit_or_prior_sparse_and_negative_slope_fallback() {
        // Sparse positive count (< 5) falls back safely to flat prior (a == 0.0)
        let (a, b) = platt_fit_or_prior(&[0.9, 0.8, 0.2, 0.1, 0.05, 0.02], &[true, true, false, false, false, false], 5).unwrap();
        assert_eq!(a, 0.0);
        assert!(b.is_finite() && b < 0.0);

        // Inverted relationship (higher scores are negatives) -> negative slope rejected -> flat prior
        let s = [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0];
        let h = [false, false, false, false, false, true, true, true, true, true];
        let (a2, b2) = platt_fit_or_prior(&s, &h, 5).unwrap();
        assert_eq!(a2, 0.0);
        assert!(b2.abs() < 1e-5);

        // Clean monotone data fits positive slope
        let (a3, _) = platt_fit_or_prior(&s, &[true, true, true, true, true, false, false, false, false, false], 5).unwrap();
        assert!(a3 > 0.0);
    }

    #[test]
    fn thresholds_and_ece_behave() {
        let s = [0.95, 0.9, 0.85, 0.8, 0.7, 0.6, 0.5, 0.4];
        let h = [true, true, false, true, true, false, false, false];
        let (t, recall) = threshold_for_precision(&s, &h, 0.75).unwrap();
        assert_eq!(t, 0.7);
        assert!((recall - 1.0).abs() < 1e-6);
        let fit = fit_label(&s, &h, 0.75, 0.1).unwrap();
        assert_eq!((fit.positives, fit.negatives), (4, 4));
        assert!(ece(&[0.9, 0.1], &[true, false], ECE_BINS).unwrap() < 0.15);
    }
}
