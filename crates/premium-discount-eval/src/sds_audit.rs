//! Four SDS ledger dialects on one line-level trace schema ([`SdsTraceLineV1`]: one JSON line per decision, frame or
//! document) and the eight stream invariants ([`verify_sds_trace_stream`]).
//!
//! Each dialect keeps its own ledger; an adapter in the owning repository writes these lines next to the ledger, so
//! one auditor checks them all with the same rules:
//!
//! 1. no premature decision below `MinObs` (abstain unless the row is a pooled target);
//! 2. the live peek and the committed decision read the same z (`|peek_live_z − committed_z| < 1e-12`);
//! 3. no flat tie is assigned;
//! 4. an optical / format rejection never becomes a prejudice deduction;
//! 5. a drift (`|drift_z| ≥ 2`) shrinks the effective half-life (never below the floor, never above the base);
//! 6. a perforation that opened more than the budget (0.65) fell back to one full pass;
//! 7. no ground-truth / reference provenance on a control path, no uncatalogued axis write;
//! 8. no statistics shared across recipes or across perforated (`+perf`) and canonical scopes.
//!
//! Model-free, `wasm32-unknown-unknown` clean, no scenario literals.

use serde::{Deserialize, Serialize};

/// A ledger dialect
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdsDialect {
    /// `premium-discount-sds::Ledger` (track | primary | secondary + MinObs back-off)
    ZenaSdsV1,
    /// per-space ledgers (`spaces/<space_tag>/`) with field rejection statistics and a precision fingerprint
    LogisSdsV3,
    /// audit chain with a flush guard (WORM records)
    LvSdsAudit,
    /// faceted scope keys with partial pooling and control feedback paths
    SandboxFacetedV2,
}

impl SdsDialect {
    pub const ALL: [SdsDialect; 4] = [SdsDialect::ZenaSdsV1, SdsDialect::LogisSdsV3, SdsDialect::LvSdsAudit, SdsDialect::SandboxFacetedV2];

    /// The recipe label its trace lines carry
    pub fn recipe(self) -> &'static str {
        match self {
            SdsDialect::ZenaSdsV1 => "zena-sds-v1",
            SdsDialect::LogisSdsV3 => "sds-v3",
            SdsDialect::LvSdsAudit => "lv-worm-v1",
            SdsDialect::SandboxFacetedV2 => "sandbox-sds-v2",
        }
    }

    pub fn from_recipe(recipe: &str) -> Option<SdsDialect> {
        SdsDialect::ALL.into_iter().find(|d| d.recipe() == recipe)
    }
}

/// Suffix of a perforated (local, approximate) scope; canonical scopes never carry it
pub const PERFORATED_SUFFIX: &str = "+perf";
/// The default base half-life when a line does not say (14 days)
pub const DEFAULT_BASE_HALF_LIFE_MS: f64 = 14.0 * 86_400_000.0;
/// The shortest effective half-life a drift may reach (2 days)
pub const MIN_DRIFT_HALF_LIFE_MS: f64 = 2.0 * 86_400_000.0;
/// `|drift_z|` at which the half-life must shrink
pub const DRIFT_Z_TRIGGER: f64 = 2.0;
/// Open-cell ratio above which a perforated pass must fall back to one full pass
pub const PERFORATION_BUDGET: f32 = 0.65;
/// Largest `|peek_live_z − committed_z|` counted as the same reading
pub const PEEK_EQUALITY_EPS: f64 = 1e-12;

