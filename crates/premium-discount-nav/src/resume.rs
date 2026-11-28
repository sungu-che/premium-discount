//! 개인정보 잠금과 안전 복귀: 고정 60초 블라인드 대신, 잠금 사유와 창 · 페이지 서명으로 풀 때를 정하는 상태 머신
//! ([`PrivacyStateMachine`]). 픽셀을 보지 않고 기기 안에서만 돈다.
//!
//! - `Locked`: 픽셀 캡처 · 기록을 하지 않는다. 500 ms 마다 픽셀 없는 확인([`ZeroPixelProbe`]: 캡처하는 모니터에서 가려지지
//!   않은 창의 OS · UIA · 주소 신호)을 받아, 민감 신호가 남아 있으면 계속 잠근다. 비전 · 화면 글로만 안 사유(계좌 사본 그림,
//!   시드 구문)는 이 확인으로 다시 볼 수 없으므로, 잠근 표면(창 + 제목 · 주소 틀)이 그대로인 동안에는 신호가 없어도 풀지
//!   않는다(3초 만에 풀고 다시 찍고 다시 잠그는 고리를 막는다). 은행 창을 오래 띄워 두면 그동안 내내 잠겨 있다.
//! - `GraceProbe`: 신호가 사라져도 [`GRACE_MS`] 동안 깨끗한 확인이 [`GRACE_TICKS`] 번 이상 이어져야 푼다(아이디 · 비밀번호 →
//!   로딩 → 2단계 인증 사이의 빈틈). 그 사이 민감 신호가 다시 보이면 처음 잠근 시각을 그대로 두고 다시 잠근다.
//!   3초 걸린 로그인은 신호가 사라지고 3초 뒤, 모두 6초쯤에 돌아온다.
//! - `ReceiptGuard`: 결제 문맥에서 잠긴 뒤 주문 완료 · 영수증 페이지에 닿으면, 시간 제한 없이 그 페이지를 떠날 때까지 막는다.
//!   통과하는 것은 주소 틀로 만든 페이지 해시 하나(타임트라이얼 결승 체크포인트)뿐이고, 결제 흐름마다 한 번만 낸다.
//!   [`POST_PAYMENT_ARM_MS`] 는 보호를 끄는 시간이 아니라, 결제 잠금이 풀린 뒤 영수증 페이지를 기다리는 시간이다.
//! - `Normal`: 다시 캡처 · 기록한다. 돌아온 첫 프레임은 센서를 차분 없이 다시 채운다(`trigger_sensor_reprime`).
//!
//! 캡처 · 기록을 하지 않은 구간(잠금 · 유예 · 영수증 보호)은 동결 구간 `(시작, 끝)` ms 다([`PrivacyStateMachine::active_frozen_spans`]).
//! 타임트라이얼 구간 시간 · 에피소드 간격 · 탐색 시간에서 겹친 만큼 뺀다([`frozen_overlap_ms`]). 벽시계 시각 자체는 고치지 않는다.
//! 잠금 전 체크포인트는 같은 `cc` 로 [`BRIDGE_TTL_MS`] 안에 돌아와야 이어 붙인다 (아니면 `bridge_broken`).

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::OnceLock;

use crate::hash::{page_hash, url_template};
use crate::portfolio::PaceScale;
use crate::privacy::PrivacyTrigger;

/// 유예: 민감 신호가 사라진 뒤 이만큼 깨끗해야 푼다
pub const GRACE_MS: i64 = 3_000;
/// 유예: 깨끗한 확인이 이만큼 이어져야 푼다 (500 ms 마다 확인하면 시간 조건이 먼저 찬다)
pub const GRACE_TICKS: u8 = 3;
/// 결제 잠금이 풀린 뒤 영수증 페이지를 기다리는 시간
pub const POST_PAYMENT_ARM_MS: i64 = 15_000;
/// 잠금 전 체크포인트를 이어 붙일 수 있는 시간 (잠근 때부터)
pub const BRIDGE_TTL_MS: i64 = 900_000;
/// 기억하는 동결 구간 수 상한 (오래된 것부터 버린다)
const MAX_FROZEN_SPANS: usize = 64;

/// 주문 완료 · 영수증 화면의 주소 틀 · 제목 조각 (소문자, 공백 · `-` · `_` 를 뺀 글에서 찾는다)
const RECEIPT_MARKS: [&str; 15] = [
    "order/complete",
    "order/done",
    "order/confirm",
    "orders/complete",
    "checkout/success",
    "checkout/complete",
    "checkout/thankyou",
    "payment/result",
    "payment/complete",
    "ordercomplete",
    "orderconfirm",
    "주문완료",
    "결제완료",
    "구매완료",
    "주문이완료",
];
/// 결제 · 주문서 진행 화면의 조각
const PAYMENT_MARKS: [&str; 8] = ["checkout", "/pay", "payment", "billing", "ordersheet", "order/sheet", "결제", "주문서"];

/// 픽셀 없이 창 · 페이지가 같은지 가리는 서명. 기기 메모리에만 두고 기록하지 않는다
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SurfaceSignature {
    pub hwnd_id: isize,
    /// 창 제목 + 주소 틀의 해시 (비전으로만 잠갔을 때 표면이 바뀌었는지 본다)
    pub route_or_title_sig: u64,
}

impl SurfaceSignature {
    /// 창 핸들과 제목 · 주소 틀로 서명을 만든다. 해시 열쇠는 프로세스마다 새로 뽑아, 다른 실행의 서명과 이어지지 않는다
    pub fn new(hwnd_id: isize, route_or_title: &str) -> SurfaceSignature {
        static KEYS: OnceLock<RandomState> = OnceLock::new();
        SurfaceSignature {
            hwnd_id,
            route_or_title_sig: KEYS.get_or_init(RandomState::new).hash_one(route_or_title.trim()),
        }
    }
}

