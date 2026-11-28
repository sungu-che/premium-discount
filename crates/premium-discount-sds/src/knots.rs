//! Monotone piecewise-linear score calibration `knots = [(raw_x, calibrated_y)]` (e.g. a quantile map from one
//! cosine space onto another) and its inverse, so a gate set on the calibrated scale can be applied to raw scores.
//!
//! Pure arithmetic, `wasm32-unknown-unknown` clean. Tables must be strictly increasing on both axes.

/// `f(x)`: the calibrated score of a raw score `x`, linear between knots (`y0 + (y1 − y0)(x − x0)/(x1 − x0)`).
/// Outside the table: its end values; a non-finite input or an empty table returns `x` unchanged.
pub fn calibrate_knots(x: f32, knots: &[(f32, f32)]) -> f32 {
    if !x.is_finite() || knots.is_empty() {
        return x;
    }
    if x <= knots[0].0 {
        return knots[0].1;
    }
    for w in knots.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    knots[knots.len() - 1].1
}

/// `f⁻¹(y_gate)`: the raw score whose calibrated score is `y_gate`. Below or above the table: its end raw values;
/// a non-finite gate or an empty table returns `y_gate` unchanged.
#[inline]
pub fn uncalibrate_knots(y_gate: f32, knots: &[(f32, f32)]) -> f32 {
    if !y_gate.is_finite() || knots.is_empty() {
        return y_gate;
    }
    if y_gate <= knots[0].1 {
        return knots[0].0;
    }
    for w in knots.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if y_gate <= y1 {
            let dy = (y1 - y0).max(1e-12);
            return x0 + (x1 - x0) * (y_gate - y0) / dy;
        }
    }
    knots[knots.len() - 1].0
}

/// A raw gate at or below every raw score whose calibrated score can exceed `y_gate`: [`uncalibrate_knots`]
/// lowered until [`calibrate_knots`] no longer lands above the gate (f32 rounding in either map). Pruning raw
/// upper bounds against this value never drops a score the calibrated rule `calibrate_knots(s) > y_gate` accepts.
pub fn uncalibrate_knots_floor(y_gate: f32, knots: &[(f32, f32)]) -> f32 {
    let x = uncalibrate_knots(y_gate, knots);
    if !x.is_finite() || knots.is_empty() {
        return x;
    }
    let mut slack = 1e-6 * (1.0 + x.abs());
    let mut floor = x - slack;
    for _ in 0..32 {
        if calibrate_knots(floor, knots) <= y_gate {
            break;
        }
        slack *= 2.0;
        floor = x - slack;
    }
    floor
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strictly increasing 27-knot quantile table (raw cosine → another cosine scale).
    pub(crate) const KNOTS_27: [(f32, f32); 27] = [
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

    fn rng(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        ((*seed >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    }

    #[test]
    fn the_inverse_maps_knots_back_and_clamps_the_ends() {
        let k = &KNOTS_27;
        assert!(k.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 > w[0].1));
        for &(x, y) in k {
            assert!((calibrate_knots(x, k) - y).abs() < 1e-6, "{x} -> {}", calibrate_knots(x, k));
            assert!((uncalibrate_knots(y, k) - x).abs() < 1e-6, "{y} -> {}", uncalibrate_knots(y, k));
        }
        assert_eq!(uncalibrate_knots(-3.0, k), -1.0);
        assert_eq!(uncalibrate_knots(2.0, k), 1.0);
        assert!(uncalibrate_knots(f32::NAN, k).is_nan());
        assert!(calibrate_knots(f32::NAN, k).is_nan());
        assert_eq!(uncalibrate_knots(0.4, &[]), 0.4);
        assert_eq!(calibrate_knots(0.4, &[]), 0.4);
        assert!((calibrate_knots(0.7527, k) - 0.4921).abs() < 1e-4);
        assert!((uncalibrate_knots(0.4921, k) - 0.7527).abs() < 1e-4);
    }

    #[test]
    fn round_trips_hold_and_the_floor_never_admits_a_passing_raw_score() {
        let k = &KNOTS_27;
        let mut s = 0x5EED_0B57u64;
        for i in 0..20_000 {
            let x = -1.0 + 2.0 * (i as f32 / 20_000.0);
            assert!((uncalibrate_knots(calibrate_knots(x, k), k) - x).abs() < 2e-5, "x={x}");
            let y = rng(&mut s);
            let floor = uncalibrate_knots_floor(y, k);
            assert!(floor <= uncalibrate_knots(y, k));
            assert!(calibrate_knots(floor, k) <= y, "y={y} floor={floor}");
            let probe = uncalibrate_knots(y, k) + 1e-7 * rng(&mut s);
            if calibrate_knots(probe, k) > y {
                assert!(probe > floor, "y={y} probe={probe} floor={floor}");
            }
        }
    }
}