/// One decision, frame or document of any dialect
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdsTraceLineV1 {
    pub seq: u64,
    pub dialect: SdsDialect,
    /// [`SdsDialect::recipe`] of the ledger that produced the statistic
    pub recipe: String,
    /// The full scope key, mode / quantization tags included (`…+q8_0+perf`)
    pub scope_key: String,
    /// `"secondary" | "primary" | "global" | "pooled"`, or None when the decision abstained (MinObs not met)
    pub resolved_level: Option<String>,
    pub min_obs_required: u64,
    pub observed_n: u64,
    pub effective_n: f64,
    pub peek_live_z: Option<f64>,
    pub committed_z: Option<f64>,
    pub drift_z: f64,
    pub effective_half_life_ms: Option<f64>,
    /// `[margin, top_gap_ratio, tail_flatness, entropy_norm, positive_ratio]`
    pub decay_shape_5d: Option<[f32; 5]>,
    /// `"decisive" | "pair_confusion" | "flat"`
    pub tie_kind: Option<String>,
    pub assigned_accepted: bool,
    /// `"format" | "prejudice" | "enum" | "self_id"`
    pub reject_gate: Option<String>,
    /// Perforation open-cell ratio (0 ..= 1) when the decision came from a perforated pass
    pub open_cell_ratio: Option<f32>,
    /// The pass skipped the second expansion and ran one full pass instead
    pub budget_fallback_full: bool,
    /// Provenance read on a control path (`"spec" | "experience"`; `"truth" | "reference"` are violations)
    pub provenance_in_control: Option<String>,
    /// The base half-life of the scope (default [`DEFAULT_BASE_HALF_LIFE_MS`])
    #[serde(default)]
    pub base_half_life_ms: Option<f64>,
    /// A rejection on this line was turned into a prejudice deduction
    #[serde(default)]
    pub prejudice_deducted: bool,
    /// Axis writes the dialect's catalogue did not know
    #[serde(default)]
    pub uncatalogued_axis_writes: u64,
    /// The mode of the input that asked (`"+perf"` for a perforated input, `""` for canonical); None when unknown
    #[serde(default)]
    pub input_mode: Option<String>,
}

/// The eight invariants, in order
pub const INVARIANTS: [&str; 8] = [
    "min_obs_abstain",
    "peek_live_equals_committed",
    "flat_tie_not_assigned",
    "format_not_prejudice",
    "drift_shrinks_half_life",
    "budget_fallback_full",
    "control_provenance_clean",
    "no_cross_recipe_or_mode_mix",
];

/// Violations of the eight invariants over a stream
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SdsInvariantReport {
    pub lines: u64,
    /// Violations per invariant ([`INVARIANTS`] order)
    pub violations: [u64; 8],
    /// `seq` of the first violating line per invariant
    pub first_violation_seq: [Option<u64>; 8],
}

impl SdsInvariantReport {
    pub fn clean(&self) -> bool {
        self.violations.iter().all(|&v| v == 0)
    }

    fn flag(&mut self, invariant: usize, seq: u64) {
        self.violations[invariant] += 1;
        self.first_violation_seq[invariant].get_or_insert(seq);
    }
}

/// Checks every line of a stream against the eight invariants.
pub fn verify_sds_trace_stream(lines: &[SdsTraceLineV1]) -> SdsInvariantReport {
    let mut r = SdsInvariantReport {
        lines: lines.len() as u64,
        ..SdsInvariantReport::default()
    };
    for l in lines {
        // 1. below MinObs only a pooled target may resolve
        if l.observed_n < l.min_obs_required && l.resolved_level.as_deref().is_some_and(|lv| lv != "pooled") {
            r.flag(0, l.seq);
        }
        // 2. peek and commit read the same z
        if let (Some(p), Some(c)) = (l.peek_live_z, l.committed_z) {
            if !(p.is_finite() && c.is_finite()) || (p - c).abs() >= PEEK_EQUALITY_EPS {
                r.flag(1, l.seq);
            }
        }
        // 3. a flat tie is never assigned
        if l.tie_kind.as_deref() == Some("flat") && l.assigned_accepted {
            r.flag(2, l.seq);
        }
        // 4. a format rejection stays out of prejudice
        if l.reject_gate.as_deref().is_some_and(|g| g.eq_ignore_ascii_case("format")) && l.prejudice_deducted {
            r.flag(3, l.seq);
        }
        // 5. a drift shrinks the half-life, within [floor, base]
        if l.drift_z.is_finite() && l.drift_z.abs() >= DRIFT_Z_TRIGGER {
            if let Some(h) = l.effective_half_life_ms {
                let base = l.base_half_life_ms.unwrap_or(DEFAULT_BASE_HALF_LIFE_MS);
                let floor = MIN_DRIFT_HALF_LIFE_MS.min(base);
                let shrunk = if l.drift_z.abs() > DRIFT_Z_TRIGGER && base > floor { h < base } else { h <= base };
                if !h.is_finite() || h < floor || !shrunk {
                    r.flag(4, l.seq);
                }
            }
        }
        // 6. over budget means one full pass
        if l.open_cell_ratio.is_some_and(|o| o > PERFORATION_BUDGET) && !l.budget_fallback_full {
            r.flag(5, l.seq);
        }
        // 7. control paths see no ground truth / reference, and every axis write is catalogued
        let tainted = l.provenance_in_control.as_deref().is_some_and(|p| p.eq_ignore_ascii_case("truth") || p.eq_ignore_ascii_case("reference"));
        if (l.dialect == SdsDialect::SandboxFacetedV2 && tainted) || l.uncatalogued_axis_writes > 0 {
            r.flag(6, l.seq);
        }
        // 8. the statistic comes from the dialect's own recipe and from a scope of the input's mode
        let recipe_mixed = l.recipe != l.dialect.recipe();
        let perforated_scope = l.scope_key.contains(PERFORATED_SUFFIX);
        let mode_mixed = match l.input_mode.as_deref() {
            Some(m) if m == PERFORATED_SUFFIX => !perforated_scope,
            Some(_) => perforated_scope,
            None => false,
        };
        if recipe_mixed || mode_mixed {
            r.flag(7, l.seq);
        }
    }
    r
}

