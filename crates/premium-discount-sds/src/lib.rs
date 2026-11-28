//! # premium-discount-sds
//!
//! Score Dynamics (SDS, 상대 가치 판정): 고정 임계값 대신 스코프마다 적응하는 통계로 "이 점수가 *여기서* 드문가"를 본다.
//! Qntology/logis-center(`commerce/src-tauri/src/utils/score_dynamics.rs` 와 `ai_utils.rs` 의 배정 도구)에서 옮겨 온
//! raon-zena 의 `zena-sds` 를 이 crate 로 합쳤다(2026-11-17). raon-zena 의 `zena-sds` 는 이 crate 를 다시 내보낸다.
//!
//! * [`moments`] – 합치고 뺄 수 있는 평균 · 분산 [`Moments`] (모집단 · 표본)와 스코프 백오프 [`backoff`].
//!   premium-discount-nav 의 마찰 기준선이 쓴다.
//! * [`scope`]  – [`Scope`] `{ track, primary, secondary }`, 세 키와 트랙마다의 [`MinObs`].
//! * [`stats`]  – 합칠 수 있는 통계: [`Welford`] (μ, σ, n + 최근 값 고리, `drift_z`), 점수 목록의 [`decay_shape`],
//!   혼동 장부([`ConfusionStat`]), 거절 수([`RejectStat`]), [`ScopeStat`].
//! * [`ledger`] – [`Ledger`]: 저장된 바탕 + 이번 실행의 관측(프로세스마다 실행 파일 하나), secondary → primary →
//!   global → unscoped 백오프 읽기, 혼동 동점 깨기, 전이 사전확률.
//! * [`matrix`] – 이중 센터링과 마진 순 1:1 배정(동점 모양은 부르는 쪽이 정함).
//! * [`knots`]  – 단조 구간 선형 보정표와 그 역변환: 보정 눈금의 문턱을 원시 점수에 그대로 쓰게 한다.
//!
//! 전역 상태가 없다: [`Ledger`] 는 평범한 값(`Send + Sync`)이고, 함께 쓰려면 `Mutex` / `RwLock` 뒤에 둔다.
//! 표본이 모자라면 판정하지 않는다 — 단계가 [`MinObs`] 를 채우기 전에는 읽기가 모두 `None` 이다.

#[cfg(feature = "audit")]
pub mod audit;
pub mod geometry;
pub mod knots;
pub mod ledger;
pub mod matrix;
pub mod moments;
pub mod perforate;
pub mod scope;
pub mod stats;
pub mod temporal;

mod persist;

pub use ledger::{Band, Ledger, Level};
pub use matrix::{AssignDiag, TieKind};
pub use moments::{backoff, Moments};
pub use scope::{team_fate, MinObs, RunRetire, Scope, TeamFate, WriteMode};
pub use stats::{
    adaptive_half_life_ms, decay_shape, tie_verdict, CalibrationStat, ConfusionStat, DecayShape,
    DecayStat, GateStat, RejectStat, ScopeStat, Welford, CALIBRATION_SHRINK_K,
    DRIFT_HALF_LIFE_GAMMA, DRIFT_HALF_LIFE_TRIGGER_Z, DRIFT_MIN_HALF_LIFE_MS,
};

/// 장부 · 실행 파일에 적는 세대 표시. 다른 recipe 의 파일은 합치지 않고 비켜 둔다. 기기에 쌓인 장부를 그대로 읽도록
/// `zena-sds` 때의 값을 그대로 쓴다.
pub const RECIPE: &str = "zena-sds-v1:welford+ring/run-files/backoff/confusion/transition";

/// 역사적 승자 쪽으로 동점을 깨는 최소 승률 (과반이면 정말 번갈아 이기는 쌍이 굳어 버린다).
pub const TIE_WIN_RATE: f32 = 0.65;

/// 동점이 역사적 승자 쪽으로 가려면, 그 승자가 상대보다 거절된 비율이 이만큼을 넘지 않아야 한다
/// ([`Ledger::learned_specificity`]).
pub const TIE_SPECIFICITY_SLACK: f32 = 0.15;
