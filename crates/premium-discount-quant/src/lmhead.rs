//! Exact decoding with a sliced output head.
//!
//! Slicing `lm_head` to `kept_ids` (PRD §3.4) is only safe if the arg-max (or top-k) never sits
//! in a dropped row. The reference implementation this project grew out of measured slicing and
//! rejected it for exactly that reason. Here the dropped ("cold") rows are clustered offline;
//! each cluster stores its centre `c` and radius `ρ = max ‖w − c‖`. For a hidden state `h`,
//! every cold logit obeys `w·h ≤ c·h + ρ‖h‖` (Cauchy–Schwarz), so after scoring the resident
//! `[K, D]` head we only re-score the clusters whose bound beats the current best — gathered row
//! by row from the on-disk table. The result equals a full `[V, D]` arg-max over the same
//! quantized rows.

use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct ColdGuard {
    pub dim: usize,
    /// `[C, D]` cluster centres.
    pub centers: Vec<f32>,
    /// `[C]` radii (already inflated by a rounding margin).
    pub radius: Vec<f32>,
    /// Cold token ids grouped by cluster.
    pub members: Vec<u32>,
    /// `[C + 1]` offsets into `members`.
    pub offsets: Vec<u32>,
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    premium_discount_cpu_kernels::dot_f32_simd(a, b)
}

/// Mean-centered Cauchy-Schwarz upper-bound guard (Pack A/C) over cold vocabulary rows:
///
/// For every cold row `w_i` with optional bias `b_i`:
///   `w_i · h + b_i = mu_rest · h + (w_i - mu_rest) · h + b_i`
///   `<= mu_rest · h + w_rest_bound * ||h||_2 + b_rest_max`
/// where `mu_rest = mean(w_i)` and `w_rest_bound = max_i ||w_i - mu_rest||_2` (inflated by an
/// F32 rounding margin). When `upper_bound(h) < kept_max`, no cold row can beat the kept top-1,
/// guaranteeing 100% lossless exact argmax without evaluating cold rows.
#[derive(Debug, Clone)]
pub struct MeanCenteredCsGuard {
    pub dim: usize,
    /// Mean of all cold weight rows `[dim]`.
    pub mu_rest: Vec<f32>,
    /// `max_i ||w_i - mu_rest||_2` inflated for F32 rounding safety.
    pub w_rest_bound: f32,
    /// `max_i b_i` (0.0 when unbiased).
    pub b_rest_max: f32,
    /// Cold token ids in row order.
    pub ids: Vec<u32>,
}