/// One stream's summary across dialects
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnifiedSdsAuditSummary {
    /// The stream's dialect when every line has the same one
    pub dialect: Option<SdsDialect>,
    pub total_scopes: usize,
    pub flat_tie_abstentions: u64,
    pub confusion_tie_breaks: u64,
    pub active_drift_shifts: u64,
    /// SandboxFacetedV2 only: ground truth / reference read on the control path (pass line == 0)
    pub provenance_control_breaches: u64,
    /// Uncatalogued axis writes (pass line == 0)
    pub uncatalogued_axis_writes: u64,
}

/// Summary counts of a stream (the invariants themselves are [`verify_sds_trace_stream`])
pub fn summarize_sds_trace(lines: &[SdsTraceLineV1]) -> UnifiedSdsAuditSummary {
    let mut scopes: Vec<&str> = lines.iter().map(|l| l.scope_key.as_str()).collect();
    scopes.sort_unstable();
    scopes.dedup();
    let first = lines.first().map(|l| l.dialect);
    UnifiedSdsAuditSummary {
        dialect: first.filter(|d| lines.iter().all(|l| l.dialect == *d)),
        total_scopes: scopes.len(),
        flat_tie_abstentions: lines.iter().filter(|l| l.tie_kind.as_deref() == Some("flat") && !l.assigned_accepted).count() as u64,
        confusion_tie_breaks: lines.iter().filter(|l| l.tie_kind.as_deref() == Some("pair_confusion") && l.assigned_accepted).count() as u64,
        active_drift_shifts: lines.iter().filter(|l| l.drift_z.is_finite() && l.drift_z.abs() >= DRIFT_Z_TRIGGER).count() as u64,
        provenance_control_breaches: lines
            .iter()
            .filter(|l| l.dialect == SdsDialect::SandboxFacetedV2)
            .filter(|l| l.provenance_in_control.as_deref().is_some_and(|p| p.eq_ignore_ascii_case("truth") || p.eq_ignore_ascii_case("reference")))
            .count() as u64,
        uncatalogued_axis_writes: lines.iter().map(|l| l.uncatalogued_axis_writes).sum(),
    }
}

