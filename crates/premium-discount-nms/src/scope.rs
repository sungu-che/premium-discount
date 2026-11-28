//! 순위 가산(스코프 부스트)의 세기를 원점수에 맞춘다.
//!
//! 고정 가산(예: 지금 체크포인트 대상 +0.12)은 바닥을 겨우 넘은 애매한 후보도 같은 몫을 받아, 원점수가 분명히 높은
//! 바깥 후보를 뒤집는다. [`AdaptiveScopeBoost`] 는 원점수가 바닥에서 `ramp` 만큼 올라갈 때까지 가산을 비례로 줄이되
//! `min_frac` 아래로는 줄이지 않는다:
//!
//! `Δ(s) = full × clamp((s − floor) / ramp, min_frac, 1)`

/// 원점수 비례 가산
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdaptiveScopeBoost {
    /// 다 받았을 때의 가산
    pub full: f32,
    /// 후보가 살아남는 원점수 바닥 (그 아래 후보는 부르는 쪽이 버린다)
    pub floor: f32,
    /// 바닥에서 이만큼 올라가면 가산을 다 받는다
    pub ramp: f32,
    /// 바닥 바로 위에서도 받는 몫 (0 ~ 1)
    pub min_frac: f32,
}

impl AdaptiveScopeBoost {
    /// 지금 체크포인트 대상(L1)의 가산: 다 받으면 +0.12, 바닥 0.42, 0.54 부터 다 받음, 바닥 위 최소 +0.03
    pub const CHECKPOINT_L1: AdaptiveScopeBoost = AdaptiveScopeBoost {
        full: 0.12,
        floor: 0.42,
        ramp: 0.12,
        min_frac: 0.25,
    };

    /// 원점수 `score` 의 가산. 유한하지 않은 점수와 바닥 아래 점수는 0
    pub fn boost(&self, score: f32) -> f32 {
        if !score.is_finite() || score < self.floor {
            return 0.0;
        }
        let ramp = if self.ramp > 0.0 { self.ramp } else { f32::MIN_POSITIVE };
        self.full * ((score - self.floor) / ramp).clamp(self.min_frac.clamp(0.0, 1.0), 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weak_in_scope_match_no_longer_overtakes_a_clearly_stronger_one() {
        let l1 = AdaptiveScopeBoost::CHECKPOINT_L1;
        // 바닥 바로 위 (0.43): 최소 몫 0.25 × 0.12 = 0.03 → 0.46, 바깥 핵심 경고 0.53 아래에 머문다
        assert!((l1.boost(0.43) - 0.03).abs() < 1e-6);
        assert!(0.43 + l1.boost(0.43) < 0.53);
        // 고정 가산이었다면 0.43 + 0.12 = 0.55 > 0.53 으로 뒤집혔다
        assert!(0.43 + l1.full > 0.53);
        // 바닥에서 ramp 만큼 오르면 다 받는다 (예전 시험의 0.61 → +0.12 는 그대로)
        assert!((l1.boost(0.54) - 0.12).abs() < 1e-6);
        assert!((l1.boost(0.61) - 0.12).abs() < 1e-6);
        assert!((l1.boost(0.48) - 0.06).abs() < 1e-6, "{}", l1.boost(0.48));
        // 바닥 아래와 유한하지 않은 점수는 가산 없음, 가산은 점수와 함께 늘기만 한다
        assert_eq!(l1.boost(0.40), 0.0);
        assert_eq!(l1.boost(f32::NAN), 0.0);
        let mut prev = 0.0;
        for i in 0..=100 {
            let b = l1.boost(0.42 + i as f32 * 0.005);
            assert!(b >= prev);
            prev = b;
        }
    }
}
