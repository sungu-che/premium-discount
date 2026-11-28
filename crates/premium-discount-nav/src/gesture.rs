//! 제스처 감지: 클릭 · 터치(탭) · 스와이프 · 스크롤
//!
//! 브라우저 확장(pointer · scroll 이벤트)과 앱(Windows 포인터 입력)이 같은 원시 표본을 넘기고, 여기서 한 규칙으로 가른다.
//! 좌표 · 시각은 기기를 떠나지 않는다. 밖으로 나가는 것은 제스처 종류([`Gesture`])뿐이고, 타임트라이얼 체크포인트에
//! 붙는다(`element@swipe-left`, `page@scroll-down`). 마찰 신호로도 쓴다: 짧은 시간에 같은 곳을 여러 번 누르면
//! [`crate::Signal::RageClick`], 스크롤 방향을 자꾸 뒤집으면 [`crate::Signal::Rescrolled`].
//!
//! 기준값(누름 허용 움직임 10 px, 스와이프 50 px · 500 ms 안, 스크롤 묶음 300 ms · 100 px)은 흔히 쓰는 값에서 시작한 초깃값이고
//! 실측으로 고친다.

use premium_discount_sds::Moments;
use serde::{Deserialize, Serialize};

/// 체크포인트에 붙는 제스처. 누름(클릭 · 탭)은 기본 동작이라 체크포인트에는 붙이지 않는다(붙어 오면 지운다)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gesture {
    Press,
    SwipeLeft,
    SwipeRight,
    SwipeUp,
    SwipeDown,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

impl Gesture {
    pub fn name(self) -> &'static str {
        match self {
            Gesture::Press => "press",
            Gesture::SwipeLeft => "swipe-left",
            Gesture::SwipeRight => "swipe-right",
            Gesture::SwipeUp => "swipe-up",
            Gesture::SwipeDown => "swipe-down",
            Gesture::ScrollUp => "scroll-up",
            Gesture::ScrollDown => "scroll-down",
            Gesture::ScrollLeft => "scroll-left",
            Gesture::ScrollRight => "scroll-right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerKind {
    Mouse,
    Touch,
    Pen,
}

/// 포인터 표본 하나 (화면 좌표 px, 아래 · 오른쪽이 +)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointerSample {
    pub t_ms: i64,
    pub x: f32,
    pub y: f32,
}

/// 누름부터 뗌까지의 한 획
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub pointer: PointerKind,
    pub samples: Vec<PointerSample>,
}

/// 스크롤 위치의 변화 하나 (px, 아래 · 오른쪽으로 내려가면 +)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScrollSample {
    pub t_ms: i64,
    pub dx: f32,
    pub dy: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GestureParams {
    /// 누름으로 보는 최대 움직임 (터치 슬롭)
    pub press_slop_px: f32,
    /// 누름으로 보는 최대 시간 (더 길면 길게 누름: 제스처로 보지 않는다)
    pub press_max_ms: i64,
    /// 스와이프로 보는 최소 거리와 최대 시간 (터치 · 펜만. 마우스로 끄는 것은 드래그)
    pub swipe_min_px: f32,
    pub swipe_max_ms: i64,
    /// 이만큼 쉬면 스크롤 묶음을 끊는다
    pub scroll_idle_ms: i64,
    /// 스크롤 묶음이 이만큼 움직여야 체크포인트가 된다
    pub scroll_min_px: f32,
    /// rage: `rage_window_ms` 안에 반경 `rage_radius_px` 안에서 `rage_presses` 번 이상 누름
    pub rage_presses: usize,
    pub rage_window_ms: i64,
    pub rage_radius_px: f32,
}

impl Default for GestureParams {
    fn default() -> Self {
        GestureParams {
            press_slop_px: 10.0,
            press_max_ms: 500,
            swipe_min_px: 50.0,
            swipe_max_ms: 500,
            scroll_idle_ms: 300,
            scroll_min_px: 100.0,
            rage_presses: 3,
            rage_window_ms: 700,
            rage_radius_px: 24.0,
        }
    }
}