/// 픽셀 없는 확인 한 번 (잠금 · 유예 · 영수증 보호 중에는 500 ms 마다, 평소에는 캡처하기 전에)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeroPixelProbe {
    pub now_ms: i64,
    /// 앞에 있는 앱 · 사이트 (`cc`)
    pub active_cc: String,
    /// 앞에 있는 표면 (창 + 제목 · 주소 틀)
    pub surface: SurfaceSignature,
    /// 캡처하는 모니터에서 가려지지 않은 창의 민감 신호 ([`crate::evaluate_privacy`] 의 사유). 비전이 지금 프레임에서 잡은
    /// 사유도 여기로 넘긴다. 없으면 None
    pub unoccluded_trigger: Option<PrivacyTrigger>,
    /// 결제 · 주문서 진행 화면이다 ([`payment_route`])
    pub in_payment_flow: bool,
    /// 주문 완료 · 영수증 화면이면 그 페이지 해시 ([`receipt_finish_page`])
    pub receipt_finish_page_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyPhase {
    /// 캡처 · 기록한다
    Normal,
    Locked {
        since_ms: i64,
        trigger: PrivacyTrigger,
        locked_surface: SurfaceSignature,
        had_payment_context: bool,
    },
    /// 신호가 사라져 풀기를 기다린다 (여전히 캡처 · 기록하지 않는다)
    GraceProbe {
        locked_since_ms: i64,
        clear_since_ms: i64,
        consecutive_clean_ticks: u8,
        had_payment_context: bool,
        /// 잠근 사유와 표면: 픽셀로만 안 사유면 그 표면으로 돌아왔을 때 다시 잠근다
        trigger: PrivacyTrigger,
        locked_surface: SurfaceSignature,
    },
    /// 시간 제한 없이 영수증 표면을 떠날 때까지 픽셀 · 글 수집을 막는다
    ReceiptGuard {
        entered_ms: i64,
        receipt_surface: SurfaceSignature,
        receipt_page_hash: String,
        finish_checkpoint_emitted: bool,
    },
}

/// 확인 한 번의 결과
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumeStepOutcome {
    /// 픽셀 캡처(DXGI · 비전)와 기록(행동 · 지도 · 체크포인트 · 보관함)을 해도 되는가. Normal 에서만 true
    pub capture_and_record_allowed: bool,
    /// 이번 확인에서 캡처를 멈췄다(Normal → 잠금 · 영수증 보호): 지난 5초 사건과 센서 잔상을 지운다
    /// ([`crate::LiveEpisodeWindow::begin_privacy_lock`])
    pub privacy_epoch_started: bool,
    /// 이번 확인에서 Normal 로 돌아왔다: 첫 프레임은 센서를 차분 없이 다시 채운다 ([`crate::LiveEpisodeWindow::end_privacy_lock`])
    pub trigger_sensor_reprime: bool,
    /// 결제 흐름의 결승 페이지 해시 (영수증 보호에 들어갈 때 한 번만). 체크포인트 시각은 이 확인의 `now_ms` 다
    pub sanitized_finish_page: Option<String>,
    /// 잠금 전 체크포인트를 더는 이어 붙이지 않는다: 다른 앱 · 사이트에서 돌아왔거나 [`BRIDGE_TTL_MS`] 가 지났다
    pub bridge_broken: bool,
}

/// 잠금이 바뀔 때 들려주는 신호 (기록하지 않는 정적 효과음: 화면을 보지 않는 사용자가 잠김 · 풀림을 안다)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyTransitionCue {
    #[default]
    None,
    EnteredLockdown,
    ResumedNormal,
}

impl ResumeStepOutcome {
    /// 이번 확인의 신호: 캡처를 멈췄으면 잠김, 다시 시작했으면 풀림
    pub fn cue(&self) -> PrivacyTransitionCue {
        if self.privacy_epoch_started {
            PrivacyTransitionCue::EnteredLockdown
        } else if self.trigger_sensor_reprime {
            PrivacyTransitionCue::ResumedNormal
        } else {
            PrivacyTransitionCue::None
        }
    }
}

/// [`PrivacyStateMachine::step_with`] 이 [`ZeroPixelProbe`] 밖에서 더 보는 것. 기본값이면 [`PrivacyStateMachine::step`] 과 같다
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StepContext {
    /// 인증 · 결제 문맥 (로그인 2단계 · 결제 인증). 잠금이 결제 문맥이 아니어도 이것이면 인증 잠금으로 본다
    pub auth_context: bool,
    /// 앞에 있는 창이 인증 앱(OTP · 패스키)이다. 판정은 부르는 쪽이 한다
    pub authenticator_surface: bool,
    /// 기기 안 개인 속도 배율: 다리 유지 시간([`BRIDGE_TTL_MS`])을 이만큼 늘린다
    pub pace: Option<PaceScale>,
}

/// 개인정보 잠금 · 안전 복귀 상태 머신 (모듈 설명 참고)
#[derive(Debug, Clone)]
pub struct PrivacyStateMachine {
    pub phase: PrivacyPhase,
    pub grace_ms: i64,
    pub min_clean_ticks: u8,
    pub post_payment_arm_ms: i64,
    pub bridge_ttl_ms: i64,
    /// 결제 문맥의 잠금이 풀린 시각 (영수증을 기다리는 동안)
    last_payment_unlock_ms: Option<i64>,
    /// 이 결제 흐름의 결승 페이지를 냈다
    finish_emitted: bool,
    /// 떠난 영수증 표면: 창을 바꿨다가(Alt-Tab) 그 표면으로 돌아오면 다시 막는다
    receipt_memory: Option<(SurfaceSignature, String)>,
    /// 잠금 전 체크포인트의 (`cc`, 잠근 시각)
    bridge: Option<(String, i64)>,
    frozen_spans: Vec<(i64, i64)>,
    frozen_since: Option<i64>,
    /// 이번 확인에 쓰는 다리 유지 시간 (`bridge_ttl_ms` × 개인 속도)
    active_ttl_ms: i64,
    /// 지난 확인의 시각 (인증 앱에 있던 시간만큼 다리 시계를 멈춘다)
    last_step_ms: Option<i64>,
}

