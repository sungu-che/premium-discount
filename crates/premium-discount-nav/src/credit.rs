//! 런 하나의 가감점(+ / − / 0)과 단계 귀책. PD-Verdict 관측의 `sign` · `credit` 만 정하고, 판정 수식 · 계약은 그대로다.
//!
//! - **무마찰 완주(CleanPass)**: 마찰 폭발 · 재시도 · 접힌 취소 루프 없이 끝냈지만 지름길 띠(z ≤ −band)에는 못 미친 런도
//!   +1 을 낸다(몫 0.65, 고스트를 따랐으면 0.35). 예전에는 관측이 없어 늘 쓰는 무마찰 코스가 끝내 Unrated 로 남았다.
//! - **지름길**: 띠를 넘으면 +1 (스스로 찾았으면 1.0, 고스트를 따랐으면 0.35 — 확증 편향을 줄인다).
//! - **단계 귀책**: 병목(z ≥ 1), 접힌 취소 루프(그 단계에서 모달을 열었다가 아무것도 하지 않고 닫아 같은 단계로 돌아옴),
//!   반복 무반응 클릭은 그 단계 비트에만 찍는다([`step_fault_mask`], `route_marks_v2` · `shortcut_wins_v2` 의 `fault_steps_mask`).
//! - **청취 시간**: 스크린리더가 읽어 준 시간(`READING_MS_PER_CHAR` × 글자)을 빼고 완주 시간을 잰다([`listening_adjusted_ms`]).
//! - **입력 표면**: 비서에게 쓰는 중일 때만 먼저 말을 걸지 않는다([`InputSurfaceRole::holds_proactive`]). 자기 문서 본문을
//!   쓰는 중이나 스크린리더로 둘러보는 중에도 막으면 일하는 동안 코칭이 굶는다.
//! - **수락 창**: 스크린리더가 켜져 있으면 제안을 받아들인 것으로 보는 시간을 35초로 늘린다([`accept_window_ms`]).

use premium_discount_verdict::credits;

use crate::behavior::reading_grace_ms;

/// 제안한 번호를 이 안에 고르면 제안을 받아들인 것이다 (ms)
pub const ACCEPT_WINDOW_MS: u64 = 15_000;
/// 스크린리더가 켜져 있을 때의 수락 창: 안내를 끝까지 듣고 고를 시간 (ms)
pub const SCREEN_READER_ACCEPT_WINDOW_MS: u64 = 35_000;
/// 병목으로 보는 단계 마찰 z
pub const BOTTLENECK_Z: f64 = 1.0;
/// 무반응 클릭이 이 수 이상이면 그 단계의 잘못이다
pub const NOOP_CLICK_FAULT: u32 = 2;

/// 지금 쓰는 수락 창
pub fn accept_window_ms(screen_reader_active: bool) -> u64 {
    if screen_reader_active {
        SCREEN_READER_ACCEPT_WINDOW_MS
    } else {
        ACCEPT_WINDOW_MS
    }
}

/// 입력이 일어나는 곳
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSurfaceRole {
    /// 비서(코치)에게 보내는 글을 쓰는 중
    ChatPrompt,
    /// 자기 일(문서 · 코드 · 셀)의 본문을 쓰는 중
    WorkBuffer,
    /// 스크린리더로 화면을 둘러보는 키 입력
    ScreenReaderNav,
}

impl InputSurfaceRole {
    /// 이 입력이 이어지는 동안 먼저 말을 걸지 않는가
    pub fn holds_proactive(self) -> bool {
        self == InputSurfaceRole::ChatPrompt
    }
}

/// 스크린리더가 읽어 준 시간을 뺀 경과 (ms, 0 아래로 가지 않는다)
pub fn listening_adjusted_ms(elapsed_ms: u64, spoken_chars: usize) -> u64 {
    elapsed_ms.saturating_sub(u64::from(reading_grace_ms(spoken_chars)))
}

/// 런 한 단계의 결과
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StepOutcome {
    /// 그 단계의 마찰 z (기준선이 모자라면 None)
    pub friction_z: Option<f64>,
    /// 그 단계에서 모달을 열었다가 아무것도 하지 않고 닫아 같은 단계로 돌아왔다 (접힌 취소 루프)
    pub folded_cancel_loop: bool,
    /// 화면이 바뀌지 않은 누름 수
    pub noop_clicks: u32,
}

impl StepOutcome {
    /// 이 단계에 잘못이 있는가
    pub fn faulted(&self) -> bool {
        self.friction_z.is_some_and(|z| z.is_finite() && z >= BOTTLENECK_Z) || self.folded_cancel_loop || self.noop_clicks >= NOOP_CLICK_FAULT
    }
}

/// 16비트 단계 귀책 마스크: 잘못이 있는 단계의 비트만 켠다 (17번째 단계부터는 담지 않는다). `warned_trap_mask` 는 이미 경고한 함정
/// 단계로 그대로 더하고, 자책 패배(`shielded`)는 모든 단계를 기권으로 둔다(`u16::MAX`)
pub fn step_fault_mask(steps: &[StepOutcome], warned_trap_mask: u16, shielded: bool) -> u16 {
    if shielded {
        return u16::MAX;
    }
    steps
        .iter()
        .enumerate()
        .take(16)
        .filter(|(_, s)| s.faulted())
        .fold(warned_trap_mask, |mask, (i, _)| mask | (1u16 << i))
}

