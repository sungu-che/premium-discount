//! 3D stereo depth, rigid registration, and mask postprocessing evaluator over `lv-spatial` & `premium-discount-eval`.

use image::{GrayImage, Luma};
use lv_spatial::fit3::align_rigid;
use lv_spatial::frame::{DepthKind, Rgb};
use lv_spatial::mask_ops::postprocess::{apply as postprocess_mask, PostprocessParams};
use lv_spatial::stereo::{sgbm, SgbmCfg, StereoRig};
use premium_discount_eval::calib::average_precision;
use premium_discount_eval::rule::{summarize_scenarios, EvalCell, Rule, ScenarioSummary};

#[derive(Debug, Clone)]
pub struct SpatialGateReport {
    pub valid_frac: f32,
    pub median_abs_err_m: f32,
    pub flat_valid_count: u32,
    pub rigid_max_err: f64,
    pub hole_filled: bool,
    pub avg_precision: f32,
    pub summary: ScenarioSummary,
}

fn make_stereo_pair(w: u32, h: u32, shift: u32) -> (Rgb, Rgb) {
    let mut s = 0x1234_5678_9ABC_DEF0u64;
    let mut raw = vec![0.0f32; (w * h) as usize];
    for v in raw.iter_mut() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *v = 40.0 + 170.0 * ((s >> 33) as f32 / (1u64 << 31) as f32);
    }
    let mut tex = raw.clone();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = (y * w + x) as usize;
            tex[i] = 0.5 * raw[i]
                + 0.125 * (raw[i - 1] + raw[i + 1] + raw[i - w as usize] + raw[i + w as usize]);
        }
    }
    let mut left = Vec::with_capacity((w * h * 3) as usize);
    let mut right = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let vl = tex[(y * w + x) as usize].round().clamp(0.0, 240.0) as u8;
            left.extend_from_slice(&[vl, vl, vl]);
            let xr = (x + shift).min(w - 1);
            let vr = tex[(y * w + xr) as usize].round().clamp(0.0, 240.0) as u8;
            right.extend_from_slice(&[vr, vr, vr]);
        }
    }
    (Rgb::new(w, h, left), Rgb::new(w, h, right))
}

pub fn run_spatial_eval_suite() -> anyhow::Result<SpatialGateReport> {
    let (w, h, shift) = (288u32, 192u32, 12u32);
    let rig = StereoRig {
        focal_px: 205.0,
        baseline_m: 0.05,
        subpixel_px: 0.25,
    };
    let expected_z = rig.focal_px * rig.baseline_m / shift as f32;
    let (left, right) = make_stereo_pair(w, h, shift);
    let out = sgbm(&left, &right, &rig, &SgbmCfg::default(), None)?;
    anyhow::ensure!(out.depth.kind == DepthKind::Metric);

    let mut errs = Vec::new();
    for y in 12..h - 12 {
        for x in 32..w - 12 {
            let z = out.depth.at(x, y);
            if z.is_finite() {
                errs.push((z - expected_z).abs());
            }
        }
    }
    errs.sort_by(f32::total_cmp);
    let median_abs_err_m = *errs.get(errs.len() / 2).unwrap_or(&f32::INFINITY);

    let flat = Rgb::new(192, 128, vec![128u8; 192 * 128 * 3]);
    let flat_out = sgbm(&flat, &flat, &rig, &SgbmCfg::default(), None)?;

    // Rigid 3D alignment check (s = 1.0)
    let src = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]];
    let dst = src.map(|[x, y, z]| [-y + 0.5, x - 1.0, z + 2.0]);
    let sim = align_rigid(&src, &dst).ok_or_else(|| anyhow::anyhow!("align_rigid failed"))?;
    let rigid_max_err = src
        .iter()
        .zip(&dst)
        .map(|(a, b)| {
            let p = sim.apply(*a);
            ((p[0] - b[0]).powi(2) + (p[1] - b[1]).powi(2) + (p[2] - b[2]).powi(2)).sqrt()
        })
        .fold(0.0f64, f64::max);

    // Pure geometry mask postprocessing (1px hole area=2 rule with holes=2)
    let mut mask = GrayImage::from_pixel(9, 9, Luma([0u8]));
    for y in 2..=6 {
        for x in 2..=6 {
            if !(x == 4 && y == 4) {
                mask.put_pixel(x, y, Luma([255u8]));
            }
        }
    }
    let clean = postprocess_mask(
        &mask,
        &PostprocessParams { holes: 2, dots: 1, border_fix: 0, grow: 0 },
    )?;
    let hole_filled = clean.get_pixel(4, 4)[0] == 255;

    let avg_precision = average_precision(&[0.95, 0.85, 0.40, 0.15], &[true, true, false, false])
        .unwrap_or(0.0);

    let cells = vec![
        EvalCell::evaluate("sgbm", "stereo", "median_abs_err_m", Some(median_abs_err_m), Rule::AtMost { limit: 0.03 }, false),
        EvalCell::evaluate("sgbm", "stereo", "valid_frac", Some(out.stats.valid_frac()), Rule::AtLeast { reference: 0.70, tolerance: 0.10 }, false),
        EvalCell::evaluate("fit3", "rigid", "rigid_max_err", Some(rigid_max_err as f32), Rule::AtMost { limit: 1e-6 }, false),
    ];
    let summary = summarize_scenarios("spatial", &cells);

    Ok(SpatialGateReport {
        valid_frac: out.stats.valid_frac(),
        median_abs_err_m,
        flat_valid_count: flat_out.stats.valid,
        rigid_max_err,
        hole_filled,
        avg_precision,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_gate_verifies_sgbm_fit3_mask_ops_and_summary() {
        let rep = run_spatial_eval_suite().unwrap();
        assert!(rep.summary.passed, "{:?}", rep.summary);
        assert_eq!(rep.flat_valid_count, 0);
        assert!(rep.rigid_max_err < 1e-9);
        assert!(rep.hole_filled);
        assert!(rep.avg_precision > 0.99);
    }
}
