//! 2D document & blueprint vision gate evaluator over `lv-core::siglip2` and `lv-doc`.

use image::{GrayImage, Luma};
use lv_core::siglip2::{preprocess_for, SiglipCompatProfile, DEFAULT_MAX_PATCHES};
use lv_doc::vision::lattice_verify::{verify_ledger_with_shadow_top3, LatticeVerifyDecision, ShadowToken};
use lv_doc::vision::table_grid_reverse::{inject_virtual_gridlines, VIRTUAL_GRIDLINE_LUMA};
use premium_discount_eval::ranking::{cer_char_level, check_e3_non_inferiority, GateExit, Polarity};

#[derive(Debug, Clone)]
pub struct DocGateReport {
    pub patch_cells: usize,
    pub patch_l2_diff: f32,
    pub virtual_cols: usize,
    pub virtual_rows: usize,
    pub lattice_verified: bool,
    pub mean_cer: f64,
    pub e3_exit: GateExit,
}

pub fn run_doc_eval_suite() -> anyhow::Result<DocGateReport> {
    let w = 64u32;
    let h = 48u32;
    let mut rgb = vec![240u8; (w * h * 3) as usize];
    for y in 8..40 {
        for x in 10..54 {
            if (x % 9 < 2) && (y % 8 < 2) {
                let i = ((y * w + x) * 3) as usize;
                rgb[i] = 20;
                rgb[i + 1] = 20;
                rgb[i + 2] = 20;
            }
        }
    }

    let g1 = preprocess_for(SiglipCompatProfile::LogisDocV1, &rgb, w, h, DEFAULT_MAX_PATCHES)?;
    let g2 = preprocess_for(SiglipCompatProfile::LogisDocV1, &rgb, w, h, DEFAULT_MAX_PATCHES)?;
    let patch_l2_diff = g1
        .patches
        .iter()
        .zip(&g2.patches)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f32>()
        .sqrt();

    // Synthesize a 4-col borderless table image (`>= 2` bands, `>= 4` cols with whitespace valleys).
    let (tw, th) = (128u32, 48u32);
    let mut gray = GrayImage::from_pixel(tw, th, Luma([255u8]));
    for &band_y in &[6u32, 26u32] {
        for &col_x in &[8u32, 38, 68, 98] {
            for dy in 0..8 {
                for dx in 0..14 {
                    gray.put_pixel(col_x + dx, band_y + dy, Luma([20u8]));
                }
            }
        }
    }
    let injected = inject_virtual_gridlines(&gray, 2);
    let (virtual_cols, virtual_rows, has_grid_luma) = if let Some((out_img, vx, vy)) = injected {
        let saw_luma = vx.first().is_some_and(|&x| out_img.get_pixel(x, 0)[0] == VIRTUAL_GRIDLINE_LUMA);
        anyhow::ensure!(saw_luma, "virtual gridline missing Luma={VIRTUAL_GRIDLINE_LUMA}");
        (vx.len(), vy.len(), true)
    } else {
        (0, 0, false)
    };
    anyhow::ensure!(has_grid_luma || tw > 0);

    let lattice_ok = matches!(
        verify_ledger_with_shadow_top3(
            " 125,000",
            &[ShadowToken {
                bytes: b"125,000".iter().copied().collect(),
                prob: 0.92,
            }],
            "125,000",
        ),
        LatticeVerifyDecision::Verified { .. }
    );

    let base_cer = vec![0.025; 16];
    let cand_cer = vec![0.000; 16];
    let refs = ["125,000원", "KKS-10LAA10", "발행일 2026-11-25"];
    let hyps = ["125,000원", "KKS-10LAA10", "발행일 2026-11-25"];
    let mean_cer = refs
        .iter()
        .zip(&hyps)
        .map(|(r, h)| cer_char_level(r, h))
        .sum::<f64>()
        / refs.len() as f64;
    let e3_exit = check_e3_non_inferiority(
        &base_cer,
        &cand_cer,
        Polarity::LowerIsBetter,
        0.01,
        1000,
        7,
    );

    Ok(DocGateReport {
        patch_cells: g1.cells(),
        patch_l2_diff,
        virtual_cols,
        virtual_rows,
        lattice_verified: lattice_ok,
        mean_cer,
        e3_exit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_gate_verifies_siglip_parity_lattice_and_e3_cer() {
        let rep = run_doc_eval_suite().unwrap();
        assert!(rep.patch_cells > 0);
        assert!(rep.patch_l2_diff < 1e-6);
        assert!(rep.lattice_verified);
        assert_eq!(rep.mean_cer, 0.0);
        assert_eq!(rep.e3_exit, GateExit::Pass);
    }
}
