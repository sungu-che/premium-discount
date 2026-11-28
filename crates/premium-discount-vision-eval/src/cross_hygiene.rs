//! Cross-ecosystem 4-Tier engine tree hygiene audit using `premium-discount-eval::hygiene`.

use std::path::{Path, PathBuf};

use premium_discount_eval::hygiene::{scan_engine_tree, HygieneFinding};

pub const TIER0_PURE_MATH_CRATES: &[&str] = &[
    "premium-discount/crates/premium-discount-eval/src",
    "premium-discount/crates/premium-discount-nms/src",
    "premium-discount/crates/premium-discount-sds/src",
];

pub const TIER1_ENGINE_CORE_CRATES: &[&str] = &[
    "louis-vision/crates/lv-kernels/src",
    "louis-vision/crates/lv-sds/src",
    "louis-vision/crates/lv-core/src",
    "louis-vision/crates/lv-screen/src",
    "louis-vision/crates/lv-doc/src",
    "louis-vision/crates/lv-spatial/src",
    "louis-vision/crates/lv-planar/src",
    "louis-vision/crates/lv-domain/src",
    "louis-vision/crates/lv-router/src",
    "louis-vision/crates/lv-agent/src",
    "louis-vision/apps/vision/crates/sandbox-core/src/act",
];

pub const FORBIDDEN_BENCH_IDENTS: &[&str] = &[
    "nn_scenarios",
    "ScreenSpot",
    "OmniDocBench",
    "OSWorld",
    "Mind2Web",
    "NYUv2",
    "ScanNet",
    "docs_nn_scenarios",
    "DOCS_NN_HOLDOUT_SEED",
    "MASTER_HOLDOUT_SEED",
    "ZH-",
    "ui_nn_scenarios",
    "UI_NN_HOLDOUT_SEED",
    "UI-ZH-",
    "XH-",
    "CXH-",
    "build_holdout_zh_36_specs",
    "corpus_plan_manifest",
    "docs_synth_mcp",
    "ReleaseSeal",
    "reference_report",
    "exact_reference",
    "composite_reference",
    "holdout_zh",
    "holdout_xh",
    "holdout_cxh",
];

pub const FORBIDDEN_TIER0_DOMAIN_LITERALS: &[&str] = &[
    "ConstructionSettle",
    "RndAudit",
    "InsolvencyLegal",
    "Finance3Way",
    "Iso6346Container",
    "HsCodeKr",
    "BizRegNoKr10",
    "cbam_embedded_co2",
    "bill_of_lading",
    "commercial_invoice",
    "HaringDancerRig",
];

/// Audits Tier 0 pure-math crates and Tier 1 engine cores under `workspace_parent` for leaks.
pub fn audit_workspace_hygiene(workspace_parent: &Path) -> anyhow::Result<Vec<HygieneFinding>> {
    let mut findings = Vec::new();
    for rel in TIER0_PURE_MATH_CRATES {
        let dir = workspace_parent.join(rel);
        if dir.exists() {
            findings.extend(scan_engine_tree(&dir, FORBIDDEN_TIER0_DOMAIN_LITERALS, &["rs"])?);
            findings.extend(scan_engine_tree(&dir, FORBIDDEN_BENCH_IDENTS, &["rs"])?);
        }
    }
    for rel in TIER1_ENGINE_CORE_CRATES {
        let dir = workspace_parent.join(rel);
        if dir.exists() {
            findings.extend(scan_engine_tree(&dir, FORBIDDEN_BENCH_IDENTS, &["rs"])?);
        }
    }
    Ok(findings)
}

/// Audits all 10 Tier 1 crates under `lv_root` for benchmark and UI/docs holdout contamination literals.
pub fn audit_louis_vision_crates(lv_root: &Path) -> anyhow::Result<Vec<HygieneFinding>> {
    let mut findings = Vec::new();
    for krate in [
        "lv-kernels",
        "lv-sds",
        "lv-core",
        "lv-screen",
        "lv-doc",
        "lv-spatial",
        "lv-planar",
        "lv-domain",
        "lv-router",
        "lv-agent",
    ] {
        let src = lv_root.join("crates").join(krate).join("src");
        if src.exists() {
            findings.extend(scan_engine_tree(&src, FORBIDDEN_BENCH_IDENTS, &["rs"])?);
        }
    }
    Ok(findings)
}

pub fn workspace_parent_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

pub fn sibling_louis_vision_root() -> PathBuf {
    workspace_parent_root().join("louis-vision")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louis_vision_engine_trees_contain_zero_benchmark_contamination() {
        let ws = workspace_parent_root();
        let lv = sibling_louis_vision_root();
        if !lv.join("Cargo.toml").exists() {
            return;
        }
        let findings = audit_workspace_hygiene(&ws).unwrap();
        assert!(findings.is_empty(), "hygiene findings in workspace: {findings:?}");
        let lv_findings = audit_louis_vision_crates(&lv).unwrap();
        assert!(lv_findings.is_empty(), "hygiene findings in louis-vision: {lv_findings:?}");
    }
}
