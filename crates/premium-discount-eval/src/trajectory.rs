//! Multi-stage course trajectories (which checkpoints a run reached, in order, and where it first failed) and one gate
//! report format ([`GateReportV1`]) for every suite's final verdict.
//!
//! Model-free, `wasm32-unknown-unknown` clean, no scenario literals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ranking::GateExit;

/// One run against its expected checkpoints
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryScore {
    pub expected: usize,
    /// Expected checkpoints reached in order (a greedy in-order match of `observed` against `expected`)
    pub reached: usize,
    /// Every expected checkpoint was reached in order
    pub completed: bool,
    /// Index of the first expected checkpoint not reached in order
    pub first_miss: Option<usize>,
    /// Observed checkpoints that are not the next expected one (detours, repeats)
    pub detours: usize,
    /// Bits of expected steps (first 16) that a fault was attributed to
    pub fault_mask: u16,
}

impl TrajectoryScore {
    pub fn completion_ratio(&self) -> f64 {
        if self.expected == 0 {
            1.0
        } else {
            self.reached as f64 / self.expected as f64
        }
    }
}

/// Scores one run: `expected` checkpoints in order, `observed` the checkpoints the run passed, `faulted[i]` whether
/// expected step `i` had a fault (friction, a folded cancel loop, …; shorter slices mean no fault).
pub fn score_trajectory(expected: &[&str], observed: &[&str], faulted: &[bool]) -> TrajectoryScore {
    let mut next = 0usize;
    let mut detours = 0usize;
    for o in observed {
        if next < expected.len() && *o == expected[next] {
            next += 1;
        } else {
            detours += 1;
        }
    }
    let fault_mask = faulted
        .iter()
        .take(16.min(expected.len()))
        .enumerate()
        .filter(|(_, f)| **f)
        .fold(0u16, |m, (i, _)| m | (1 << i));
    TrajectoryScore {
        expected: expected.len(),
        reached: next,
        completed: next == expected.len(),
        first_miss: (next < expected.len()).then_some(next),
        detours,
        fault_mask,
    }
}

/// Many runs together
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrajectorySummary {
    pub runs: usize,
    pub completion_rate: f64,
    pub mean_completion_ratio: f64,
    pub mean_detours: f64,
    /// How often each expected step (0..16) was the first miss
    pub first_miss_histogram: BTreeMap<usize, usize>,
    /// How often each expected step (0..16) carried a fault
    pub fault_histogram: BTreeMap<usize, usize>,
}

pub fn summarize_trajectories(scores: &[TrajectoryScore]) -> TrajectorySummary {
    let n = scores.len();
    if n == 0 {
        return TrajectorySummary::default();
    }
    let mut s = TrajectorySummary {
        runs: n,
        completion_rate: scores.iter().filter(|t| t.completed).count() as f64 / n as f64,
        mean_completion_ratio: scores.iter().map(TrajectoryScore::completion_ratio).sum::<f64>() / n as f64,
        mean_detours: scores.iter().map(|t| t.detours as f64).sum::<f64>() / n as f64,
        ..TrajectorySummary::default()
    };
    for t in scores {
        if let Some(i) = t.first_miss {
            *s.first_miss_histogram.entry(i).or_insert(0) += 1;
        }
        for bit in 0..16 {
            if t.fault_mask & (1 << bit) != 0 {
                *s.fault_histogram.entry(bit).or_insert(0) += 1;
            }
        }
    }
    s
}

/// The schema tag of [`GateReportV1`]
pub const GATE_REPORT_SCHEMA: &str = "gate-report/1";

/// One check of a gate
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateCheck {
    pub name: String,
    pub passed: bool,
    /// A failed audit check (static hygiene, ledger integrity, tampering) is an audit error, not a regression
    #[serde(default)]
    pub audit: bool,
    #[serde(default)]
    pub value: Option<f64>,
    /// The pass line as written (`">= 0.95"`, `"== 0"`)
    #[serde(default)]
    pub limit: Option<String>,
}