/// 한 획을 가른다: 거의 움직이지 않고 짧으면 누름(클릭 · 탭), 터치 · 펜으로 빠르게 멀리 밀면 스와이프(주된 축의 방향),
/// 그 밖(드래그 · 길게 누름 · 느린 끌기)은 None
pub fn classify_stroke(stroke: &Stroke, p: &GestureParams) -> Option<Gesture> {
    let first = stroke.samples.first()?;
    let last = stroke.samples.last()?;
    let duration = last.t_ms - first.t_ms;
    let wander = stroke.samples.iter().map(|s| (s.x - first.x).hypot(s.y - first.y)).fold(0.0f32, f32::max);
    if wander <= p.press_slop_px && duration <= p.press_max_ms {
        return Some(Gesture::Press);
    }
    let (dx, dy) = (last.x - first.x, last.y - first.y);
    if stroke.pointer != PointerKind::Mouse && dx.hypot(dy) >= p.swipe_min_px && duration <= p.swipe_max_ms {
        return Some(if dx.abs() >= dy.abs() {
            if dx < 0.0 {
                Gesture::SwipeLeft
            } else {
                Gesture::SwipeRight
            }
        } else if dy < 0.0 {
            Gesture::SwipeUp
        } else {
            Gesture::SwipeDown
        });
    }
    None
}

/// 쉬지 않고 이어진 스크롤 한 묶음
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollBurst {
    pub start_ms: i64,
    pub end_ms: i64,
    /// 주된 축으로 `scroll_min_px` 이상 움직였으면 그 방향
    pub gesture: Option<Gesture>,
    /// 방향을 뒤집은 수: 묶음 안에서 주된 축의 부호가 바뀐 수 + 같은 축에서 앞 묶음과 방향이 반대면 1
    pub reversals: u32,
}

fn sign(v: f32) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// 시간순 스크롤 표본을 묶음으로 자르고, 묶음마다 방향과 뒤집은 수를 센다
pub fn scroll_bursts(samples: &[ScrollSample], p: &GestureParams) -> Vec<ScrollBurst> {
    let mut groups: Vec<Vec<ScrollSample>> = Vec::new();
    for s in samples {
        match groups.last_mut() {
            Some(g) if s.t_ms - g.last().map_or(s.t_ms, |l| l.t_ms) <= p.scroll_idle_ms => g.push(*s),
            _ => groups.push(vec![*s]),
        }
    }
    let mut out: Vec<ScrollBurst> = Vec::new();
    // (세로 축인가, 방향 부호) — 앞 묶음의 방향
    let mut previous: Option<(bool, i8)> = None;
    for g in groups {
        let (net_x, net_y) = g.iter().fold((0.0f32, 0.0f32), |(x, y), s| (x + s.dx, y + s.dy));
        let (moved_x, moved_y) = g.iter().fold((0.0f32, 0.0f32), |(x, y), s| (x + s.dx.abs(), y + s.dy.abs()));
        let vertical = moved_y >= moved_x;
        let along = |s: &ScrollSample| if vertical { s.dy } else { s.dx };
        let mut reversals = 0u32;
        let mut last_sign = 0i8;
        for s in &g {
            let sg = sign(along(s));
            if sg != 0 {
                if last_sign != 0 && sg != last_sign {
                    reversals += 1;
                }
                last_sign = sg;
            }
        }
        let net = if vertical { net_y } else { net_x };
        let gesture = (net.abs() >= p.scroll_min_px).then(|| match (vertical, net > 0.0) {
            (true, true) => Gesture::ScrollDown,
            (true, false) => Gesture::ScrollUp,
            (false, true) => Gesture::ScrollRight,
            (false, false) => Gesture::ScrollLeft,
        });
        if gesture.is_some() {
            let dir = (vertical, sign(net));
            if previous.is_some_and(|(v, sg)| v == dir.0 && sg == -dir.1) {
                reversals += 1;
            }
            previous = Some(dir);
        }
        out.push(ScrollBurst {
            start_ms: g.first().map_or(0, |s| s.t_ms),
            end_ms: g.last().map_or(0, |s| s.t_ms),
            gesture,
            reversals,
        });
    }
    out
}

/// 시간순 누름(시각, x, y) 중에 rage 가 있는가: `rage_window_ms` 안에 서로 `rage_radius_px` 안에서 `rage_presses` 번 이상
pub fn rage_presses(presses: &[(i64, f32, f32)], p: &GestureParams) -> bool {
    presses.iter().enumerate().any(|(i, (t0, x0, y0))| {
        presses[i..]
            .iter()
            .take_while(|(t, _, _)| t - t0 <= p.rage_window_ms)
            .filter(|(_, x, y)| (x - x0).hypot(y - y0) <= p.rage_radius_px)
            .count()
            >= p.rage_presses
    })
}