impl MeanCenteredCsGuard {
    /// Builds a guard from dense F32 cold weight rows `[ids.len(), dim]` and optional biases `[ids.len()]`.
    pub fn from_rows(rows: &[f32], ids: &[u32], bias: Option<&[f32]>, dim: usize) -> Self {
        let n = ids.len();
        assert_eq!(rows.len(), n * dim);
        if let Some(b) = bias {
            assert_eq!(b.len(), n);
        }
        if n == 0 {
            return Self {
                dim,
                mu_rest: vec![0.0; dim],
                w_rest_bound: 0.0,
                b_rest_max: f32::NEG_INFINITY,
                ids: Vec::new(),
            };
        }
        let mut sum = vec![0.0f64; dim];
        for r in rows.chunks_exact(dim) {
            for (s, &v) in sum.iter_mut().zip(r) {
                *s += v as f64;
            }
        }
        let inv_n = 1.0 / n as f64;
        let mu_rest: Vec<f32> = sum.iter().map(|&s| (s * inv_n) as f32).collect();
        let max_dev_sq: f64 = rows
            .par_chunks_exact(dim)
            .map(|r| {
                r.iter()
                    .zip(&mu_rest)
                    .map(|(&w, &m)| {
                        let d = w as f64 - m as f64;
                        d * d
                    })
                    .sum::<f64>()
            })
            .reduce(|| 0.0f64, f64::max);
        let w_rest_bound = (max_dev_sq.sqrt() as f32) * (1.0 + 1e-4) + 1e-6;
        let b_rest_max = bias
            .map(|b| b.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        Self {
            dim,
            mu_rest,
            w_rest_bound,
            b_rest_max,
            ids: ids.to_vec(),
        }
    }

    /// Strict upper bound on `max_{i in cold} (w_i · h + b_i)` with F32 rounding margin.
    #[inline]
    pub fn upper_bound(&self, h: &[f32], h_norm: f32) -> f32 {
        if self.ids.is_empty() {
            return f32::NEG_INFINITY;
        }
        let mu_h = dot(&self.mu_rest, h);
        let dev = self.w_rest_bound * h_norm;
        let ub = mu_h + dev + self.b_rest_max;
        ub + 1e-3 * (mu_h.abs() + dev) + 1e-4
    }

    /// Returns `true` iff any cold token could possibly tie or beat `kept_max`.
    #[inline]
    pub fn needs_rescue(&self, kept_max: f32, h: &[f32], h_norm: f32) -> bool {
        !self.ids.is_empty() && self.upper_bound(h, h_norm) >= kept_max
    }

    /// Exact top-1 over `kept_ids` union `self.ids` using the single-vector mean-centered CS guard.
    /// Returns `(best_id, best_logit, rescued)` where `rescued == true` iff cold rows were evaluated.
    pub fn exact_argmax(
        &self,
        kept_logits: &[f32],
        kept_ids: &[u32],
        h: &[f32],
        cold_logit: impl Fn(u32) -> f32,
    ) -> (u32, f32, bool) {
        let (mut best_id, mut best) = (u32::MAX, f32::NEG_INFINITY);
        for (&l, &id) in kept_logits.iter().zip(kept_ids) {
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
        let h_norm = dot(h, h).sqrt();
        if !self.needs_rescue(best, h, h_norm) {
            return (best_id, best, false);
        }
        for &id in &self.ids {
            let l = cold_logit(id);
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
        (best_id, best, true)
    }
}

impl ColdGuard {
    /// Save the precomputed cluster guard to a compact binary cache (`.cold_guard.bin`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        f.write_all(b"LCCG")?;
        f.write_all(&1u32.to_le_bytes())?;
        f.write_all(&(self.dim as u32).to_le_bytes())?;
        f.write_all(&(self.radius.len() as u32).to_le_bytes())?;
        f.write_all(&(self.members.len() as u32).to_le_bytes())?;
        for &v in &self.centers {
            f.write_all(&v.to_le_bytes())?;
        }
        for &v in &self.radius {
            f.write_all(&v.to_le_bytes())?;
        }
        for &v in &self.members {
            f.write_all(&v.to_le_bytes())?;
        }
        for &v in &self.offsets {
            f.write_all(&v.to_le_bytes())?;
        }
        f.flush()?;
        Ok(())
    }

    /// Load a cached cluster guard from `.cold_guard.bin`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        if bytes.len() < 20 || &bytes[0..4] != b"LCCG" {
            anyhow::bail!("invalid cold guard cache header");
        }
        let ver = u32::from_le_bytes(bytes[4..8].try_into()?);
        if ver != 1 {
            anyhow::bail!("unsupported cold guard version {ver}");
        }
        let dim = u32::from_le_bytes(bytes[8..12].try_into()?) as usize;
        let c = u32::from_le_bytes(bytes[12..16].try_into()?) as usize;
        let n = u32::from_le_bytes(bytes[16..20].try_into()?) as usize;
        let need = 20 + (c * dim + c + n + (c + 1)) * 4;
        if bytes.len() != need {
            anyhow::bail!("cold guard cache size {} != expected {need}", bytes.len());
        }
        let mut p = 20usize;
        let read_f32s = |bytes: &[u8], pos: &mut usize, cnt: usize| -> Vec<f32> {
            let out: Vec<f32> = bytes[*pos..*pos + cnt * 4]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            *pos += cnt * 4;
            out
        };
        let read_u32s = |bytes: &[u8], pos: &mut usize, cnt: usize| -> Vec<u32> {
            let out: Vec<u32> = bytes[*pos..*pos + cnt * 4]
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            *pos += cnt * 4;
            out
        };
        let centers = read_f32s(&bytes, &mut p, c * dim);
        let radius = read_f32s(&bytes, &mut p, c);
        let members = read_u32s(&bytes, &mut p, n);
        let offsets = read_u32s(&bytes, &mut p, c + 1);
        Ok(Self {
            dim,
            centers,
            radius,
            members,
            offsets,
        })
    }
}

