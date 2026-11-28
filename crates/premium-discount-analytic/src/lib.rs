//! # premium-discount-analytic
//!
//! 웹 행동 분석 코어. logis-center 의 분석(analytic) 파이프라인과 tracker-logis-center 의 수집 사건을 옮겼다(2026-11-18).
//!
//! | 모듈 | 하는 일 | 옮겨 온 곳 |
//! | --- | --- | --- |
//! | [`event`] | 수집 사건 `web-event/1`: 속성 없는 PUG + 셀렉터용 조상 모양, 개인정보 거르기, 모달 가상 주소 · 탭 묶음 · 재전송에도 같은 사건 id | tracker-logis-center `tracker.js` · `workers/src/index.ts` |
//! | [`funnel`] | Track 1 (모델 없음): 웹 체크포인트, 선택 대조(고른 요소 vs 지나친 형제, 위치 편향 보정), 마찰 에피소드, 퍼널(단축 경로 · 비교 쇼핑 면제) | 새로 (premium-discount-nav 규칙) |
//! | [`pug`] | 속성 없는 PUG (`{들여쓰기}{태그} \| {글}`), 예전 HTML 사건 변환, 가변 문구를 뺀 선택 대조 키 | logis-center `analytic.rs` · `parsing.rs` · `ai_utils.rs` |
//! | [`query`] | 분석 질의 해석: 기간(비교 포함) · 계절 · 사건 종류 · 페이지 종류 · 행동 · 인사이트 | logis-center `query_analytic.rs` · `full_bias.json` |
//! | [`time`] | 결정론적 시간 해석 (모델이 낸 날짜는 믿지 않는다), 같은 경과 시간 기간 비교 | logis-center `utils/time_guide.rs` |
//! | [`blob`] | 사건 BLOB 코덱 (BUG-RT-2/3/4 고침) | logis-center `runtime.ts` `decodeAnalyticBlob` |
//! | [`report`] | 리포트의 표본 신뢰도 · 단정하지 않는 기간 비교 문장 | 새로 (시나리오 5) |
//! | [`forget`] | 지운 사건을 다시 들이지 않는 판단 (id 묘비 + 사이트 컷오프 묘비, 구조화한 행 보호) | 앱 `analytic.rs` 의 가드에서 |
//!
//! 모델 · 저장소 · HTTP 에 의존하지 않는다. 임베딩(embeddinggemma 768차원) · 로컬 저장(LanceDB)은 앱(`apps/core/src/analytic.rs`)이 한다.

pub mod blob;
pub mod event;
pub mod forget;
pub mod funnel;
pub mod negation;
pub mod pug;
pub mod query;
pub mod report;
pub mod time;