/// 80 ms 미만의 초단기 경직 바운스(Spasticity bounce)는 비자발적 떨림으로 보고 광클 마찰에서 디바운스 면책한다 (Phase 7 Part 1).
pub const SPASTIC_BOUNCE_MS: i64 = 80;
/// 기본 `@hold` 판정 하한(ms). 운동 기저선이 느린 사용자는 [`KinematicProfile::hold_threshold_ms`] 로 동반 상향 시프트한다.
pub const BASE_HOLD_MS: i64 = 600;
/// 느린 클릭 해제 뒤 실제 길게 누름으로 분리하는 여유(ms).
pub const HOLD_SHIFT_MARGIN_MS: i64 = 450;

/// 기기 로컬 전용 개인 운동 기저선 (Phase 7 Part 1, 서버 전송 0바이트).
///
/// 손떨림·뇌병변·스위치 사용자의 성공 클릭 분포(포인터 이동 반경 `wander` · 누름 유지 시간 `duration`)를 Welford 로 누적해
/// 클릭 허용 반경과 시간을 탄성 확장하고, `@hold` 임계치도 동반 상향시켜 비장애인과 동일한 `Gesture::Press` (기본 체크포인트 해시)로 수렴시킨다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KinematicProfile {
    pub wander_px: Moments,
    pub duration_ms: Moments,
    /// 경직 바운스 디바운스 간격 (기본 [`SPASTIC_BOUNCE_MS`]).
    #[serde(default = "default_spastic_bounce_ms")]
    pub spastic_bounce_ms: i64,
    /// 지나친 이상치로 스와이프까지 삼키지 않도록 하는 최대 슬롭 상한(px).
    #[serde(default = "default_max_slop_px")]
    pub max_slop_px: f32,
    /// 최대 클릭 유지 시간 상한(ms).
    #[serde(default = "default_max_press_ms")]
    pub max_press_ms: i64,
    /// 의도하지 않은 같은 키 반복(키 바운스)의 간격 분포(ms). 5개 미만이면 [`KEY_BOUNCE_MS`] 띠를 쓴다.
    #[serde(default)]
    pub key_bounce_ms: Moments,
    /// 수식 키가 미끄러진 입력에서 지우기(Backspace)까지의 간격 분포(ms). 5개 미만이면 [`MODIFIER_SLIP_RECOVERY_MS`].
    #[serde(default)]
    pub modifier_slip_ms: Moments,
}

fn default_spastic_bounce_ms() -> i64 {
    SPASTIC_BOUNCE_MS
}
fn default_max_slop_px() -> f32 {
    60.0
}
fn default_max_press_ms() -> i64 {
    2_500
}

impl Default for KinematicProfile {
    fn default() -> Self {
        Self {
            wander_px: Moments::default(),
            duration_ms: Moments::default(),
            spastic_bounce_ms: SPASTIC_BOUNCE_MS,
            max_slop_px: default_max_slop_px(),
            max_press_ms: default_max_press_ms(),
            key_bounce_ms: Moments::default(),
            modifier_slip_ms: Moments::default(),
        }
    }
}

impl KinematicProfile {
    /// 성공한 일반 클릭 획의 이동 반경(px)과 누름 유지 시간(ms)을 기저선에 반영한다.
    pub fn observe_press_stroke(&mut self, stroke: &Stroke) {
        let (Some(first), Some(last)) = (stroke.samples.first(), stroke.samples.last()) else {
            return;
        };
        let duration = (last.t_ms - first.t_ms).max(0) as f64;
        let wander = stroke.samples.iter().map(|s| (s.x - first.x).hypot(s.y - first.y)).fold(0.0f32, f32::max) as f64;
        self.observe_press_sample(wander, duration);
    }

    pub fn observe_press_sample(&mut self, wander_px: f64, duration_ms: f64) {
        if wander_px.is_finite() && wander_px >= 0.0 && wander_px <= self.max_slop_px as f64 {
            self.wander_px.push(wander_px);
        }
        if duration_ms.is_finite() && duration_ms >= 0.0 && duration_ms <= self.max_press_ms as f64 {
            self.duration_ms.push(duration_ms);
        }
    }