/// One suite's gate verdict in one format: what ran (suite, split, pins), what it measured, each check and the exit
/// code (0 pass · 1 regressed · 2 audit error)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateReportV1 {
    pub schema: String,
    pub suite: String,
    pub split: String,
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub checks: Vec<GateCheck>,
    pub exit_code: i32,
}

impl GateReportV1 {
    pub fn new(suite: &str, split: &str) -> GateReportV1 {
        GateReportV1 {
            schema: GATE_REPORT_SCHEMA.to_string(),
            suite: suite.to_string(),
            split: split.to_string(),
            pins: BTreeMap::new(),
            metrics: BTreeMap::new(),
            checks: Vec::new(),
            exit_code: GateExit::Pass.code(),
        }
    }

    pub fn pin(&mut self, repo: &str, rev: &str) -> &mut Self {
        self.pins.insert(repo.to_string(), rev.to_string());
        self
    }

    pub fn metric(&mut self, name: &str, value: f64) -> &mut Self {
        self.metrics.insert(name.to_string(), value);
        self
    }

    /// Records a check and folds it into the exit code (an audit failure wins over a regression)
    pub fn check(&mut self, name: &str, passed: bool, value: Option<f64>, limit: Option<&str>) -> &mut Self {
        self.push(name, passed, false, value, limit)
    }

    /// Records an audit check (its failure is an audit error)
    pub fn audit(&mut self, name: &str, passed: bool, value: Option<f64>, limit: Option<&str>) -> &mut Self {
        self.push(name, passed, true, value, limit)
    }

    fn push(&mut self, name: &str, passed: bool, audit: bool, value: Option<f64>, limit: Option<&str>) -> &mut Self {
        self.checks.push(GateCheck { name: name.to_string(), passed, audit, value, limit: limit.map(str::to_string) });
        self.exit_code = self.exit().code();
        self
    }

    pub fn exit(&self) -> GateExit {
        if self.checks.iter().any(|c| !c.passed && c.audit) {
            GateExit::AuditError
        } else if self.checks.iter().any(|c| !c.passed) {
            GateExit::Regressed
        } else {
            GateExit::Pass
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trajectories_count_in_order_progress_detours_and_faults() {
        let expected = ["s", "a", "b", "goal"];
        let full = score_trajectory(&expected, &["s", "x", "a", "b", "b", "goal"], &[false, true]);
        assert!(full.completed && full.first_miss.is_none());
        assert_eq!((full.reached, full.detours, full.fault_mask), (4, 2, 0b10));
        let stuck = score_trajectory(&expected, &["s", "b", "a"], &[]);
        assert_eq!((stuck.reached, stuck.first_miss, stuck.completed), (2, Some(2), false), "b before a does not count");
        assert!((stuck.completion_ratio() - 0.5).abs() < 1e-12);
        let s = summarize_trajectories(&[full, stuck]);
        assert_eq!((s.runs, s.completion_rate), (2, 0.5));
        assert_eq!(s.first_miss_histogram.get(&2), Some(&1));
        assert_eq!(s.fault_histogram.get(&1), Some(&1));
        assert_eq!(summarize_trajectories(&[]).runs, 0);
    }

    #[test]
    fn the_gate_report_folds_checks_into_one_exit_code() {
        let mut r = GateReportV1::new("blind_nav", "diag");
        r.pin("premium-discount", "abc123").metric("quota_d_survival", 1.0);
        r.check("quota_d_survival", true, Some(1.0), Some("== 1.0"));
        assert_eq!(r.exit(), GateExit::Pass);
        r.check("completion", false, Some(0.81), Some(">= 0.9"));
        assert_eq!((r.exit(), r.exit_code), (GateExit::Regressed, 1));
        r.audit("hygiene", false, Some(1.0), Some("== 0"));
        assert_eq!((r.exit(), r.exit_code), (GateExit::AuditError, 2));
        let json = serde_json::to_string(&r).unwrap();
        let back: GateReportV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.schema, GATE_REPORT_SCHEMA);
    }
}
