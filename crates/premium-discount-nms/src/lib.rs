//! # premium-discount-nms
//!
//! 자연어 요청 · 화면 후보에서 "여럿 중 무엇 하나"를 고르는 도메인 중립 NMS 코어. raon-zena 의 `nlp-nms-router` 에서
//! 캐릭터 전용 부분(제나의 사전 · 동작 · 호출어)을 빼고 옮겼다(2026-11-17). 원 설계는 Qntology/logis-center 의 자연어 검색 +
//! NMS 다(Apache-2.0, `NOTICE`). raon-zena 의 라우터는 이 위에 `zena_lexicon` 사전과 제나의 동작을 얹는다.
//!
//! 1. [`korean`] / [`normalize`] – 호출어 떼기([`normalize::set_wake_words`] 로 정함), 받침 조화를 보는 조사 · 서술격 떼기.
//! 2. [`analyze`] – 역할 분석([`analyze::Analyzer`]): 규칙 분석기, (기능 `stanza`) Stanza 분석기, 부정 범위 표시.
//!    긴 부정("-지 않- · -지 못하-") · 이중 부정 수([`analyze::negation_counts`]), 역접 · 양보 절 나누기([`analyze::clauses`]),
//!    구간의 절 위치 · 부정 수([`analyze::read_span`]).
//! 3. [`embed`]   – [`embed::TextEmbedder`]: 의존 없는 384차원 글자 · 자모 해시 임베더, (기능 `neural`) granite 임베더.
//! 4. [`domain`]  – [`domain::DomainBank`]`<M, E>`: 정확 · 교착어 접두 일치 먼저, 그다음 bias / prejudice 최대 코사인.
//!    항목의 내용 `M` 은 부르는 쪽 타입이다. [`domain::BankBackend`] 는 이 은행과 섹터 계층 은행([`domain::BiasedTieredBank`])을
//!    한 인터페이스로 바꿔 끼운다.
//! 5. [`span`]    – 구간 NMS (v1 그리디, v2 이중 센터링 + 마진 게이트 1:1 배정 + SDS 혼동 장부 동점 깨기). 창 구간에 더해
//!    구문 압축 구간([`span::syntactic_spans`]: 의존 구문 머리-논항, 모델이 없으면 조사 뗀 명사구 + 끝 서술어)을 겨루게 한다
//!    ([`span::span_assign_with`]). 토픽 명사구는 [`span::noun_phrases`].
//! 6. [`semantic`]– 시간 창 안의 의미 NMS, 적응 중복 하한 `clamp(μ + 3σ, τ, 0.995)`.
//! 7. [`spatial`] – 공간-글 NMS: 글 유사도 신뢰도, snap / twin IoU 규칙의 상자 억제, 1:1 배타 배정. 다단 표 셀의
//!    `[행 × 열 × 역할]` 주소([`spatial::TableCellContext`])와 그 문맥으로 하는 요청 ↔ 셀 1:1 배정.
//! 8. [`scope`]   – 원점수에 비례하는 스코프 가산([`scope::AdaptiveScopeBoost`]).

pub mod analyze;
pub mod domain;
pub mod embed;
pub mod korean;
pub mod normalize;
pub mod semantic;
pub mod scope;
pub mod span;
pub mod spatial;
pub mod temporal;

pub use domain::{BankBackend, BankMatch, DomainBank, DomainEntry, DomainSpec, Hit};

/// granite 모델이 설치된 곳 (시험 · 진단용)
#[cfg(feature = "neural")]
#[doc(hidden)]
pub fn __granite_dir() -> Option<std::path::PathBuf> {
    premium_discount_nlp::models::granite_dir()
}