    /// `slop_eff = max(base.press_slop_px, mu_wander + 2.5 * sd_wander)`,
    /// `press_max_eff = max(base.press_max_ms, 1.8 * mu_dur)` (상한으로 클램프).
    pub fn effective_params(&self, base: &GestureParams) -> GestureParams {
        let mut out = base.clone();
        if self.wander_px.n >= 3 {
            let eff_slop = (self.wander_px.mean + 2.5 * self.wander_px.sample_sd()) as f32;
            out.press_slop_px = eff_slop.max(base.press_slop_px).min(self.max_slop_px);
            out.swipe_min_px = out.swipe_min_px.max(out.press_slop_px + 15.0);
        }
        if self.duration_ms.n >= 3 {
            let eff_ms = (1.8 * self.duration_ms.mean).round() as i64;
            out.press_max_ms = eff_ms.max(base.press_max_ms).min(self.max_press_ms);
        }
        out
    }

    /// `@hold` 판정 임계치: `max(600ms, press_max_eff + 450ms)`.
    pub fn hold_threshold_ms(&self, base: &GestureParams) -> i64 {
        let eff = self.effective_params(base);
        BASE_HOLD_MS.max(eff.press_max_ms + HOLD_SHIFT_MARGIN_MS)
    }

    /// 키 바운스 간격 하나를 기저선에 넣는다 (0 ~ 400 ms 만)
    pub fn observe_key_bounce(&mut self, gap_ms: f64) {
        if gap_ms.is_finite() && (0.0..=400.0).contains(&gap_ms) {
            self.key_bounce_ms.push(gap_ms);
        }
    }

    /// 수식 키 미끄럼에서 지우기까지의 간격 하나를 기저선에 넣는다 (0 ~ 1,500 ms 만)
    pub fn observe_modifier_slip(&mut self, gap_ms: f64) {
        if gap_ms.is_finite() && (0.0..=1_500.0).contains(&gap_ms) {
            self.modifier_slip_ms.push(gap_ms);
        }
    }

    /// 키 바운스로 보는 간격 띠 `(최소, 최대)` ms: 기저선이 5개 이상이면 평균 ± 2σ(40 ~ 250 ms 안), 아니면 [`KEY_BOUNCE_MS`]
    pub fn key_bounce_band(&self) -> (i64, i64) {
        if self.key_bounce_ms.n < 5 {
            return (*KEY_BOUNCE_MS.start(), *KEY_BOUNCE_MS.end());
        }
        let (m, sd) = (self.key_bounce_ms.mean, self.key_bounce_ms.sample_sd());
        let lo = (m - 2.0 * sd).round().clamp(40.0, 250.0) as i64;
        let hi = (m + 2.0 * sd).round().clamp(40.0, 250.0) as i64;
        (lo.min(hi), hi.max(lo))
    }

    /// 수식 키 미끄럼 뒤 지우기를 복구로 보는 시간 (ms): 기저선이 5개 이상이면 평균 + 2σ(1,500 ms 안), 아니면 기본값
    pub fn modifier_slip_window_ms(&self) -> i64 {
        if self.modifier_slip_ms.n < 5 {
            return MODIFIER_SLIP_RECOVERY_MS;
        }
        (self.modifier_slip_ms.mean + 2.0 * self.modifier_slip_ms.sample_sd()).round().clamp(100.0, 1_500.0) as i64
    }
}

/// 의도하지 않은 같은 키 반복으로 보는 기본 간격 띠 (ms)
pub const KEY_BOUNCE_MS: std::ops::RangeInclusive<i64> = 80..=150;
/// 수식 키 미끄럼 뒤 지우기 한 번을 복구로 보는 기본 시간 (ms)
pub const MODIFIER_SLIP_RECOVERY_MS: i64 = 600;
/// 지우기 키의 이름
pub const BACKSPACE_KEY: &str = "Backspace";

/// 키 누름 하나 (값이 아니라 키 이름만: 글자 키는 한 글자 이름, 기능 키는 "Enter" · "Backspace" …)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyPress {
    pub t_ms: i64,
    pub key: String,
    /// 수식 키(Ctrl · Alt · Shift · Meta)를 함께 눌렀다
    pub modified: bool,
}

