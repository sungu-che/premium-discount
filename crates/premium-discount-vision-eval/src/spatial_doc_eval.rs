//! 3D x 2D metric planar bridge evaluator over `lv-planar`, `lv-spatial`, and `lv-domain`.

use std::sync::atomic::AtomicU64;

use lv_doc::vision::plan::Baseline;
use lv_domain::{verify_iso6346_check_digit, CodeScheme};
use lv_planar::anchor3d::backproject_rect_point;
use lv_planar::doc_bridge::{prepare_rectified_document, repair_surface_identifier};
use lv_planar::gate::{
    evaluate_readability, ReadabilityBlockReason, ReadabilityGateCfg, ReadabilityVerdict,
};
use lv_planar::homography::{compute_planar_homography, unwarp_planar_patch};
use lv_planar::screen_bridge::{
    inspect_and_guard_3d_screen, PlanarScreenGuardCfg, PlanarScreenOutcome, ScreenRedactReason,
};
use lv_spatial::camera::{Intrinsics, Pose};
use lv_spatial::frame::{DepthKind, DepthMap, Rgb};
use lv_spatial::planar::{fit_planar_patch, PlanarCandidateRoi, PlanarFitCfg, SurfaceClassHint};

#[derive(Debug, Clone)]
pub struct SpatialDocBridgeReport {
    pub far_blocked: bool,
    pub near_px_per_mm: f32,
    pub rectified_dims: (u32, u32),
    pub iso_repaired: String,
    pub iso_valid: bool,
    pub backproject_finite: bool,
    pub screen_redacted_pixels: usize,
}

fn synth_frontal_depth(
    intr: &Intrinsics,
    pose: &Pose,
    z_m: f64,
    half_u: f64,
    half_v: f64,
    hint: SurfaceClassHint,
) -> (DepthMap, PlanarCandidateRoi) {
    let (w, h) = (intr.width, intr.height);
    let mut data = vec![f32::NAN; (w * h) as usize];
    let (fwd, right, up) = intr.camera_axes(pose.yaw);
    let eye = [pose.x as f64, pose.eye_m as f64, pose.z as f64];
    let n_w = [-fwd[0] as f64, -fwd[1] as f64, -fwd[2] as f64];
    let u_w = [right[0] as f64, right[1] as f64, right[2] as f64];
    let v_w = [-up[0] as f64, -up[1] as f64, -up[2] as f64];
    let center = [
        eye[0] + z_m * fwd[0] as f64,
        eye[1] + z_m * fwd[1] as f64,
        eye[2] + z_m * fwd[2] as f64,
    ];
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (w, 0u32, h, 0u32);
    for py in 0..h {
        for px in 0..w {
            let r = intr.ray(pose.yaw, px as f32 + 0.5, py as f32 + 0.5);
            let dir = [r[0] as f64, r[1] as f64, r[2] as f64];
            let denom = dir[0] * n_w[0] + dir[1] * n_w[1] + dir[2] * n_w[2];
            if denom.abs() < 1e-6 {
                continue;
            }
            let oc = [center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]];
            let t = (oc[0] * n_w[0] + oc[1] * n_w[1] + oc[2] * n_w[2]) / denom;
            if t <= 0.1 {
                continue;
            }
            let rel = [
                eye[0] + t * dir[0] - center[0],
                eye[1] + t * dir[1] - center[1],
                eye[2] + t * dir[2] - center[2],
            ];
            let u = rel[0] * u_w[0] + rel[1] * u_w[1] + rel[2] * u_w[2];
            let v = rel[0] * v_w[0] + rel[1] * v_w[1] + rel[2] * v_w[2];
            if u.abs() <= half_u && v.abs() <= half_v {
                let z_cam =
                    dir[0] * fwd[0] as f64 + dir[1] * fwd[1] as f64 + dir[2] * fwd[2] as f64;
                data[(py * w + px) as usize] = (t * z_cam) as f32;
                min_x = min_x.min(px);
                max_x = max_x.max(px + 1);
                min_y = min_y.min(py);
                max_y = max_y.max(py + 1);
            }
        }
    }
    (
        DepthMap { width: w, height: h, kind: DepthKind::Metric, data },
        PlanarCandidateRoi { x0: min_x, y0: min_y, x1: max_x.min(w), y1: max_y.min(h), hint },
    )
}

