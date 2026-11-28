//! Screen vision & privacy gate evaluator over `lv-screen` (`eval_bridge`, `sensor`, `naflex`, `screen`).

use std::sync::atomic::{AtomicU64, Ordering};

use lv_core::siglip2::EpochWatch;
use lv_screen::eval_bridge::{calibrate_vision_scores, evaluate_perforation_ab};
use lv_screen::naflex::{patchify_frame_with, redact_rgb_for_naflex, Preprocess};
use lv_screen::sensor::{LogLumSensor, PurgableSensor, REPRIME_THRESHOLD_MILLI};
use premium_discount_eval::gate_ab::{ArmRunSummary, DetectionCounters, GateAbVerdict};

#[derive(Debug, Clone)]
pub struct ScreenGateReport {
    pub ab_verdict: GateAbVerdict,
    pub platt_params: (f32, f32),
    pub toast_masked_cells: usize,
    pub reprime_sentinel_ok: bool,
    pub privacy_epoch_aborts: bool,
}

pub fn run_screen_eval_suite() -> anyhow::Result<ScreenGateReport> {
    let full_off = ArmRunSummary {
        runs: 20,
        done: 20,
        wasted: 200,
        ticks: 1000,
        counters: DetectionCounters { tp: 36, fp: 4, fn_: 4, tn: 156 },
    };
    let perf_on = ArmRunSummary {
        runs: 20,
        done: 20,
        wasted: 118,
        ticks: 590,
        counters: DetectionCounters { tp: 36, fp: 2, fn_: 4, tn: 158 },
    };
    let ab_verdict = evaluate_perforation_ab(&perf_on, &full_off);
    anyhow::ensure!(ab_verdict.passed, "perforated gate A/B failed: {:?}", ab_verdict.reasons);

    let scores = [0.02, 0.04, 0.05, 0.14, 0.17, 0.21];
    let hits = [false, false, false, true, true, true];
    let platt_params = calibrate_vision_scores(&scores, &hits, 2)
        .ok_or_else(|| anyhow::anyhow!("platt_fit_or_prior returned None"))?;

    // Verify toast redaction zeroizes target corner without bleeding into unaffected patches.
    let (w, h, max_patches) = (128usize, 96usize, 64usize);
    let base_rgb = vec![180u8; w * h * 3];
    let mut redacted_rgb = base_rgb.clone();
    let masked = redact_rgb_for_naflex(
        &mut redacted_rgb,
        w,
        h,
        &[(0.78, 0.78, 0.20, 0.20)],
        max_patches,
        &Preprocess::RAON_VE,
    );
    let grid_clean = patchify_frame_with(&base_rgb, w, h, max_patches, &Preprocess::RAON_VE);
    let grid_masked = patchify_frame_with(&redacted_rgb, w, h, max_patches, &Preprocess::RAON_VE);
    anyhow::ensure!(!masked.is_empty(), "expected non-empty toast mask indices");
    let cell0_clean = grid_clean.patch(0);
    let cell0_masked = grid_masked.patch(0);
    let diff0: f32 = cell0_clean
        .iter()
        .zip(cell0_masked)
        .map(|(a, b)| (a - b).abs())
        .sum();
    anyhow::ensure!(diff0 < 1e-6, "redaction bled into corner patch 0 (diff={diff0})");

    // Verify LogLumSensor purge & epoch watch invalidation.
    let mut sensor = LogLumSensor::new(64, 64, 0.25);
    let frame = vec![120u8; 64 * 64 * 4];
    let st0 = sensor.process_bgra(&frame);
    anyhow::ensure!(sensor.is_primed() && st0.count.iter().all(|&c| c == 0));
    sensor.purge_privacy_residue();
    anyhow::ensure!(!sensor.is_primed());
    let reprime_sentinel_ok = REPRIME_THRESHOLD_MILLI == u32::MAX;

    let epoch = AtomicU64::new(3);
    let watch = EpochWatch::capture(&epoch);
    anyhow::ensure!(watch.is_current());
    epoch.fetch_add(1, Ordering::Release);
    let privacy_epoch_aborts = watch.check().is_err();

    Ok(ScreenGateReport {
        ab_verdict,
        platt_params,
        toast_masked_cells: masked.len(),
        reprime_sentinel_ok,
        privacy_epoch_aborts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_gate_verifies_perforation_ab_redaction_and_privacy_epoch() {
        let rep = run_screen_eval_suite().unwrap();
        assert!(rep.ab_verdict.passed);
        assert!(rep.ab_verdict.waste_reduction > 0.35);
        assert!(rep.toast_masked_cells > 0);
        assert!(rep.reprime_sentinel_ok);
        assert!(rep.privacy_epoch_aborts);
    }
}