impl Default for PrivacyStateMachine {
    fn default() -> Self {
        PrivacyStateMachine {
            phase: PrivacyPhase::Normal,
            grace_ms: GRACE_MS,
            min_clean_ticks: GRACE_TICKS,
            post_payment_arm_ms: POST_PAYMENT_ARM_MS,
            bridge_ttl_ms: BRIDGE_TTL_MS,
            last_payment_unlock_ms: None,
            finish_emitted: false,
            receipt_memory: None,
            bridge: None,
            frozen_spans: Vec::new(),
            frozen_since: None,
            active_ttl_ms: BRIDGE_TTL_MS,
            last_step_ms: None,
        }
    }
}

impl PrivacyStateMachine {
    pub fn new() -> PrivacyStateMachine {
        PrivacyStateMachine::default()
    }

    /// 지금 캡처 · 기록해도 되는가 (Normal)
    pub fn capture_allowed(&self) -> bool {
        matches!(self.phase, PrivacyPhase::Normal)
    }

    /// 확인 한 번으로 상태를 옮긴다. 캡처하기 전에 부른다 (잠금 · 유예 · 영수증 보호 중에는 500 ms 마다)
    pub fn step(&mut self, probe: &ZeroPixelProbe) -> ResumeStepOutcome {
        self.step_with(probe, &StepContext::default())
    }

    /// [`step`](Self::step) 에 문맥을 더한 것. 인증 잠금(결제 문맥이거나 `auth_context`) 중에 앞 창이 인증 앱이면 같은 인증의
    /// OTP · 패스키 단계로 본다: 상태를 그대로 두고(캡처 · 기록 없음, 유예를 세지 않음) 그동안 다리 시계를 멈춘다. 그래서 원래
    /// 앱으로 돌아와 풀리면 잠금 전 체크포인트가 이어진다(`bridge_broken == false`). 다리 유지 시간은 개인 속도만큼 늘린다
    pub fn step_with(&mut self, probe: &ZeroPixelProbe, cx: &StepContext) -> ResumeStepOutcome {
        self.active_ttl_ms = cx.pace.unwrap_or(PaceScale::NEUTRAL).scale_ttl_ms(self.bridge_ttl_ms);
        let elapsed = self.last_step_ms.map_or(0, |last| (probe.now_ms - last).max(0));
        self.last_step_ms = Some(probe.now_ms);
        let auth_lock = match &self.phase {
            PrivacyPhase::Locked { had_payment_context, .. } | PrivacyPhase::GraceProbe { had_payment_context, .. } => *had_payment_context || cx.auth_context,
            _ => false,
        };
        if auth_lock && cx.authenticator_surface {
            if let Some((_, since)) = self.bridge.as_mut() {
                *since += elapsed;
            }
            if let PrivacyPhase::GraceProbe { clear_since_ms, .. } = &mut self.phase {
                *clear_since_ms += elapsed;
            }
            return ResumeStepOutcome::default();
        }
        self.step_inner(probe)
    }

    fn step_inner(&mut self, probe: &ZeroPixelProbe) -> ResumeStepOutcome {
        let now = probe.now_ms;
        let mut out = ResumeStepOutcome::default();
        if self.bridge.as_ref().is_some_and(|(_, since)| now - since > self.active_ttl_ms) {
            self.bridge = None;
            out.bridge_broken = true;
        }
        let was_capturing = self.capture_allowed();
        self.phase = match std::mem::replace(&mut self.phase, PrivacyPhase::Normal) {
            PrivacyPhase::Normal => self.step_normal(probe, &mut out),
            PrivacyPhase::Locked {
                since_ms,
                trigger,
                locked_surface,
                had_payment_context,
            } => {
                let had_payment_context = had_payment_context || probe.in_payment_flow;
                if probe.unoccluded_trigger.is_some() || (trigger.pixel_only() && probe.surface == locked_surface) {
                    PrivacyPhase::Locked {
                        since_ms,
                        trigger,
                        locked_surface,
                        had_payment_context,
                    }
                } else {
                    let grace = PrivacyPhase::GraceProbe {
                        locked_since_ms: since_ms,
                        clear_since_ms: now,
                        consecutive_clean_ticks: 1,
                        had_payment_context,
                        trigger,
                        locked_surface,
                    };
                    self.clean_tick(probe, grace, &mut out)
                }
            }
            PrivacyPhase::GraceProbe {
                locked_since_ms,
                clear_since_ms,
                consecutive_clean_ticks,
                had_payment_context,
                trigger,
                locked_surface,
            } => {
                let had_payment_context = had_payment_context || probe.in_payment_flow;
                if let Some(again) = probe.unoccluded_trigger {
                    // 유예 중 2단계 인증 창 같은 것: 처음 잠근 시각을 그대로 두고 다시 잠근다 (동결 구간 하나)
                    PrivacyPhase::Locked {
                        since_ms: locked_since_ms,
                        trigger: again,
                        locked_surface: probe.surface,
                        had_payment_context,
                    }
                } else if trigger.pixel_only() && probe.surface == locked_surface {
                    PrivacyPhase::Locked {
                        since_ms: locked_since_ms,
                        trigger,
                        locked_surface,
                        had_payment_context,
                    }
                } else {
                    let grace = PrivacyPhase::GraceProbe {
                        locked_since_ms,
                        clear_since_ms,
                        consecutive_clean_ticks: consecutive_clean_ticks.saturating_add(1),
                        had_payment_context,
                        trigger,
                        locked_surface,
                    };
                    self.clean_tick(probe, grace, &mut out)
                }
            }
            PrivacyPhase::ReceiptGuard {
                entered_ms,
                receipt_surface,
                receipt_page_hash,
                finish_checkpoint_emitted,
            } => match (probe.unoccluded_trigger, &probe.receipt_finish_page_hash) {
                (Some(trigger), _) => {
                    self.receipt_memory = Some((receipt_surface, receipt_page_hash));
                    PrivacyPhase::Locked {
                        since_ms: now,
                        trigger,
                        locked_surface: probe.surface,
                        had_payment_context: true,
                    }
                }
                // 같은 창에서 영수증 페이지에 머문다 (영수증 안의 다른 페이지로 옮겨도): 시간 제한 없이 막는다
                (None, Some(hash)) if probe.surface.hwnd_id == receipt_surface.hwnd_id => PrivacyPhase::ReceiptGuard {
                    entered_ms,
                    receipt_surface: probe.surface,
                    receipt_page_hash: hash.clone(),
                    finish_checkpoint_emitted,
                },
                // 떠났다. 다른 창으로 옮겼으면(Alt-Tab) 그 영수증 표면으로 돌아올 때 다시 막도록 기억하고,
                // 같은 창이 다른 페이지로 갔으면 영수증은 끝났다
                _ => {
                    self.receipt_memory = (probe.surface.hwnd_id != receipt_surface.hwnd_id).then_some((receipt_surface, receipt_page_hash));
                    PrivacyPhase::Normal
                }
            },
        };
        let capturing = self.capture_allowed();
        if was_capturing && !capturing {
            self.frozen_since = Some(now);
            out.privacy_epoch_started = true;
        } else if !was_capturing && capturing {
            if let Some(start) = self.frozen_since.take() {
                self.seal_span(start, now);
            }
            out.trigger_sensor_reprime = true;
        }
        out.capture_and_record_allowed = capturing;
        out
    }