pub fn run_spatial_doc_eval_suite() -> anyhow::Result<SpatialDocBridgeReport> {
    let intr = Intrinsics::pinhole(640, 480, 22.0);
    let mut pose = Pose::at_pos(0.0, 1.5, 0.0);
    pose.yaw = std::f32::consts::FRAC_PI_2;
    let fit_cfg = PlanarFitCfg::default();
    let gate_cfg = ReadabilityGateCfg::default();

    // 1. Far nameplate must block with NeedReposition (LowEffectiveDpi), triggering 0 VLM calls.
    let (dm_far, roi_far) =
        synth_frontal_depth(&intr, &pose, 3.1, 0.09, 0.06, SurfaceClassHint::IndustrialNameplate);
    let patch_far = fit_planar_patch(&roi_far, &dm_far, None, &intr, &pose, None, &fit_cfg)
        .ok_or_else(|| anyhow::anyhow!("far patch fit failed"))?;
    let far_blocked = matches!(
        evaluate_readability(&patch_far, &intr, &pose, &gate_cfg),
        ReadabilityVerdict::NeedReposition(app)
            if app.reason == ReadabilityBlockReason::LowEffectiveDpi && app.required_px_per_mm >= 5.5
    );

    // 2. Near nameplate passes >= 5.5 px/mm, rectifies, repairs ISO 6346, and backprojects to finite 3D.
    let (dm_near, roi_near) =
        synth_frontal_depth(&intr, &pose, 0.28, 0.035, 0.0225, SurfaceClassHint::IndustrialNameplate);
    let patch_near = fit_planar_patch(&roi_near, &dm_near, None, &intr, &pose, None, &fit_cfg)
        .ok_or_else(|| anyhow::anyhow!("near patch fit failed"))?;
    let near_px_per_mm = match evaluate_readability(&patch_near, &intr, &pose, &gate_cfg) {
        ReadabilityVerdict::Pass { min_px_per_mm } => min_px_per_mm,
        other => anyhow::bail!("expected Pass for near nameplate, got {other:?}"),
    };
    let hom = compute_planar_homography(&patch_near, &intr, &pose, 5.5, 4.0, 2016)
        .ok_or_else(|| anyhow::anyhow!("compute_planar_homography returned None"))?;
    let rgb = Rgb::new(640, 480, vec![210u8; 640 * 480 * 3]);
    let rect = unwarp_planar_patch(&rgb, hom);
    let _prep = prepare_rectified_document(&rect, Baseline::Best, None)
        .ok_or_else(|| anyhow::anyhow!("prepare_rectified_document returned None"))?;
    let iso_repaired = repair_surface_identifier("MSCU1Z34S66", CodeScheme::Iso6346Container);
    let iso_valid = verify_iso6346_check_digit(&iso_repaired);
    let (w_pt, _px_pt) = backproject_rect_point(
        &rect.homography,
        &patch_near,
        0.5 * rect.width as f32,
        0.5 * rect.height as f32,
        Some(&dm_near),
        &intr,
        &pose,
    );
    let backproject_finite = w_pt.iter().all(|c| c.is_finite());

    // 3. Display screen fails closed & zeroizes perspective quad under elevated privacy epoch.
    let (dm_disp, roi_disp) =
        synth_frontal_depth(&intr, &pose, 0.45, 0.08, 0.05, SurfaceClassHint::DisplayScreen);
    let patch_disp = fit_planar_patch(&roi_disp, &dm_disp, None, &intr, &pose, None, &fit_cfg)
        .ok_or_else(|| anyhow::anyhow!("display patch fit failed"))?;
    let hom_disp = compute_planar_homography(&patch_disp, &intr, &pose, 2.5, 0.0, 1024)
        .ok_or_else(|| anyhow::anyhow!("display homography returned None"))?;
    let mut disp_rgb = Rgb::new(640, 480, vec![200u8; 640 * 480 * 3]);
    let mut disp_rect = unwarp_planar_patch(&disp_rgb, hom_disp);
    let epoch = AtomicU64::new(1);
    let screen_redacted_pixels = match inspect_and_guard_3d_screen(
        &mut disp_rgb,
        &mut disp_rect,
        &patch_disp,
        None,
        &epoch,
        0,
        &PlanarScreenGuardCfg::default(),
    )? {
        PlanarScreenOutcome::PrivacyRedacted { zeroized_pixels, reason, .. } => {
            anyhow::ensure!(reason == ScreenRedactReason::EpochChangedDuringPass);
            zeroized_pixels
        }
        PlanarScreenOutcome::Clear(_) => anyhow::bail!("expected DisplayScreen fail-closed redaction"),
    };
    anyhow::ensure!(disp_rect.rgb.iter().all(|&b| b == 0));

    Ok(SpatialDocBridgeReport {
        far_blocked,
        near_px_per_mm,
        rectified_dims: (rect.width, rect.height),
        iso_repaired,
        iso_valid,
        backproject_finite,
        screen_redacted_pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_doc_eval_verifies_lv_planar_3d_2d_bridge() {
        let rep = run_spatial_doc_eval_suite().unwrap();
        assert!(rep.far_blocked);
        assert!(rep.near_px_per_mm >= 5.5);
        assert_eq!(rep.iso_repaired, "MSCU1234566");
        assert!(rep.iso_valid);
        assert!(rep.backproject_finite);
        assert!(rep.screen_redacted_pixels > 10_000);
    }
}