/// 키 누름 하나의 판정
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyVerdict {
    /// 의도한 입력
    Keep,
    /// 의도하지 않은 같은 키 반복: 다시 고침(Retried) 마찰로 세지 않는다 (sign 0)
    Bounce,
    /// 수식 키가 미끄러진 입력 바로 뒤의 지우기 한 번: 마찰로 세지 않는다 (sign 0)
    SlipRecovery,
}

/// 운동 떨림 사용자의 키보드 바운스와 수식 키 미끄럼 복구를 가린다 (누름마다 판정, 같은 길이).
///
/// - 같은 키를 같은 수식 상태로 바운스 띠([`KinematicProfile::key_bounce_band`]) 안의 간격으로 다시 눌렀으면 [`KeyVerdict::Bounce`].
/// - 수식 키와 함께 누른 글자 키 바로 뒤([`KinematicProfile::modifier_slip_window_ms`] 안)의 지우기 한 번(다음 누름이 또 지우기가
///   아님)은 [`KeyVerdict::SlipRecovery`].
pub fn filter_keyboard_bounces(presses: &[KeyPress], profile: &KinematicProfile) -> Vec<KeyVerdict> {
    let (lo, hi) = profile.key_bounce_band();
    let slip = profile.modifier_slip_window_ms();
    let printable = |k: &KeyPress| k.key.chars().count() == 1;
    (0..presses.len())
        .map(|i| {
            let cur = &presses[i];
            let Some(prev) = i.checked_sub(1).map(|j| &presses[j]) else { return KeyVerdict::Keep };
            let gap = cur.t_ms - prev.t_ms;
            if cur.key == prev.key && cur.modified == prev.modified && (lo..=hi).contains(&gap) {
                return KeyVerdict::Bounce;
            }
            let single = presses.get(i + 1).is_none_or(|next| next.key != BACKSPACE_KEY);
            if cur.key == BACKSPACE_KEY && prev.modified && printable(prev) && (0..=slip).contains(&gap) && single {
                return KeyVerdict::SlipRecovery;
            }
            KeyVerdict::Keep
        })
        .collect()
}

/// 개인 운동 기저선([`KinematicProfile`])을 적용한 획 분류. 손떨림·지연 해제가 있는 일반 클릭도 표준 [`Gesture::Press`] 로 수렴한다.
pub fn classify_stroke_adaptive(stroke: &Stroke, base: &GestureParams, profile: &KinematicProfile) -> Option<Gesture> {
    let eff = profile.effective_params(base);
    classify_stroke(stroke, &eff)
}

/// 근육 경직(spasticity)으로 인한 초단기 바운스 연타(`< min_bounce_ms`, 같은 반경 안)를 단일 클릭으로 디바운스한다.
pub fn filter_spastic_bounces(presses: &[(i64, f32, f32)], min_bounce_ms: i64, radius_px: f32) -> Vec<(i64, f32, f32)> {
    let mut out: Vec<(i64, f32, f32)> = Vec::with_capacity(presses.len());
    for &(t, x, y) in presses {
        let is_bounce = out
            .last()
            .is_some_and(|&(pt, px, py)| t - pt >= 0 && t - pt < min_bounce_ms && (x - px).hypot(y - py) <= radius_px);
        if !is_bounce {
            out.push((t, x, y));
        }
    }
    out
}

