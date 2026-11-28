//! # premium-discount-quant
//!
//! 원본 `safetensors`(BF16/F32, mmap)를 한 번만 읽어 양자화하는 데 필요한 형식 · 인코더 · 커널, 그리고 출력 투영 어휘 잘라 내기.
//! raon-zena 의 `raon-quant` 에서 모델과 상관없는 부분을 옮겼다(2026-11-17). Raon 모델의 층 배치 규칙(피라미드 계획)과
//! 블록 캐시는 raon-zena 의 얇은 `raon-quant` 에 남는다. candle 에 의존하지 않는다: 블록 바이트를 만들고, candle 로 올리는 것은
//! 부르는 쪽이다(`qtensor_from_ggml`).
//!
//! * [`kquant`]      – 중요도 가중 최소제곱 블록 인코더 Q8_0 · Q6_K · Q5_K · Q4_K (Qntology/logis-center ggml-kquant 에서 가져옴).
//! * [`kernels`]     – 블록 디코더, 융합 역양자화 · 내적 커널(F32 누산), 시작할 때 자가 검증.
//! * [`vocab`]       – 바이트 수준 BPE 토크나이저에서 남길 행(`kept_ids`, Raon-1-nano 의 고정 규칙)과 [`vocab::VocabSlice`].
//! * [`trim`]        – 문서 언어에 맞춘 남길 행 고르기 (logis-center 의 어휘 잘라 내기 규칙).
//! * [`lmhead`]      – 잘라 낸 출력 투영에서도 전체 어휘와 같은 arg-max (잘린 행 묶음의 코시-슈바르츠 상한), 단조 보정표의
//!   역변환(`uncalibrate_knots`)으로 보정 눈금 문턱에서도 잘못 잘라 내지 않는 코사인 arg-max.
//! * [`probe`]       – 가속기 자가 검증 프로브: 결함 주입은 정적 분기의 `Err`, 패닉은 풀리는 빌드에서만 잡는다.
//! * [`gguf`], [`safetensors`], [`imatrix`] – 파일 형식.

pub mod artifact_trim;
pub mod gguf;
pub mod imatrix;
pub mod kernels;
// Vendored verbatim; keep upstream style.
#[allow(clippy::manual_is_multiple_of)]
#[rustfmt::skip]
pub mod kquant;
pub mod lmhead;
#[cfg(feature = "mine")]
pub mod mine;
pub mod probe;
pub mod safetensors;
pub mod trim;
pub mod vocab;
pub mod vocab_plan;