    /// Normal 에서의 확인: 민감 신호가 보이면 잠그고, 결제 뒤 기다리던 영수증이거나 떠났던 영수증 표면이면 막는다
    fn step_normal(&mut self, probe: &ZeroPixelProbe, out: &mut ResumeStepOutcome) -> PrivacyPhase {
        let now = probe.now_ms;
        if let Some(trigger) = probe.unoccluded_trigger {
            self.finish_emitted = false;
            self.bridge = Some((probe.active_cc.clone(), now));
            return PrivacyPhase::Locked {
                since_ms: now,
                trigger,
                locked_surface: probe.surface,
                had_payment_context: probe.in_payment_flow || trigger == PrivacyTrigger::OverlayPaymentWindow,
            };
        }
        if self.last_payment_unlock_ms.is_some_and(|t| now - t > self.post_payment_arm_ms) {
            self.last_payment_unlock_ms = None;
        }
        if let Some(hash) = &probe.receipt_finish_page_hash {
            if self.last_payment_unlock_ms.is_some() {
                return self.enter_receipt(probe, hash, true, out);
            }
            if self.receipt_memory.as_ref().is_some_and(|(s, h)| *s == probe.surface && h == hash) {
                return self.enter_receipt(probe, hash, false, out);
            }
        }
        if self.receipt_memory.as_ref().is_some_and(|(s, _)| s.hwnd_id == probe.surface.hwnd_id && *s != probe.surface) {
            // 그 창이 다른 페이지로 옮겼다: 영수증을 떠났다
            self.receipt_memory = None;
        }
        PrivacyPhase::Normal
    }

    /// 유예 중 깨끗한 확인 한 번: 결제 문맥이면 영수증 보호로, 유예를 다 채웠으면 Normal 로
    fn clean_tick(&mut self, probe: &ZeroPixelProbe, grace: PrivacyPhase, out: &mut ResumeStepOutcome) -> PrivacyPhase {
        let PrivacyPhase::GraceProbe {
            clear_since_ms,
            consecutive_clean_ticks,
            had_payment_context,
            ..
        } = grace
        else {
            return grace;
        };
        if had_payment_context {
            if let Some(hash) = &probe.receipt_finish_page_hash {
                self.settle_bridge(probe, out);
                return self.enter_receipt(probe, hash, true, out);
            }
        }
        if probe.now_ms - clear_since_ms >= self.grace_ms && consecutive_clean_ticks >= self.min_clean_ticks {
            if had_payment_context {
                self.last_payment_unlock_ms = Some(probe.now_ms);
            }
            self.settle_bridge(probe, out);
            return PrivacyPhase::Normal;
        }
        grace
    }

    /// 영수증 보호에 들어간다. 결제 흐름에서 처음이면 결승 페이지 해시를 낸다 (`emit`: 결제 문맥으로 들어왔다)
    fn enter_receipt(&mut self, probe: &ZeroPixelProbe, hash: &str, emit: bool, out: &mut ResumeStepOutcome) -> PrivacyPhase {
        if emit && !self.finish_emitted {
            self.finish_emitted = true;
            out.sanitized_finish_page = Some(hash.to_string());
        }
        self.last_payment_unlock_ms = None;
        PrivacyPhase::ReceiptGuard {
            entered_ms: probe.now_ms,
            receipt_surface: probe.surface,
            receipt_page_hash: hash.to_string(),
            finish_checkpoint_emitted: self.finish_emitted,
        }
    }

    /// 잠금에서 돌아왔다: 같은 `cc` 이고 [`BRIDGE_TTL_MS`] 안이면 잠금 전 체크포인트를 그대로 잇는다
    fn settle_bridge(&mut self, probe: &ZeroPixelProbe, out: &mut ResumeStepOutcome) {
        if let Some((cc, since)) = self.bridge.take() {
            let moved = !cc.is_empty() && !probe.active_cc.is_empty() && !cc.eq_ignore_ascii_case(&probe.active_cc);
            out.bridge_broken |= moved || probe.now_ms - since > self.active_ttl_ms;
        }
    }

    fn seal_span(&mut self, start: i64, end: i64) {
        self.frozen_spans.push((start, end));
        self.frozen_spans = merge_frozen_spans(&self.frozen_spans);
        let excess = self.frozen_spans.len().saturating_sub(MAX_FROZEN_SPANS);
        self.frozen_spans.drain(..excess);
    }