/// 경직 바운스를 면책한 뒤 광클(RageClick)을 판정한다.
pub fn rage_presses_adaptive(presses: &[(i64, f32, f32)], p: &GestureParams, profile: &KinematicProfile) -> bool {
    let cleaned = filter_spastic_bounces(presses, profile.spastic_bounce_ms, p.rage_radius_px);
    rage_presses(&cleaned, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(pointer: PointerKind, points: &[(i64, f32, f32)]) -> Stroke {
        Stroke {
            pointer,
            samples: points.iter().map(|(t_ms, x, y)| PointerSample { t_ms: *t_ms, x: *x, y: *y }).collect(),
        }
    }

    #[test]
    fn taps_clicks_and_swipes() {
        let p = GestureParams::default();
        let tap = stroke(PointerKind::Touch, &[(0, 100.0, 100.0), (60, 104.0, 103.0), (120, 103.0, 101.0)]);
        assert_eq!(classify_stroke(&tap, &p), Some(Gesture::Press));
        let click = stroke(PointerKind::Mouse, &[(0, 10.0, 10.0), (90, 10.0, 10.0)]);
        assert_eq!(classify_stroke(&click, &p), Some(Gesture::Press));
        let left = stroke(PointerKind::Touch, &[(0, 300.0, 200.0), (80, 220.0, 205.0), (160, 120.0, 210.0)]);
        assert_eq!(classify_stroke(&left, &p), Some(Gesture::SwipeLeft));
        let up = stroke(PointerKind::Pen, &[(0, 200.0, 400.0), (200, 210.0, 300.0)]);
        assert_eq!(classify_stroke(&up, &p), Some(Gesture::SwipeUp));
        let right = stroke(PointerKind::Touch, &[(0, 0.0, 0.0), (100, 80.0, 10.0)]);
        assert_eq!(classify_stroke(&right, &p), Some(Gesture::SwipeRight));
        let down = stroke(PointerKind::Touch, &[(0, 0.0, 0.0), (100, 5.0, 90.0)]);
        assert_eq!(classify_stroke(&down, &p), Some(Gesture::SwipeDown));
    }

    #[test]
    fn drags_long_presses_and_slow_moves_are_not_gestures() {
        let p = GestureParams::default();
        assert_eq!(classify_stroke(&stroke(PointerKind::Mouse, &[(0, 0.0, 0.0), (100, 200.0, 0.0)]), &p), None, "마우스로 끌기");
        assert_eq!(classify_stroke(&stroke(PointerKind::Touch, &[(0, 0.0, 0.0), (900, 2.0, 1.0)]), &p), None, "길게 누름");
        assert_eq!(classify_stroke(&stroke(PointerKind::Touch, &[(0, 0.0, 0.0), (1200, 300.0, 0.0)]), &p), None, "느린 끌기");
        assert_eq!(classify_stroke(&stroke(PointerKind::Touch, &[(0, 0.0, 0.0), (100, 30.0, 0.0)]), &p), None, "짧은 밀기");
        assert_eq!(classify_stroke(&stroke(PointerKind::Touch, &[]), &p), None);
    }

    #[test]
    fn scrolls_are_grouped_and_reversals_counted() {
        let p = GestureParams::default();
        let s = |t_ms: i64, dx: f32, dy: f32| ScrollSample { t_ms, dx, dy };
        let samples = [
            // 아래로 240 px (한 묶음)
            s(0, 0.0, 80.0),
            s(50, 0.0, 80.0),
            s(100, 0.0, 80.0),
            // 쉰 뒤 위로 150 px: 앞 묶음과 반대 → 뒤집음 1
            s(800, 0.0, -100.0),
            s(850, 0.0, -50.0),
            // 쉰 뒤 위아래로 헤맴: 묶음 안에서 3번 뒤집고, 순이동이 작아 방향 없음
            s(2000, 0.0, 30.0),
            s(2050, 0.0, -30.0),
            s(2100, 0.0, 30.0),
            s(2150, 0.0, -20.0),
            // 쉰 뒤 옆으로 120 px
            s(3000, 60.0, 5.0),
            s(3050, 60.0, 0.0),
        ];
        let bursts = scroll_bursts(&samples, &p);
        let summary: Vec<(Option<Gesture>, u32)> = bursts.iter().map(|b| (b.gesture, b.reversals)).collect();
        assert_eq!(
            summary,
            vec![(Some(Gesture::ScrollDown), 0), (Some(Gesture::ScrollUp), 1), (None, 3), (Some(Gesture::ScrollRight), 0)]
        );
        assert_eq!((bursts[0].start_ms, bursts[0].end_ms), (0, 100));
        assert_eq!(scroll_bursts(&[], &p), vec![]);
    }

    #[test]
    fn rage_needs_three_presses_close_in_time_and_space() {
        let p = GestureParams::default();
        assert!(rage_presses(&[(0, 10.0, 10.0), (200, 12.0, 11.0), (450, 9.0, 14.0)], &p));
        assert!(!rage_presses(&[(0, 10.0, 10.0), (200, 12.0, 11.0), (900, 9.0, 14.0)], &p), "창 밖");
        assert!(!rage_presses(&[(0, 10.0, 10.0), (200, 120.0, 11.0), (450, 9.0, 14.0)], &p), "다른 곳");
        assert!(!rage_presses(&[(0, 10.0, 10.0), (200, 12.0, 11.0)], &p));
    }

    #[test]
    fn gestures_serialize_as_kebab_case_names() {
        for g in [Gesture::Press, Gesture::SwipeLeft, Gesture::ScrollDown, Gesture::ScrollRight] {
            assert_eq!(serde_json::to_value(g).unwrap(), g.name());
        }
    }

    #[test]
    fn ac01_tremor_and_slow_release_converge_to_press_and_spastic_bounce_is_exempt() {
        let base = GestureParams::default();
        let mut profile = KinematicProfile::default();
        // Train local kinematic profile with a motor-impaired user's normal clicks (wander ~22-30px, dur ~650-800ms).
        for &(w, d) in &[(22.0, 680.0), (26.0, 740.0), (24.0, 720.0), (29.0, 790.0), (25.0, 710.0)] {
            profile.observe_press_sample(w, d);
        }
        // Raw defaults reject wander=28px, dur=820ms.
        let tremor_click = stroke(PointerKind::Mouse, &[(0, 100.0, 100.0), (400, 128.0, 100.0), (820, 118.0, 106.0)]);
        assert_eq!(classify_stroke(&tremor_click, &base), None);
        // Adaptive kinematic profile converges it to standard Gesture::Press (no @hold split!).
        assert_eq!(classify_stroke_adaptive(&tremor_click, &base, &profile), Some(Gesture::Press));
        assert!(profile.hold_threshold_ms(&base) > 820, "hold threshold must shift above slow press");
        // Spastic triple bounce within 45ms intervals is debounced to 1 press (not RageClick).
        let spastic = [(0, 50.0, 50.0), (35, 52.0, 51.0), (70, 51.0, 52.0)];
        assert!(rage_presses(&spastic, &base));
        assert!(!rage_presses_adaptive(&spastic, &base, &profile));
        // Genuine intentional rage clicks spaced >= 180ms apart still trigger RageClick.
        let genuine_rage = [(0, 50.0, 50.0), (200, 52.0, 51.0), (420, 51.0, 52.0)];
        assert!(rage_presses_adaptive(&genuine_rage, &base, &profile));
    }

    fn key(t_ms: i64, key: &str, modified: bool) -> KeyPress {
        KeyPress { t_ms, key: key.into(), modified }
    }

    #[test]
    fn keyboard_bounces_and_a_single_slip_recovery_are_exempt() {
        let profile = KinematicProfile::default();
        let presses = [
            key(0, "Enter", false),
            key(110, "Enter", false),    // 떨림으로 한 번 더 눌림 (110 ms)
            key(900, "A", true),         // Shift 가 미끄러져 대문자
            key(1_200, BACKSPACE_KEY, false),
            key(1_500, "a", false),
            key(2_400, "Enter", false),
            key(2_700, "Enter", false),  // 300 ms: 의도한 두 번째 누름
            key(3_000, "b", true),
            key(3_200, BACKSPACE_KEY, false),
            key(3_350, BACKSPACE_KEY, false), // 연속 지우기는 복구 한 번이 아니다
        ];
        let v = filter_keyboard_bounces(&presses, &profile);
        use KeyVerdict::*;
        assert_eq!(v, vec![Keep, Bounce, Keep, SlipRecovery, Keep, Keep, Keep, Keep, Keep, Bounce]);
        // 기저선이 쌓이면 그 사람의 띠를 쓴다
        let mut learned = KinematicProfile::default();
        for g in [190.0, 200.0, 210.0, 205.0, 195.0] {
            learned.observe_key_bounce(g);
        }
        let (lo, hi) = learned.key_bounce_band();
        assert!(lo > 150 && hi < 250, "({lo}, {hi})");
        assert_eq!(filter_keyboard_bounces(&[key(0, "x", false), key(200, "x", false)], &learned)[1], Bounce);
        assert_eq!(filter_keyboard_bounces(&[key(0, "x", false), key(110, "x", false)], &learned)[1], Keep);
        // 옛 프로필 JSON 은 새 칸 없이 읽힌다
        let mut j = serde_json::to_value(KinematicProfile::default()).unwrap();
        j.as_object_mut().unwrap().remove("key_bounce_ms");
        j.as_object_mut().unwrap().remove("modifier_slip_ms");
        let back: KinematicProfile = serde_json::from_value(j).unwrap();
        assert_eq!(back.key_bounce_ms.n, 0);
    }
}