/// 런의 가감점
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunCredit {
    /// 지름길 승리 (완주 z ≤ −band)
    Shortcut { credit: f64 },
    /// 무마찰 완주 (지름길 띠에는 못 미침)
    CleanPass { credit: f64 },
    /// 잘못이 있는 단계가 있다: 런 전체가 아니라 그 단계들에 귀책한다
    Faulted { mask: u16 },
    /// 기권 (자책 패배 등)
    Abstain,
}

impl RunCredit {
    /// PD-Verdict 관측의 `(sign, credit)`. 잘못이 있는 런 · 기권은 None (단계의 discount 는 단계 귀책 규칙이 낸다)
    pub fn obs(self) -> Option<(i32, f64)> {
        match self {
            RunCredit::Shortcut { credit } | RunCredit::CleanPass { credit } => Some((1, credit)),
            RunCredit::Faulted { .. } | RunCredit::Abstain => None,
        }
    }
}

/// 런 하나를 판정한다. `run_z` 는 청취 시간을 뺀 완주 시간의 z (공개 코스 기준, 낮을수록 빠름), `band` 는 지름길 띠 (보통 2.0)
pub fn run_credit(steps: &[StepOutcome], run_z: Option<f64>, band: f64, ghost_followed: bool, shielded: bool) -> RunCredit {
    if shielded {
        return RunCredit::Abstain;
    }
    let mask = step_fault_mask(steps, 0, false);
    if mask != 0 {
        return RunCredit::Faulted { mask };
    }
    match run_z {
        Some(z) if z.is_finite() && z <= -band => RunCredit::Shortcut { credit: credits::shortcut(ghost_followed) },
        _ => RunCredit::CleanPass { credit: credits::clean_pass(ghost_followed) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(n: usize) -> Vec<StepOutcome> {
        vec![StepOutcome { friction_z: Some(-0.2), ..StepOutcome::default() }; n]
    }

    #[test]
    fn a_frictionless_finish_earns_a_clean_pass_and_a_fast_one_a_shortcut() {
        assert_eq!(run_credit(&clean(4), Some(-0.5), 2.0, false, false), RunCredit::CleanPass { credit: 0.65 });
        assert_eq!(run_credit(&clean(4), None, 2.0, true, false), RunCredit::CleanPass { credit: 0.35 }, "followed the ghost");
        assert_eq!(run_credit(&clean(4), Some(-2.4), 2.0, false, false), RunCredit::Shortcut { credit: 1.0 });
        assert_eq!(run_credit(&clean(4), Some(-2.4), 2.0, true, false), RunCredit::Shortcut { credit: 0.35 });
        assert_eq!(run_credit(&clean(4), Some(-2.4), 2.0, false, true), RunCredit::Abstain, "a shielded defeat");
        assert_eq!(RunCredit::CleanPass { credit: 0.65 }.obs(), Some((1, 0.65)));
        assert_eq!(RunCredit::Abstain.obs(), None);
    }

    #[test]
    fn folded_cancel_loops_and_noop_clicks_fault_only_their_own_step() {
        let mut steps = clean(5);
        steps[1].folded_cancel_loop = true; // M1 → 모달 → M1
        steps[3].noop_clicks = 2;
        steps[4].noop_clicks = 1; // 한 번은 봐준다
        let mask = step_fault_mask(&steps, 0, false);
        assert_eq!(mask, (1 << 1) | (1 << 3));
        assert_eq!(run_credit(&steps, Some(-3.0), 2.0, false, false), RunCredit::Faulted { mask });
        // 병목과 경고한 함정은 그대로, 자책 패배는 모든 단계 기권
        steps[0].friction_z = Some(1.4);
        assert_eq!(step_fault_mask(&steps, 1 << 9, false), (1 << 0) | (1 << 1) | (1 << 3) | (1 << 9));
        assert_eq!(step_fault_mask(&steps, 0, true), u16::MAX);
        // 17번째 단계부터는 담지 않는다
        let mut long = clean(20);
        long[18].folded_cancel_loop = true;
        assert_eq!(step_fault_mask(&long, 0, false), 0);
    }

    #[test]
    fn listening_time_and_input_surfaces() {
        // 30글자를 읽어 주는 3.6초는 완주 시간에서 뺀다
        assert_eq!(listening_adjusted_ms(10_000, 30), 6_400);
        assert_eq!(listening_adjusted_ms(1_000, 30), 0);
        assert!(InputSurfaceRole::ChatPrompt.holds_proactive());
        assert!(!InputSurfaceRole::WorkBuffer.holds_proactive(), "typing in one's own document does not starve coaching");
        assert!(!InputSurfaceRole::ScreenReaderNav.holds_proactive());
        assert_eq!(accept_window_ms(false), 15_000);
        assert_eq!(accept_window_ms(true), 35_000);
    }
}
