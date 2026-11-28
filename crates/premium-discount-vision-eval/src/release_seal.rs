//! Zero-tolerance hard gates, E3 non-inferiority, WORM ledger integrity, and Release-Seal evaluator.

use std::path::{Path, PathBuf};

use premium_discount_eval::ranking::{check_e3_non_inferiority, GateExit, Polarity};
use premium_discount_sds::audit::verify_file;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroToleranceViolations {
    pub false_auto_finalize_adverse_or_proc: u32,
    pub unproven_upward_edit_or_rubber_stamp: u32,
    pub interlock_out_of_order_or_stage_fire: u32,
    pub outbound_term_fact_or_c2o_leak: u32,
    pub privacy_pixel_kiosk_or_gov_id_leak: u32,
    pub sql_ast_or_cross_vault_breach: u32,
    pub pinned_stage_weight_swap_or_overrun: u32,
    pub haring_visual_or_blockies_breach: u32,
}

impl ZeroToleranceViolations {
    pub fn total_breaches(&self) -> u32 {
        self.false_auto_finalize_adverse_or_proc
            + self.unproven_upward_edit_or_rubber_stamp
            + self.interlock_out_of_order_or_stage_fire
            + self.outbound_term_fact_or_c2o_leak
            + self.privacy_pixel_kiosk_or_gov_id_leak
            + self.sql_ast_or_cross_vault_breach
            + self.pinned_stage_weight_swap_or_overrun
            + self.haring_visual_or_blockies_breach
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSealInput {
    pub hygiene_clean: bool,
    pub op_dir_digest_before: String,
    pub op_dir_digest_after: String,
    pub worm_log_path: PathBuf,
    pub expected_anchor_seq: u64,
    pub expected_anchor_hash: String,
    pub dsli_7: f64,
    pub dsli_18: f64,
    pub zh_passed: usize,
    pub xh_passed: usize,
    pub cxh_passed: usize,
    pub odd_adversarial_passed: usize,
    pub slice_r10_pure_vs_e2e: Vec<(String, Vec<f64>, Vec<f64>, f64)>,
    pub mean_audio_tiou: f64,
    pub violations: ZeroToleranceViolations,
}

pub fn evaluate_final_release_seal(inp: &ReleaseSealInput, boot_seed: u64) -> GateExit {
    if !inp.hygiene_clean || inp.op_dir_digest_before != inp.op_dir_digest_after {
        return GateExit::AuditError;
    }
    match verify_file(Path::new(&inp.worm_log_path)) {
        Ok(ok)
            if ok.lines as u64 == inp.expected_anchor_seq
                && ok.last_hash == inp.expected_anchor_hash => {}
        _ => return GateExit::AuditError,
    }
    if inp.violations.total_breaches() > 0 {
        return GateExit::Regressed;
    }
    let total_holdout = inp.zh_passed + inp.xh_passed + inp.cxh_passed;
    if inp.dsli_7 > 0.65
        || inp.dsli_18 > 0.72
        || inp.mean_audio_tiou < 0.70
        || inp.odd_adversarial_passed < 30
        || inp.zh_passed < 35
        || inp.xh_passed < 12
        || inp.cxh_passed < 12
        || total_holdout < 59
    {
        return GateExit::Regressed;
    }
    for (idx, (_lang, pure_s, e2e_s, delta_max)) in inp.slice_r10_pure_vs_e2e.iter().enumerate() {
        let exit = check_e3_non_inferiority(
            pure_s,
            e2e_s,
            Polarity::HigherIsBetter,
            *delta_max,
            2_000,
            boot_seed.wrapping_add(idx as u64),
        );
        if exit != GateExit::Pass {
            return exit;
        }
    }
    GateExit::Pass
}