    /// 동결 구간 `(시작, 끝)` ms: 끝난 구간과, 지금 잠겨 있으면 `now_ms` 까지의 구간. 정렬하고 겹친 것은 합쳤다.
    /// 타임트라이얼(`ReduceOptions.frozen_spans`) · [`crate::split_episodes_with_freezes`] 에 그대로 넘긴다
    pub fn active_frozen_spans(&self, now_ms: i64) -> Vec<(i64, i64)> {
        let mut spans = self.frozen_spans.clone();
        if let Some(start) = self.frozen_since {
            spans.push((start, now_ms));
        }
        merge_frozen_spans(&spans)
    }

    /// 새 런을 시작할 때 지난 동결 구간을 버린다 (진행 중인 잠금은 남는다)
    pub fn clear_frozen_spans(&mut self) {
        self.frozen_spans.clear();
    }
}

/// 동결 구간을 시작 순으로 정렬하고, 겹치거나 맞닿은 것을 합친다. 끝이 시작보다 이르거나 같은 구간은 버린다
pub fn merge_frozen_spans(spans: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut sorted: Vec<(i64, i64)> = spans.iter().copied().filter(|(start, end)| end > start).collect();
    sorted.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// `[from_ms, to_ms]` 와 동결 구간이 겹친 시간의 합 (ms). 겹친 동결 구간은 한 번만 센다. `to_ms` 에 대해 줄지 않고
/// `to_ms - from_ms` 를 넘지 않아서, `(to_ms - from_ms) - frozen_overlap_ms(..)` 도 줄지 않는다(구간 시간이 단조롭다)
pub fn frozen_overlap_ms(from_ms: i64, to_ms: i64, spans: &[(i64, i64)]) -> i64 {
    if to_ms <= from_ms {
        return 0;
    }
    merge_frozen_spans(spans).iter().map(|(start, end)| (to_ms.min(*end) - from_ms.max(*start)).max(0)).sum()
}

/// 낱말 비교용: 주소면 틀([`url_template`])로 바꿔 쿼리 · 조각을 버리고, 소문자로 바꾸고, 공백 · `-` · `_` 를 뺀다
fn squash(url_or_title: &str) -> String {
    let text = match url_or_title.contains("://") {
        true => url_template(url_or_title).unwrap_or_default(),
        false => url_or_title.to_string(),
    };
    text.to_lowercase().chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').collect()
}

/// 주문 완료 · 영수증 화면인가: 주소 틀(쿼리 없이) · 창 제목의 조각으로 본다 (`/order/complete`, `/checkout/success`,
/// `/payment/result`, `주문완료`, `결제완료`, `Order Confirmed` …). 페이지 글 · 픽셀은 보지 않는다
pub fn receipt_route(url_or_title: &str) -> bool {
    let text = squash(url_or_title);
    RECEIPT_MARKS.iter().any(|m| text.contains(m))
}

/// 결제 · 주문서 진행 화면인가 (`/checkout`, `/pay…`, `결제`, `주문서` …). 영수증 화면도 여기에 걸리므로 [`receipt_route`] 를 먼저 본다
pub fn payment_route(url_or_title: &str) -> bool {
    let text = squash(url_or_title);
    PAYMENT_MARKS.iter().any(|m| text.contains(m))
}

/// 주문 완료 · 영수증 페이지면 그 페이지 해시([`crate::page_hash`]: 쿼리의 주문번호 · 이메일은 주소 틀에서 빠진다).
/// `url` 은 `http(s)://` 주소다 (창 제목이면 None)
pub fn receipt_finish_page(url: &str) -> Option<String> {
    if url.contains("://") && receipt_route(url) {
        page_hash(url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(now_ms: i64, surface: SurfaceSignature, trigger: Option<PrivacyTrigger>) -> ZeroPixelProbe {
        ZeroPixelProbe {
            now_ms,
            active_cc: "shop.example".into(),
            surface,
            unoccluded_trigger: trigger,
            in_payment_flow: false,
            receipt_finish_page_hash: None,
        }
    }

    /// 500 ms 마다 확인한다: `from..=to` 의 각 시각에 `make(t)` 를 넣고 결과를 모은다
    fn run(m: &mut PrivacyStateMachine, from: i64, to: i64, make: impl Fn(i64) -> ZeroPixelProbe) -> Vec<(i64, ResumeStepOutcome)> {
        (from..=to).step_by(500).map(|t| (t, m.step(&make(t)))).collect()
    }

    #[test]
    fn ps09_a_long_lived_bank_window_stays_locked_and_a_quick_login_resumes_in_six_seconds() {
        let bank = SurfaceSignature::new(0x1001, "bank.example/transfer");
        // A-1: 캡처 차단 은행 창을 120초 띄워 둔다
        let mut m = PrivacyStateMachine::new();
        let out = run(&mut m, 0, 120_000, |t| probe(t, bank, Some(PrivacyTrigger::DisplayAffinityProtected)));
        assert!(out[0].1.privacy_epoch_started);
        assert!(out.iter().all(|(_, o)| !o.capture_and_record_allowed), "120초 내내 캡처 0");
        assert!(matches!(m.phase, PrivacyPhase::Locked { since_ms: 0, .. }));

        // A-2: 비전만 본 은행 사본(그림 보기 창): 확인으로는 신호가 안 보여도, 그 표면에 있는 동안은 풀지 않는다
        let viewer = SurfaceSignature::new(0x2002, "계좌사본.png - 사진");
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, viewer, Some(PrivacyTrigger::VisionPrivacyDomain)));
        let out = run(&mut m, 500, 120_000, |t| probe(t, viewer, None));
        assert!(out.iter().all(|(_, o)| !o.capture_and_record_allowed && !o.trigger_sensor_reprime), "3초 재노출 고리 0회");
        for t in [60_000, 120_000] {
            assert!(out.iter().any(|(at, _)| *at == t));
        }
        assert!(matches!(m.phase, PrivacyPhase::Locked { trigger: PrivacyTrigger::VisionPrivacyDomain, .. }));
        // 표면을 떠나면 유예를 거쳐 돌아온다
        let game = SurfaceSignature::new(0x3003, "Game");
        let out = run(&mut m, 120_500, 124_000, |t| probe(t, game, None));
        let resumed: Vec<i64> = out.iter().filter(|(_, o)| o.trigger_sensor_reprime).map(|(t, _)| *t).collect();
        assert_eq!(resumed, [123_500]);

        // B: 3초 로그인 뒤 전체 화면 불투명 게임으로 (뒤의 은행 창은 100% 가려져 신호가 없다)
        let login = SurfaceSignature::new(0x4004, "bank.example/login");
        let mut m = PrivacyStateMachine::new();
        let mut out = run(&mut m, 0, 2_500, |t| probe(t, login, Some(PrivacyTrigger::TreeSensitiveField)));
        out.extend(run(&mut m, 3_000, 8_000, |t| probe(t, game, None)));
        let resumed: Vec<i64> = out.iter().filter(|(_, o)| o.trigger_sensor_reprime).map(|(t, _)| *t).collect();
        assert_eq!(resumed, [6_000], "신호가 사라지고 3초(GraceProbe) 뒤, 모두 ~6초에 돌아온다");
        assert!(out.iter().all(|(t, o)| o.capture_and_record_allowed == (*t >= 6_000)));
        assert_eq!(m.active_frozen_spans(8_000), [(0, 6_000)]);
    }

    #[test]
    fn ps10_a_receipt_page_stays_guarded_until_left_and_emits_one_finish_page() {
        let receipt_url = "https://shop.example/order/complete?no=20261121-0042&email=hong@example.com";
        let receipt = receipt_finish_page(receipt_url).expect("영수증 주소");
        assert_eq!(Some(receipt.clone()), page_hash("https://shop.example/order/complete"), "쿼리의 주문번호 · 이메일은 해시에 없다");
        let (checkout, receipt_page, home) = (
            SurfaceSignature::new(0x10, "shop.example/checkout"),
            SurfaceSignature::new(0x10, "shop.example/order/complete"),
            SurfaceSignature::new(0x10, "shop.example"),
        );
        let overlay = SurfaceSignature::new(0x20, "Steam Overlay");
        let mut m = PrivacyStateMachine::new();
        let at_checkout = |t| ZeroPixelProbe {
            in_payment_flow: true,
            ..probe(t, checkout, None)
        };
        assert!(m.step(&at_checkout(0)).capture_and_record_allowed);
        // 결제 창이 떠 있는 20초
        let mut out = run(&mut m, 500, 20_000, |t| ZeroPixelProbe {
            in_payment_flow: true,
            ..probe(t, overlay, Some(PrivacyTrigger::OverlayPaymentWindow))
        });
        // 결제 창이 닫히고 /order/complete 에 30초 머문다 (실명 · 주소가 적힌 페이지)
        let on_receipt = |t| ZeroPixelProbe {
            receipt_finish_page_hash: Some(receipt.clone()),
            ..probe(t, receipt_page, None)
        };
        let guard_from = out.len();
        out.extend(run(&mut m, 20_500, 50_500, on_receipt));
        let finish: Vec<(i64, String)> = out.iter().filter_map(|(t, o)| o.sanitized_finish_page.clone().map(|h| (*t, h))).collect();
        assert_eq!(finish, [(20_500, receipt.clone())], "정제된 결승 페이지 해시는 들어갈 때 한 번만");
        assert!(out.iter().all(|(_, o)| !o.capture_and_record_allowed), "15초가 지난 30초 시점까지도 캡처 · 기록 0");
        assert!(out[guard_from..].iter().any(|(t, _)| *t == 20_500 + POST_PAYMENT_ARM_MS + 500));
        assert!(matches!(m.phase, PrivacyPhase::ReceiptGuard { finish_checkpoint_emitted: true, .. }));
        // /home 으로 옮긴 바로 그때에만 돌아온다
        let back = m.step(&probe(51_000, home, None));
        assert!(back.capture_and_record_allowed && back.trigger_sensor_reprime && back.sanitized_finish_page.is_none());
        assert_eq!(m.active_frozen_spans(52_000), [(500, 51_000)]);
    }

    #[test]
    fn a_second_factor_during_grace_relocks_into_one_frozen_span() {
        let (login, otp, game) = (SurfaceSignature::new(1, "login"), SurfaceSignature::new(1, "otp"), SurfaceSignature::new(2, "game"));
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, login, Some(PrivacyTrigger::TreeSensitiveField)));
        run(&mut m, 500, 1_000, |t| probe(t, login, None)); // 로딩 스피너: 유예
        assert!(matches!(m.phase, PrivacyPhase::GraceProbe { locked_since_ms: 0, .. }));
        let relock = m.step(&probe(1_500, otp, Some(PrivacyTrigger::VirtualSecurityKeypad)));
        assert!(!relock.capture_and_record_allowed && !relock.privacy_epoch_started, "새 잠금이 아니라 같은 잠금이다");
        assert!(matches!(m.phase, PrivacyPhase::Locked { since_ms: 0, trigger: PrivacyTrigger::VirtualSecurityKeypad, .. }));
        let out = run(&mut m, 2_000, 6_000, |t| probe(t, game, None));
        assert!(out.iter().any(|(t, o)| *t == 5_000 && o.trigger_sensor_reprime));
        assert_eq!(m.active_frozen_spans(6_000), [(0, 5_000)]);
        // 유예 중에는 아직 잠긴 셈이라 진행 중인 구간이 지금까지 이어진다
        let mut m2 = PrivacyStateMachine::new();
        m2.step(&probe(10_000, login, Some(PrivacyTrigger::TreeSensitiveField)));
        m2.step(&probe(10_500, game, None));
        assert_eq!(m2.active_frozen_spans(11_000), [(10_000, 11_000)]);
    }

    #[test]
    fn grace_needs_both_time_and_clean_ticks() {
        let (login, game) = (SurfaceSignature::new(1, "login"), SurfaceSignature::new(2, "game"));
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, login, Some(PrivacyTrigger::TreeSensitiveField)));
        // 확인이 드물게 온다: 3초가 지나도 깨끗한 확인이 3번 모여야 푼다
        assert!(!m.step(&probe(1_000, game, None)).capture_and_record_allowed);
        assert!(!m.step(&probe(5_000, game, None)).capture_and_record_allowed);
        assert!(m.step(&probe(6_000, game, None)).capture_and_record_allowed);
    }

    #[test]
    fn a_receipt_is_guarded_only_after_a_payment_and_again_after_alt_tab() {
        let receipt = page_hash("https://shop.example/order/complete").unwrap();
        let (page, home, game) = (SurfaceSignature::new(7, "shop.example/order/complete"), SurfaceSignature::new(7, "shop.example"), SurfaceSignature::new(8, "game"));
        let on_receipt = |t| ZeroPixelProbe {
            receipt_finish_page_hash: Some(receipt.clone()),
            ..probe(t, page, None)
        };
        // 결제 없이 연 주문 완료 페이지(주문 내역에서 다시 열기)는 영수증 보호도 결승도 없다
        let mut m = PrivacyStateMachine::new();
        assert!(m.step(&on_receipt(0)).capture_and_record_allowed);
        // 결제 잠금이 풀리고(유예를 다 채운 뒤) 15초 안에 영수증이 오면 막는다
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, game, Some(PrivacyTrigger::OverlayPaymentWindow)));
        run(&mut m, 500, 3_500, |t| probe(t, game, None));
        assert!(m.capture_allowed());
        let guarded = m.step(&on_receipt(10_000));
        assert!(!guarded.capture_and_record_allowed && guarded.privacy_epoch_started);
        assert_eq!(guarded.sanitized_finish_page.as_deref(), Some(receipt.as_str()));
        // 창을 바꾸면(Alt-Tab) 캡처하고, 그 영수증 표면으로 돌아오면 결승 없이 다시 막는다
        assert!(m.step(&probe(11_000, game, None)).capture_and_record_allowed);
        let again = m.step(&on_receipt(60_000));
        assert!(!again.capture_and_record_allowed && again.sanitized_finish_page.is_none());
        // 그 창이 다른 페이지로 가면 끝난다
        assert!(m.step(&probe(61_000, home, None)).capture_and_record_allowed);
        assert!(m.step(&on_receipt(62_000)).capture_and_record_allowed, "기억을 지웠고 결제도 없었다");
        // 결제 잠금이 풀리고 15초가 지나서 온 영수증은 결제와 잇지 않는다
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, game, Some(PrivacyTrigger::OverlayPaymentWindow)));
        run(&mut m, 500, 3_500, |t| probe(t, game, None));
        assert!(m.step(&on_receipt(3_500 + POST_PAYMENT_ARM_MS + 500)).capture_and_record_allowed);
    }

    #[test]
    fn a_lock_inside_the_receipt_guard_never_emits_a_second_finish() {
        let receipt = page_hash("https://shop.example/checkout/success").unwrap();
        let page = SurfaceSignature::new(7, "shop.example/checkout/success");
        let on_receipt = |t| ZeroPixelProbe {
            receipt_finish_page_hash: Some(receipt.clone()),
            ..probe(t, page, None)
        };
        let mut m = PrivacyStateMachine::new();
        m.step(&ZeroPixelProbe {
            in_payment_flow: true,
            ..probe(0, page, Some(PrivacyTrigger::TreeSensitiveField))
        });
        let mut finishes = 0;
        for (t, trigger) in [(1_000, None), (2_000, Some(PrivacyTrigger::VirtualSecurityKeypad)), (3_000, None), (4_000, None)] {
            let out = m.step(&ZeroPixelProbe {
                unoccluded_trigger: trigger,
                ..on_receipt(t)
            });
            finishes += usize::from(out.sanitized_finish_page.is_some());
            assert!(!out.capture_and_record_allowed);
        }
        assert_eq!(finishes, 1);
    }

    #[test]
    fn the_bridge_breaks_on_another_app_or_after_fifteen_minutes() {
        let (login, game) = (SurfaceSignature::new(1, "login"), SurfaceSignature::new(2, "game"));
        let resume = |m: &mut PrivacyStateMachine, from: i64, cc: &str| {
            run(m, from, from + 3_000, |t| ZeroPixelProbe {
                active_cc: cc.into(),
                ..probe(t, game, None)
            })
            .into_iter()
            .any(|(_, o)| o.bridge_broken)
        };
        // 같은 사이트로 돌아왔다: 잠금 전 체크포인트를 잇는다 (인증 창이 다른 cc 여도 잠금 중에는 상관없다)
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, login, Some(PrivacyTrigger::TreeSensitiveField)));
        m.step(&ZeroPixelProbe {
            active_cc: "auth.example".into(),
            ..probe(500, login, Some(PrivacyTrigger::TreeSensitiveField))
        });
        assert!(!resume(&mut m, 1_000, "shop.example"));
        // 다른 앱에서 돌아왔다
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, login, Some(PrivacyTrigger::TreeSensitiveField)));
        assert!(resume(&mut m, 1_000, "other.example"));
        // 15분 넘게 잠겨 있었다: 잠긴 채로 바로 알린다
        let mut m = PrivacyStateMachine::new();
        m.step(&probe(0, login, Some(PrivacyTrigger::TreeSensitiveField)));
        assert!(!m.step(&probe(BRIDGE_TTL_MS, login, Some(PrivacyTrigger::TreeSensitiveField))).bridge_broken);
        assert!(m.step(&probe(BRIDGE_TTL_MS + 500, login, Some(PrivacyTrigger::TreeSensitiveField))).bridge_broken);
        assert!(!resume(&mut m, BRIDGE_TTL_MS + 1_000, "shop.example"), "한 번만 알린다");
    }

    #[test]
    fn frozen_overlap_counts_merged_spans_once_and_stays_monotone() {
        let spans = [(10, 20), (15, 30), (40, 50), (60, 60), (80, 70)];
        assert_eq!(merge_frozen_spans(&spans), [(10, 30), (40, 50)]);
        assert_eq!(merge_frozen_spans(&[(0, 5), (5, 9)]), [(0, 9)], "맞닿은 구간도 합친다");
        assert_eq!(frozen_overlap_ms(0, 100, &spans), 30);
        assert_eq!(frozen_overlap_ms(12, 45, &spans), 18 + 5);
        assert_eq!(frozen_overlap_ms(50, 40, &spans), 0);
        assert_eq!(frozen_overlap_ms(0, 100, &[]), 0);
        let mut last = 0;
        for to in 0..=100 {
            let effective = to - frozen_overlap_ms(0, to, &spans);
            assert!(effective >= last, "유효 경과 시간은 줄지 않는다");
            last = effective;
        }
        // PS-11 의 모양: 8초 뒤 45초 인증, 55초에 결승 → 유효 10초
        assert_eq!(55_000 - frozen_overlap_ms(0, 55_000, &[(8_500, 53_500)]), 10_000);
    }

    #[test]
    fn receipt_and_payment_routes_come_from_url_templates_and_titles() {
        for receipt in [
            "https://shop.example/order/complete?orderNo=1",
            "https://shop.example/checkout/success",
            "https://pay.example/payment/result#done",
            "https://shop.example/order-complete",
            "주문완료 - 쇼핑몰",
            "결제 완료",
            "Order Confirmed | Shop",
        ] {
            assert!(receipt_route(receipt), "{receipt}");
        }
        for other in ["https://shop.example/cart", "https://shop.example/orders/list", "https://shop.example/?next=order/complete", "장바구니"] {
            assert!(!receipt_route(other), "{other}");
        }
        for pay in ["https://shop.example/checkout", "https://shop.example/pay/card", "결제하기", "주문서 작성", "Payment"] {
            assert!(payment_route(pay), "{pay}");
        }
        assert!(!payment_route("https://shop.example/display/42"));
        assert_eq!(receipt_finish_page("결제완료"), None, "창 제목은 페이지 해시가 아니다");
        assert_eq!(receipt_finish_page("https://shop.example/cart"), None);
        assert_ne!(SurfaceSignature::new(1, "a"), SurfaceSignature::new(1, "b"));
        assert_eq!(SurfaceSignature::new(1, " a "), SurfaceSignature::new(1, "a"));
    }

    #[test]
    fn an_authenticator_detour_during_an_auth_lock_keeps_the_bridge() {
        let bank = SurfaceSignature::new(0x2001, "bank.example/login");
        let otp = SurfaceSignature::new(0x3001, "authenticator");
        let at = |t: i64, cc: &str, surface: SurfaceSignature, trigger: Option<PrivacyTrigger>| ZeroPixelProbe {
            active_cc: cc.into(),
            in_payment_flow: true,
            ..probe(t, surface, trigger)
        };
        // 로그인 화면에서 보안 키패드로 잠긴 뒤 OTP 앱으로 가서 40초 머물고 돌아와 푼다
        // (다리가 끊겼나, OTP 화면에서 캡처한 확인 수)
        let script = |m: &mut PrivacyStateMachine, cx_otp: StepContext| -> (bool, usize) {
            let mut broken = false;
            let mut captured_otp = 0usize;
            let mut lock = |m: &mut PrivacyStateMachine, t, cc: &str, s, trig, cx: &StepContext| {
                let out = m.step_with(&at(t, cc, s, trig), cx);
                broken |= out.bridge_broken;
                out
            };
            let first = lock(m, 0, "bank.example", bank, Some(PrivacyTrigger::VirtualSecurityKeypad), &StepContext::default());
            assert_eq!(first.cue(), PrivacyTransitionCue::EnteredLockdown);
            for t in (500..=40_000).step_by(500) {
                let out = lock(m, t, "authenticator.app", otp, None, &cx_otp);
                captured_otp += usize::from(out.capture_and_record_allowed);
            }
            for t in (40_500..=60_000).step_by(500) {
                let out = lock(m, t, "bank.example", bank, None, &StepContext::default());
                if out.trigger_sensor_reprime {
                    assert_eq!(out.cue(), PrivacyTransitionCue::ResumedNormal);
                }
            }
            (broken, captured_otp)
        };
        // before (no context): the grace ran out in the authenticator app, so the lock settled there (the bridge broke) and the
        // code screen was captured
        let (broken, captured) = script(&mut PrivacyStateMachine::new(), StepContext::default());
        assert!(broken && captured > 0, "{broken} {captured}");
        // after: the authenticator step of the same sign-in keeps the lock and pauses the bridge clock
        let otp_cx = StepContext { authenticator_surface: true, ..StepContext::default() };
        let mut m = PrivacyStateMachine::new();
        assert_eq!(script(&mut m, otp_cx), (false, 0));
        // a personal pace stretches the bridge TTL
        let mut m = PrivacyStateMachine::new();
        let slow = StepContext { pace: Some(PaceScale::from_rho_u(2.0)), ..StepContext::default() };
        m.step_with(&at(0, "bank.example", bank, Some(PrivacyTrigger::VirtualSecurityKeypad)), &slow);
        let out = m.step_with(&at(BRIDGE_TTL_MS + 1_000, "bank.example", bank, Some(PrivacyTrigger::VirtualSecurityKeypad)), &slow);
        assert!(!out.bridge_broken, "ρ_u = 2 doubles the 15-minute bridge");
    }
}