/// Lloyd k-means (Euclidean) over the decoded cold rows `rows` (`n × dim`), seeded with evenly
/// strided rows. Deterministic.
pub fn build_cold_guard(rows: &[f32], ids: &[u32], dim: usize, clusters: usize, iters: usize) -> ColdGuard {
    let n = ids.len();
    assert_eq!(rows.len(), n * dim);
    if n == 0 {
        return ColdGuard {
            dim,
            centers: vec![],
            radius: vec![],
            members: vec![],
            offsets: vec![0],
        };
    }
    let k = clusters.clamp(1, n);
    let mut centers: Vec<f32> = (0..k)
        .flat_map(|c| rows[(c * n / k) * dim..(c * n / k + 1) * dim].to_vec())
        .collect();
    let mut assign = vec![0u32; n];
    for it in 0..=iters {
        let cn: Vec<f32> = centers.chunks_exact(dim).map(|c| dot(c, c)).collect();
        assign.par_iter_mut().enumerate().for_each(|(i, a)| {
            let w = &rows[i * dim..(i + 1) * dim];
            let mut best = (f32::INFINITY, 0u32);
            for (c, cv) in centers.chunks_exact(dim).enumerate() {
                let d = cn[c] - 2.0 * dot(w, cv);
                if d < best.0 {
                    best = (d, c as u32);
                }
            }
            *a = best.1;
        });
        if it == iters {
            break;
        }
        let mut sums = vec![0f64; k * dim];
        let mut cnt = vec![0usize; k];
        for (i, &a) in assign.iter().enumerate() {
            cnt[a as usize] += 1;
            for (s, &v) in sums[a as usize * dim..(a as usize + 1) * dim]
                .iter_mut()
                .zip(&rows[i * dim..(i + 1) * dim])
            {
                *s += v as f64;
            }
        }
        for c in 0..k {
            if cnt[c] > 0 {
                for d in 0..dim {
                    centers[c * dim + d] = (sums[c * dim + d] / cnt[c] as f64) as f32;
                }
            }
        }
    }
    let mut radius = vec![0f32; k];
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); k];
    for (i, &a) in assign.iter().enumerate() {
        let (w, c) = (&rows[i * dim..(i + 1) * dim], &centers[a as usize * dim..(a as usize + 1) * dim]);
        let r = w.iter().zip(c).map(|(x, y)| ((x - y) as f64).powi(2)).sum::<f64>().sqrt() as f32;
        radius[a as usize] = radius[a as usize].max(r);
        buckets[a as usize].push(ids[i]);
    }
    for r in &mut radius {
        *r = *r * (1.0 + 1e-4) + 1e-6;
    }
    let mut members = Vec::with_capacity(n);
    let mut offsets = vec![0u32];
    for b in buckets {
        members.extend(b);
        offsets.push(members.len() as u32);
    }
    ColdGuard {
        dim,
        centers,
        radius,
        members,
        offsets,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ArgmaxStats {
    pub clusters: usize,
    pub clusters_refined: usize,
    pub rows_refined: usize,
}

/// Exact top-1 over the full vocabulary.
///
/// * `kept_logits[i]` is the logit of token `kept_ids[i]` (from the resident sliced head);
/// * `cold_logit(id)` scores one dropped row from the on-disk table (same quantization).
pub fn exact_argmax(
    kept_logits: &[f32],
    kept_ids: &[u32],
    guard: &ColdGuard,
    h: &[f32],
    cold_logit: impl Fn(u32) -> f32,
) -> (u32, f32, ArgmaxStats) {
    let (mut best_id, mut best) = (u32::MAX, f32::NEG_INFINITY);
    for (&l, &id) in kept_logits.iter().zip(kept_ids) {
        if l > best {
            best = l;
            best_id = id;
        }
    }
    let hn = dot(h, h).sqrt();
    let c = guard.radius.len();
    let mut bounds: Vec<(f32, usize)> = (0..c)
        .map(|j| {
            let ch = dot(&guard.centers[j * guard.dim..(j + 1) * guard.dim], h);
            let ub = ch + guard.radius[j] * hn;
            // Margin for f32 rounding differences between the bound and the row kernels.
            (ub + 1e-3 * (ch.abs() + guard.radius[j] * hn) + 1e-4, j)
        })
        .collect();
    bounds.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut st = ArgmaxStats {
        clusters: c,
        ..Default::default()
    };
    for (ub, j) in bounds {
        if ub < best {
            break;
        }
        st.clusters_refined += 1;
        for &id in &guard.members[guard.offsets[j] as usize..guard.offsets[j + 1] as usize] {
            st.rows_refined += 1;
            let l = cold_logit(id);
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
    }
    (best_id, best, st)
}

/// Spherical Cauchy-Schwarz guard over **L2-normalized decoded unit vectors** `w_hat_i = w_i / ||w_i||_2`.
///
/// Unlike [`ColdGuard`] (which bounds unnormalized inner products `w_i . h`), cosine matching must
/// cluster strictly on the unit sphere so per-row norm differences cannot invalidate the upper bound:
///
///   `cos(w_i, q_unit) = w_hat_i . q_unit <= c_m . q_unit + rho_m + eps_f32`
///
/// where `rho_m = (1 + 1e-4) * max_{i in C_m} ||w_hat_i - c_m||_2 + 1e-6`.
#[derive(Debug, Clone)]
pub struct UnitColdGuard {
    pub dim: usize,
    /// `[C, D]` spherical cluster centers.
    pub centers: Vec<f32>,
    /// `[C]` strict inflated radii on the unit sphere.
    pub radius: Vec<f32>,
    /// Cold row ids grouped by cluster.
    pub members: Vec<u32>,
    /// `[C + 1]` offsets into `members`.
    pub offsets: Vec<u32>,
}

fn normalize_unit_rows(rows: &[f32], n: usize, dim: usize) -> Vec<f32> {
    let mut unit = vec![0.0f32; n * dim];
    for i in 0..n {
        let src = &rows[i * dim..(i + 1) * dim];
        let norm = src.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>().sqrt();
        if norm > 1e-12 && norm.is_finite() {
            let dst = &mut unit[i * dim..(i + 1) * dim];
            for (d, &s) in dst.iter_mut().zip(src) {
                *d = (s as f64 / norm) as f32;
            }
        }
    }
    unit
}

/// Build a [`UnitColdGuard`] strictly on L2-normalized unit rows (`rows`: `[ids.len(), dim]`).
pub fn build_unit_cold_guard(rows: &[f32], ids: &[u32], dim: usize, clusters: usize, iters: usize) -> UnitColdGuard {
    let n = ids.len();
    assert_eq!(rows.len(), n * dim);
    let unit_rows = normalize_unit_rows(rows, n, dim);
    let base = build_cold_guard(&unit_rows, ids, dim, clusters, iters);
    UnitColdGuard {
        dim: base.dim,
        centers: base.centers,
        radius: base.radius,
        members: base.members,
        offsets: base.offsets,
    }
}

impl UnitColdGuard {
    /// Upper bound `U_m(q_unit) = c_m . q_unit + rho_m + eps` for cluster `j` against a unit query `q_unit`.
    #[inline]
    pub fn cluster_upper_bound(&self, j: usize, q_unit: &[f32]) -> f32 {
        let c = &self.centers[j * self.dim..(j + 1) * self.dim];
        let ch = dot(c, q_unit);
        let r = self.radius[j];
        ch + r + 1e-3 * (ch.abs() + r) + 1e-4
    }

    /// Biased cosine argmax with sector prior `beta_sector` and cold noise margin `delta_q4`:
    ///
    /// - Sets gate `S_gate = best_active + beta_sector`.
    /// - Skips any cold cluster `m` where `U_m(q_unit) - delta_q4 <= S_gate`.
    /// - Evaluates crossing clusters via `cold_cosine(id)` and lets a cold candidate win iff
    ///   `s_cold - delta_q4 > S_gate` (and exceeds previous best cold candidate).
    pub fn biased_cosine_argmax(
        &self,
        active_cosines: &[f32],
        active_ids: &[u32],
        q_unit: &[f32],
        beta_sector: f32,
        delta_q4: f32,
        cold_cosine: impl Fn(u32) -> f32,
    ) -> (u32, f32, bool, ArgmaxStats) {
        let (mut best_id, mut best_active) = (u32::MAX, f32::NEG_INFINITY);
        for (&s, &id) in active_cosines.iter().zip(active_ids) {
            if s > best_active || (s == best_active && id < best_id) {
                best_active = s;
                best_id = id;
            }
        }
        let s_gate = best_active + beta_sector;
        let c = self.radius.len();
        let mut bounds: Vec<(f32, usize)> = (0..c)
            .map(|j| (self.cluster_upper_bound(j, q_unit), j))
            .collect();
        bounds.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut st = ArgmaxStats {
            clusters: c,
            ..Default::default()
        };
        let mut best_score = best_active;
        let mut rescued = false;
        let mut threshold = s_gate;
        for (ub, j) in bounds {
            if ub - delta_q4 <= threshold {
                break;
            }
            st.clusters_refined += 1;
            for &id in &self.members[self.offsets[j] as usize..self.offsets[j + 1] as usize] {
                st.rows_refined += 1;
                let sc = cold_cosine(id);
                let eff = sc - delta_q4;
                if eff > threshold || (eff == threshold && rescued && id < best_id) {
                    threshold = eff;
                    best_score = sc;
                    best_id = id;
                    rescued = true;
                }
            }
        }
        (best_id, best_score, rescued, st)
    }

    /// [`biased_cosine_argmax`](Self::biased_cosine_argmax) for scores on a calibrated scale
    /// (`calibrate_knots(raw_cosine, knots)`, e.g. logis-center's `CLS_TO_LEGACY_COS`): the gate and the sector prior
    /// are in calibrated units, the cluster bounds stay raw unit-sphere cosines.
    ///
    /// - `active_scores` are calibrated; `S_gate = best_active + beta_sector`.
    /// - A cold row competes with `calibrate_knots(raw − delta_q4)` (`delta_q4`: the raw Q4 noise margin) and wins
    ///   when that exceeds the gate (equal scores: lower id), exactly what a full scan with the same rule decides.
    /// - A cold cluster is skipped only when `U_m(q) − delta_q4` is at or below a raw gate that is never above
    ///   `f⁻¹(S_gate)` ([`uncalibrate_knots`] moved past f32 rounding), so no row the calibrated rule accepts is pruned.
    ///   Comparing the raw bound with the calibrated gate itself would prune rows that pass.
    ///
    /// Returns `(id, calibrated score, rescued, stats)`.
    #[allow(clippy::too_many_arguments)]
    pub fn biased_cosine_argmax_calibrated(
        &self,
        active_scores: &[f32],
        active_ids: &[u32],
        q_unit: &[f32],
        beta_sector: f32,
        delta_q4: f32,
        knots: &[(f32, f32)],
        cold_cosine: impl Fn(u32) -> f32,
    ) -> (u32, f32, bool, ArgmaxStats) {
        let (mut best_id, mut best_active) = (u32::MAX, f32::NEG_INFINITY);
        for (&s, &id) in active_scores.iter().zip(active_ids) {
            if s > best_active || (s == best_active && id < best_id) {
                best_active = s;
                best_id = id;
            }
        }
        let c = self.radius.len();
        let mut bounds: Vec<(f32, usize)> = (0..c)
            .map(|j| (self.cluster_upper_bound(j, q_unit), j))
            .collect();
        bounds.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut st = ArgmaxStats {
            clusters: c,
            ..Default::default()
        };
        let mut best_score = best_active;
        let mut rescued = false;
        let mut threshold = best_active + beta_sector;
        let mut raw_gate = uncalibrate_knots_floor(threshold, knots);
        for (ub, j) in bounds {
            if ub - delta_q4 <= raw_gate {
                break;
            }
            st.clusters_refined += 1;
            for &id in &self.members[self.offsets[j] as usize..self.offsets[j + 1] as usize] {
                st.rows_refined += 1;
                let sc = cold_cosine(id);
                let eff = calibrate_knots(sc - delta_q4, knots);
                if eff > threshold || (eff == threshold && rescued && id < best_id) {
                    threshold = eff;
                    raw_gate = uncalibrate_knots_floor(threshold, knots);
                    best_score = calibrate_knots(sc, knots);
                    best_id = id;
                    rescued = true;
                }
            }
        }
        (best_id, best_score, rescued, st)
    }
}

/// Monotone calibration tables (`calibrate_knots` is logis-center's `interp_knots` arithmetic, bit for bit) and the
/// inverse used by [`UnitColdGuard::biased_cosine_argmax_calibrated`]; they live in `premium-discount-sds::knots`
/// so wasm32 crates (nms) share them.
pub use premium_discount_sds::knots::{calibrate_knots, uncalibrate_knots, uncalibrate_knots_floor};

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        ((*seed >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    }

    #[test]
    fn sliced_argmax_equals_full_argmax() {
        let (v, d) = (3000usize, 64usize);
        let mut s = 42u64;
        let table: Vec<f32> = (0..v * d).map(|_| rng(&mut s)).collect();
        let kept: Vec<u32> = (0..v as u32).filter(|i| i % 3 != 0).collect();
        let cold: Vec<u32> = (0..v as u32).filter(|i| i % 3 == 0).collect();
        let cold_rows: Vec<f32> = cold
            .iter()
            .flat_map(|&i| table[i as usize * d..(i as usize + 1) * d].to_vec())
            .collect();
        let g = build_cold_guard(&cold_rows, &cold, d, 32, 4);
        assert_eq!(g.members.len(), cold.len());
        let mut refined = 0;
        for trial in 0..200 {
            let mut h: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            if trial % 2 == 0 {
                // Point h at a cold row so the true arg-max is outside the kept set.
                let t = cold[trial % cold.len()] as usize;
                h.iter_mut().zip(&table[t * d..(t + 1) * d]).for_each(|(a, b)| *a = 0.1 * *a + b);
            }
            let logit = |id: u32| dot(&table[id as usize * d..(id as usize + 1) * d], &h);
            let kept_logits: Vec<f32> = kept.iter().map(|&i| logit(i)).collect();
            let (id, l, st) = exact_argmax(&kept_logits, &kept, &g, &h, logit);
            let (full_id, full_l) = (0..v as u32)
                .map(|i| (i, logit(i)))
                .fold((0, f32::NEG_INFINITY), |a, b| if b.1 > a.1 { b } else { a });
            assert_eq!((id, l), (full_id, full_l), "trial {trial}");
            refined += st.rows_refined;
        }
        // The bound prunes: on average far fewer rows than the cold set are re-scored.
        assert!(refined / 200 < cold.len(), "{refined}");
    }

    #[test]
    fn mean_centered_cs_guard_is_exact_and_prunes() {
        let (v, d) = (1024usize, 64usize);
        let mut s = 123u64;
        // Cold rows clustered tightly around a shared mean, hot rows with wider directional spread.
        let mut table = vec![0.0f32; v * d];
        let cold_mean: Vec<f32> = (0..d).map(|_| rng(&mut s) * 0.5).collect();
        let kept: Vec<u32> = (0..256u32).collect();
        let cold: Vec<u32> = (256..v as u32).collect();
        for &id in &kept {
            for j in 0..d {
                table[id as usize * d + j] = rng(&mut s);
            }
        }
        for &id in &cold {
            for j in 0..d {
                table[id as usize * d + j] = cold_mean[j] + 0.05 * rng(&mut s);
            }
        }
        let cold_rows: Vec<f32> = cold
            .iter()
            .flat_map(|&i| table[i as usize * d..(i as usize + 1) * d].to_vec())
            .collect();
        let guard = MeanCenteredCsGuard::from_rows(&cold_rows, &cold, None, d);
        let mut rescues = 0usize;
        for trial in 0..100 {
            let mut h: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            if trial % 4 == 0 {
                // Force a cold token to win.
                let t = cold[trial % cold.len()] as usize;
                for j in 0..d {
                    h[j] = table[t * d + j] * 4.0 + 0.05 * h[j];
                }
            } else {
                // Point strongly at a kept token so CS guard prunes cleanly.
                let t = kept[trial % kept.len()] as usize;
                for j in 0..d {
                    h[j] = table[t * d + j] * 3.0;
                }
            }
            let logit = |id: u32| dot(&table[id as usize * d..(id as usize + 1) * d], &h);
            let kept_logits: Vec<f32> = kept.iter().map(|&i| logit(i)).collect();
            let (id, l, rescued) = guard.exact_argmax(&kept_logits, &kept, &h, logit);
            if rescued {
                rescues += 1;
            }
            let (full_id, full_l) = (0..v as u32).map(|i| (i, logit(i))).fold(
                (u32::MAX, f32::NEG_INFINITY),
                |a, b| if b.1 > a.1 || (b.1 == a.1 && b.0 < a.0) { b } else { a },
            );
            assert_eq!((id, l), (full_id, full_l), "trial {trial}");
        }
        assert!(rescues < 100, "guard should prune hot-winning trials (rescues={rescues})");
        assert!(rescues >= 25, "cold-winning trials must rescue (rescues={rescues})");
    }

    #[test]
    fn unit_cold_guard_preserves_cosine_bound_under_varying_row_norms() {
        let (v, d) = (600usize, 64usize);
        let mut s = 777u64;
        // Intentionally give rows wildly different L2 norms (0.05..20.0) so unnormalized ColdGuard would fail cosine!
        let mut table = vec![0.0f32; v * d];
        for i in 0..v {
            let scale = if i % 2 == 0 { 0.05 } else { 20.0 };
            for j in 0..d {
                table[i * d + j] = rng(&mut s) * scale;
            }
        }
        let unit = normalize_unit_rows(&table, v, d);
        let active: Vec<u32> = (0..120u32).collect();
        let cold: Vec<u32> = (120..v as u32).collect();
        let cold_raw: Vec<f32> = cold
            .iter()
            .flat_map(|&i| table[i as usize * d..(i as usize + 1) * d].to_vec())
            .collect();
        let uguard = build_unit_cold_guard(&cold_raw, &cold, d, 32, 4);

        let mut pruned_trials = 0usize;
        for trial in 0..100 {
            let mut q: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            let target = if trial % 3 == 0 {
                cold[trial % cold.len()] as usize
            } else {
                active[trial % active.len()] as usize
            };
            for j in 0..d {
                q[j] = unit[target * d + j] * 3.0 + 0.05 * q[j];
            }
            let qn = q.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt() as f32;
            for x in &mut q {
                *x /= qn;
            }
            let cos_fn = |id: u32| dot(&unit[id as usize * d..(id as usize + 1) * d], &q);
            let act_scores: Vec<f32> = active.iter().map(|&i| cos_fn(i)).collect();

            // With beta = 0, delta_q4 = 0, unit guard must be 100% exact full cosine argmax
            let (id, sc, _rescued, st) = uguard.biased_cosine_argmax(&act_scores, &active, &q, 0.0, 0.0, cos_fn);
            let (full_id, full_sc) = (0..v as u32)
                .map(|i| (i, cos_fn(i)))
                .fold((u32::MAX, f32::NEG_INFINITY), |a, b| {
                    if b.1 > a.1 || (b.1 == a.1 && b.0 < a.0) { b } else { a }
                });
            assert_eq!(id, full_id, "trial {trial} got=({id},{sc}) full=({full_id},{full_sc})");
            if st.rows_refined < cold.len() {
                pruned_trials += 1;
            }
        }
        assert!(pruned_trials > 50, "expected spherical pruning (pruned_trials={pruned_trials})");
    }

    /// A strictly increasing 27-knot quantile table (numbers of logis-center's granite → legacy cosine map).
    const KNOTS_27: [(f32, f32); 27] = [
        (-1.0000, -1.0000),
        (0.6330, 0.1070),
        (0.6540, 0.1710),
        (0.6688, 0.2132),
        (0.6869, 0.2716),
        (0.7018, 0.3246),
        (0.7091, 0.3520),
        (0.7171, 0.3820),
        (0.7291, 0.4234),
        (0.7397, 0.4564),
        (0.7527, 0.4921),
        (0.7622, 0.5161),
        (0.7705, 0.5359),
        (0.7784, 0.5538),
        (0.7865, 0.5714),
        (0.7954, 0.5900),
        (0.8065, 0.6117),
        (0.8236, 0.6424),
        (0.8394, 0.6692),
        (0.8600, 0.7033),
        (0.8755, 0.7300),
        (0.8904, 0.7587),
        (0.9259, 0.8304),
        (0.9684, 0.9224),
        (0.9915, 0.9710),
        (0.9997, 0.9944),
        (1.0000, 1.0000),
    ];

    #[test]
    fn calibrated_cold_guard_never_prunes_a_row_the_full_scan_accepts() {
        let k = &KNOTS_27;
        let (n_cold, n_active, d) = (1200usize, 600usize, 32usize);
        let mut s = 0xC0FFEE_u64;
        let unit = |v: Vec<f32>| {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            v.into_iter().map(|x| x / n).collect::<Vec<f32>>()
        };
        let cold_rows: Vec<Vec<f32>> = (0..n_cold).map(|_| unit((0..d).map(|_| rng(&mut s)).collect())).collect();
        let active_rows: Vec<Vec<f32>> = (0..n_active).map(|_| unit((0..d).map(|_| rng(&mut s)).collect())).collect();
        let cold_ids: Vec<u32> = (0..n_cold as u32).map(|i| 10_000 + i).collect();
        let active_ids: Vec<u32> = (0..n_active as u32).collect();
        let flat: Vec<f32> = cold_rows.iter().flatten().copied().collect();
        let guard = build_unit_cold_guard(&flat, &cold_ids, d, 24, 4);
        let (beta, delta) = (0.01f32, 0.005f32);
        let (mut rescues, mut mismatches, mut pruned_rows) = (0usize, 0usize, 0usize);
        for trial in 0..10_000 {
            let mut q: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            if trial % 2 == 0 {
                // aim near a cold row so calibrated cold scores reach the steep upper knots
                let t = &cold_rows[trial % n_cold];
                let mix = 0.05 + 0.6 * (trial % 7) as f32 / 7.0;
                q.iter_mut().zip(t).for_each(|(a, b)| *a = mix * *a + b);
            }
            let q = unit(q);
            let active_scores: Vec<f32> = active_rows.iter().map(|r| calibrate_knots(dot(r, &q), k)).collect();
            let cold_cos = |id: u32| dot(&cold_rows[(id - 10_000) as usize], &q);
            let (id, score, rescued, st) =
                guard.biased_cosine_argmax_calibrated(&active_scores, &active_ids, &q, beta, delta, k, cold_cos);
            pruned_rows += n_cold - st.rows_refined;
            // full scan with the same calibrated rule
            let (mut best_id, mut best_active) = (u32::MAX, f32::NEG_INFINITY);
            for (&sc, &aid) in active_scores.iter().zip(&active_ids) {
                if sc > best_active || (sc == best_active && aid < best_id) {
                    best_active = sc;
                    best_id = aid;
                }
            }
            let gate = best_active + beta;
            let mut cand: Option<(f32, u32, f32)> = None;
            for &cid in &cold_ids {
                let raw = cold_cos(cid);
                let eff = calibrate_knots(raw - delta, k);
                if eff > gate && cand.is_none_or(|(e, i, _)| eff > e || (eff == e && cid < i)) {
                    cand = Some((eff, cid, raw));
                }
            }
            let want = match cand {
                Some((_, cid, raw)) => (cid, calibrate_knots(raw, k), true),
                None => (best_id, best_active, false),
            };
            if (id, score.to_bits(), rescued) != (want.0, want.1.to_bits(), want.2) {
                mismatches += 1;
            }
            rescues += want.2 as usize;
        }
        eprintln!(
            "calibrated cold guard: 10000 queries, mismatches {mismatches}, rescues {rescues}, cold rows pruned {:.1}%",
            100.0 * pruned_rows as f64 / (10_000 * n_cold) as f64
        );
        assert_eq!(mismatches, 0, "false pruning or false rescue in 10,000 queries");
        assert!(rescues >= 1_000 && 10_000 - rescues >= 1_000, "both outcomes are exercised: rescues={rescues}");
        assert!(pruned_rows > 0, "the raw-space bound still prunes cold rows");
    }
}