/// Reads a JSONL trace (blank lines skipped). The error names the 1-based line that failed.
pub fn parse_sds_trace_jsonl(text: &str) -> Result<Vec<SdsTraceLineV1>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, dialect: SdsDialect) -> SdsTraceLineV1 {
        SdsTraceLineV1 {
            seq,
            dialect,
            recipe: dialect.recipe().to_string(),
            scope_key: "track|primary|secondary".into(),
            resolved_level: Some("secondary".into()),
            min_obs_required: 5,
            observed_n: 9,
            effective_n: 9.0,
            peek_live_z: Some(1.25),
            committed_z: Some(1.25),
            drift_z: 0.0,
            effective_half_life_ms: None,
            decay_shape_5d: None,
            tie_kind: Some("decisive".into()),
            assigned_accepted: true,
            reject_gate: None,
            open_cell_ratio: None,
            budget_fallback_full: false,
            provenance_in_control: Some("spec".into()),
            base_half_life_ms: None,
            prejudice_deducted: false,
            uncatalogued_axis_writes: 0,
            input_mode: None,
        }
    }

    #[test]
    fn a_clean_stream_of_all_four_dialects_passes() {
        let lines: Vec<_> = SdsDialect::ALL.iter().enumerate().map(|(i, &d)| line(i as u64, d)).collect();
        let r = verify_sds_trace_stream(&lines);
        assert!(r.clean(), "{r:?}");
        assert_eq!(r.lines, 4);
        for d in SdsDialect::ALL {
            assert_eq!(SdsDialect::from_recipe(d.recipe()), Some(d));
        }
        let s = summarize_sds_trace(&lines);
        assert_eq!((s.dialect, s.total_scopes), (None, 1));
    }

    #[test]
    fn each_invariant_catches_its_counterexample() {
        let base = || line(7, SdsDialect::SandboxFacetedV2);
        let bad: [SdsTraceLineV1; 8] = [
            SdsTraceLineV1 { observed_n: 2, ..base() },
            SdsTraceLineV1 { committed_z: Some(1.250001), ..base() },
            SdsTraceLineV1 { tie_kind: Some("flat".into()), ..base() },
            SdsTraceLineV1 { reject_gate: Some("Format".into()), prejudice_deducted: true, ..base() },
            SdsTraceLineV1 { drift_z: 3.0, effective_half_life_ms: Some(DEFAULT_BASE_HALF_LIFE_MS), ..base() },
            SdsTraceLineV1 { open_cell_ratio: Some(0.72), budget_fallback_full: false, ..base() },
            SdsTraceLineV1 { provenance_in_control: Some("truth".into()), ..base() },
            SdsTraceLineV1 { scope_key: "a|b|c".into(), input_mode: Some(PERFORATED_SUFFIX.into()), ..base() },
        ];
        for (i, l) in bad.iter().enumerate() {
            let r = verify_sds_trace_stream(std::slice::from_ref(l));
            let mut want = [0u64; 8];
            want[i] = 1;
            assert_eq!(r.violations, want, "invariant {} ({})", i + 1, INVARIANTS[i]);
            assert_eq!(r.first_violation_seq[i], Some(7));
        }
        // the allowed forms of the same lines
        let ok = [
            SdsTraceLineV1 { observed_n: 2, resolved_level: None, ..base() },
            SdsTraceLineV1 { observed_n: 2, resolved_level: Some("pooled".into()), ..base() },
            SdsTraceLineV1 { tie_kind: Some("flat".into()), assigned_accepted: false, ..base() },
            SdsTraceLineV1 { drift_z: 3.0, effective_half_life_ms: Some(7.0 * 86_400_000.0), ..base() },
            SdsTraceLineV1 { drift_z: 9.0, effective_half_life_ms: Some(MIN_DRIFT_HALF_LIFE_MS), ..base() },
            SdsTraceLineV1 { open_cell_ratio: Some(0.72), budget_fallback_full: true, ..base() },
            SdsTraceLineV1 { scope_key: "a|b|c+perf".into(), input_mode: Some(PERFORATED_SUFFIX.into()), ..base() },
            SdsTraceLineV1 { provenance_in_control: Some("truth".into()), ..line(7, SdsDialect::ZenaSdsV1) },
        ];
        let r = verify_sds_trace_stream(&ok);
        assert!(r.clean(), "{r:?}");
        // a recipe that is not the dialect's own
        let mixed = SdsTraceLineV1 { recipe: "sds-v3".into(), ..line(1, SdsDialect::ZenaSdsV1) };
        assert_eq!(verify_sds_trace_stream(&[mixed]).violations[7], 1);
    }

    #[test]
    fn traces_round_trip_through_jsonl_and_old_lines_read_with_defaults() {
        let l = line(3, SdsDialect::LogisSdsV3);
        let text = format!("{}\n\n{}\n", serde_json::to_string(&l).unwrap(), serde_json::to_string(&line(4, SdsDialect::LogisSdsV3)).unwrap());
        let back = parse_sds_trace_jsonl(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], l);
        // a line written before the optional fields existed
        let mut v = serde_json::to_value(&l).unwrap();
        for k in ["base_half_life_ms", "prejudice_deducted", "uncatalogued_axis_writes", "input_mode"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let old: SdsTraceLineV1 = serde_json::from_value(v).unwrap();
        assert_eq!(old, l);
        assert!(parse_sds_trace_jsonl("{\"seq\":1}").unwrap_err().starts_with("line 1"));
        assert_eq!(serde_json::to_value(SdsDialect::SandboxFacetedV2).unwrap(), "sandbox_faceted_v2");
    }
}
