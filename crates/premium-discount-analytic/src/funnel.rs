//! 웹 행동 분석 Track 1 (모델 없음): 수집 사건 → 웹 체크포인트 · 선택 대조 · 마찰 에피소드 · 퍼널
//!
//! 1. **체크포인트**: 페이지는 [`premium_discount_nav::page_hash`](링크값), 요소는 페이지 + DOM 셀렉터 틀
//!    ([`premium_discount_nav::element_hash`]). 접근성 내비게이션의 웹 코스와 같은 해시다.
//! 2. **선택 대조 (고른 요소 vs 지나친 형제)**: 누르거나 바꾼 요소(TARGET)와 같은 목록에서 고르지 않은 형제(RELATED)를 한
//!    선택 묶음으로 본다. 묶음의 크기가 n 이면 아무 정보 없이 고를 때 각 후보가 뽑힐 확률은 1/n 이다. 후보마다 노출 수 ·
//!    뽑힌 수 · 기댓값(Σ1/n) · 분산(Σ(1/n)(1−1/n))을 쌓고, `z = (뽑힌 수 − 기댓값) / √분산` 이 띠(`band`)를 넘을 때만 premium ·
//!    discount 로 결정한다([`ChoiceTally::decide`]). 띠 안이면 결정하지 않는다(SDS 와 같은 태도).
//!
//!    기획서는 "고른 TARGET 은 premium, 비켜 간 RELATED 는 discount 로 바로 센다" 였다. 그 수를 등급 규칙(2/3)에 넣으면
//!    목록이 길수록 모든 후보가 discount 가 된다: 10개 목록에서 무작위로 고르기만 해도 후보마다 discount 가 9 배 많다
//!    ([`ChoiceTally::naive_tier`], 시험 `the_plan_rule_marks_a_random_list_discount`). 그래서 수는 함께 남기되 결정은
//!    기댓값과 견준다.
//!
//!    같은 틀의 목록(상품 카드)은 셀렉터 틀이 같아 요소 해시도 같다(셀렉터는 데이터 자리를 지운다). 그래서 선택 대조의 키는
//!    요소 해시 + 글(PUG 의 글만, [`crate::pug::text_of`])이다([`choice_key`]).
//! 3. **마찰 에피소드**: 사건을 접근성 내비게이션의 신호로 바꾸고(누름 `Picked`, 광클 `RageClick`, 머묾 `Hovered`, 첫 입력
//!    `Engaged` · 다시 고침 `Retried`, 스크롤 뒤집기 `Rescrolled`, 뒤로 `Backtracked`, 제출 `Submitted`, 바꾼 칸을 두고 떠남
//!    `Abandoned`, 들어와서 첫 동작까지 `Searched`), 세션을 에피소드로 잘라([`premium_discount_nav::split_episodes`]) 같은
//!    사이트의 다른 페이지 · 요소와 견준다([`premium_discount_nav::FrictionBaselines::decide`]).
//! 4. **퍼널**: 고른 페이지 순서(상품 → 장바구니 → 결제 → 완료)마다 도달한 세션 수와 다음 단계로 못 간 수.
//!
//! 시나리오 5 에서 더한 것 (예전 [`ChoiceBook`] · [`SiteAnalysis::funnel`] · [`SiteAnalysis::bottlenecks`] 은 그대로다):
//! 5. **위치 편향 · 가시 경쟁 풀을 고친 선택 대조** ([`DebiasedChoiceBook`]): 키는 가변 문구를 뺀 [`CanonicalChoice`] 다. 후보는
//!    뷰포트에 보인 형제만(수집 스크립트가 고른 것 포함 16개까지 보낸다. 예전 사건이 그보다 많이 보냈으면 고른 것의 순번 ±8 안에서 가까운 순)
//!    이고, 후보마다 뽑힐 확률은 1/n 이 아니라 순번 기저율 `π_k` 의 몫이다(세로 목록은 `(k+1)^−0.35` 에서 시작해 사이트의 경험치로
//!    고치고, 가로 탭 · 라디오는 고르게). 노출 · 뽑힌 수 · 기댓값 · 분산은 포아송-이항으로 쌓는다. 위쪽(선호)은 z ≥ +2 이고 세 번
//!    이상 뽑혀야, 아래쪽(외면)은 z ≤ −2 이고 기댓값이 2.5 이상이어야 한다(분산 하한은 두지 않는다: 열 개 목록에서 여덟 번 노출된
//!    압도적인 후보도 결정된다).
//! 6. **고른 뒤 마찰** ([`post_pick_windows`]): 고른 순간부터 다른 페이지로 가거나 다른 선택을 할 때까지 같은 페이지의 마찰
//!    (광클 · 다시 고침 · 스크롤 뒤집기 · 뒤로 · 떠남)을 고른 키의 몫으로 센다. 쿠폰 단추를 누른 뒤 쿠폰 코드 칸을 다시 고친 것은
//!    칸의 해시에 찍혀도 단추의 몫이다. 같은 페이지 다른 선택의 구간(없으면 사이트)과 견준 z 를 키마다 쌓는다.
//! 7. **교차 판정** ([`classify_choice`] → [`WebElementKind`]): 많이 골랐어도 고른 뒤 마찰이 크면(z ≥ +1) 또는 바로 떠나면(절반
//!    이상) 추천 경로(GoldenPath)가 아니라 함정(FrictionTrap, discount)이다(마찰 거부권). 덜 골랐어도 마찰이 없으면 결함 없는
//!    비인기 옵션(BenignIgnored, neutral)이라 discount 를 주지 않는다.
//! 8. **단계를 건너뛰는 퍼널** ([`FunnelDag`], [`SiteAnalysis::branch_summary`]): 장바구니를 거치지 않고 끝낸 세션(상세 → 바로 구매
//!    → 완료)은 단축 경로(FastTrackCourse)로 따로 세고 장바구니 이탈의 분모에 넣지 않는다. 같은 탭 묶음(`tab_group`)의 다른 탭이
//!    장바구니 · 제출까지 갔으면 닫힌 상품 상세 탭의 이탈은 비교 쇼핑이라 마찰로 세지 않는다([`SiteAnalysis::ingest_cohort`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use premium_discount_nav::{self as nav, BehaviorParams, Checkpoint, Episode, Event, FrictionBaselines, MarkType, Signal, StepKind, Welford};
use premium_discount_verdict::{Obs, Params, Tier, IMPLICIT};
use serde::{Deserialize, Serialize};

use crate::event::{Element, EventKind, WebEvent};
use crate::pug::{self, CanonicalChoice};
use crate::query::AnalyticDictionary;

/// 들어와서 첫 동작까지 이보다 오래 걸렸으면 찾는 데 걸린 시간(`Searched`)으로 센다
pub const SEARCH_MIN_MS: i64 = 1_000;
/// 머묾으로 세는 최소 시간 (tracker-logis-center 와 같은 500 ms)
pub const HOVER_MIN_MS: u32 = 500;
/// 라벨 글자 수
const LABEL_CHARS: usize = 40;
/// 페이지 종류 (사전 `analytic_page_filters` 의 키)
pub const GOODS_VIEW_KIND: &str = "goods_view";
pub const CART_KIND: &str = "cart";
pub const CHECKOUT_KIND: &str = "checkout";
pub const ORDER_COMPLETE_KIND: &str = "order_complete";
/// 같은 탭 묶음에서 다른 탭의 전환(장바구니 · 결제 · 제출)을 비교 쇼핑의 근거로 보는 시간
pub const COMPARISON_WINDOW_MS: i64 = 600_000;
/// 옵션만 보는 칸: 상품 상세에서 이것만 바꾸고 (광클 · 다시 고침 없이) 떠나면 이탈이 아니다
const OPTION_FIELDS: [&str; 5] = ["select", "select-one", "select-multiple", "radio", "checkbox"];

/// 요소의 체크포인트 해시 (페이지 + 셀렉터 틀). 조상 모양이 없거나 이상하면 None
pub fn element_of(page: &str, el: &Element) -> Option<String> {
    let selector = nav::selector_template(&el.chain)?;
    nav::element_hash(page, &selector)
}

fn label_of(el: &Element) -> String {
    pug::text_of(&el.pug).chars().take(LABEL_CHARS).collect()
}

/// 선택 대조의 키: 요소 해시 + 글. 같은 틀의 형제(목록의 카드)는 글로 갈린다. 조상 모양이 없으면 페이지 + 글
pub fn choice_key(page: &str, el: &Element) -> Option<String> {
    let text = pug::text_of(&el.pug).to_lowercase();
    let element = element_of(page, el).unwrap_or_else(|| page.to_string());
    if text.is_empty() && el.chain.is_empty() {
        return None;
    }
    nav::hash_id(&format!("choice/1|{element}|{text}"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceParams {
    /// z 띠: 이만큼 기댓값을 벗어나야 결정한다 (2.0 ≈ 한쪽 2.3 %)
    pub band: f64,
    /// 노출이 이만큼은 있어야 결정한다
    pub min_exposures: u32,
}

impl Default for ChoiceParams {
    fn default() -> Self {
        ChoiceParams { band: 2.0, min_exposures: 5 }
    }
}

/// 후보 하나의 선택 기록
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceTally {
    /// 골라진 수 (TARGET)
    pub picks: u32,
    /// 묶음에 보인 수 (TARGET + RELATED)
    pub exposures: u32,
    /// 아무 정보 없이 골랐을 때의 기댓값 Σ 1/n
    pub expected: f64,
    /// 그 분산 Σ (1/n)(1 − 1/n)
    pub variance: f64,
}

impl ChoiceTally {
    /// 지나친 수 (RELATED 로만 보인 수)
    pub fn passed_over(&self) -> u32 {
        self.exposures - self.picks
    }

    pub fn z(&self) -> Option<f64> {
        (self.variance > 0.0).then(|| (self.picks as f64 - self.expected) / self.variance.sqrt())
    }

    pub fn decide(&self, p: &ChoiceParams) -> Option<MarkType> {
        if self.exposures < p.min_exposures {
            return None;
        }
        match self.z()? {
            z if z >= p.band => Some(MarkType::Premium),
            z if z <= -p.band => Some(MarkType::Discount),
            _ => None,
        }
    }

    /// 기획서 규칙: 고른 것은 premium, 지나친 것은 discount 로 바로 세어 등급(2/3)을 낸 것. 비교용으로만 남긴다
    pub fn naive_tier(&self) -> Tier {
        nav::tier(self.picks, self.passed_over())
    }
}

/// 선택 대조 장부: 키 → 기록, 키 → 라벨 · 페이지
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceBook {
    pub items: BTreeMap<String, ChoiceTally>,
    pub labels: BTreeMap<String, String>,
    pub pages: BTreeMap<String, String>,
}

impl ChoiceBook {
    /// 선택 하나: `shown` 중에서 `chosen` 을 골랐다(`shown` 에 `chosen` 이 없으면 더한다, 같은 키는 하나로). 후보가 둘보다
    /// 적으면 대조가 아니라서 세지 않는다(false)
    pub fn observe(&mut self, chosen: &str, shown: &[&str]) -> bool {
        let mut set: Vec<&str> = Vec::with_capacity(shown.len() + 1);
        for k in std::iter::once(chosen).chain(shown.iter().copied()) {
            if !set.contains(&k) {
                set.push(k);
            }
        }
        if set.len() < 2 {
            return false;
        }
        let q = 1.0 / set.len() as f64;
        for k in &set {
            let t = self.items.entry(k.to_string()).or_default();
            t.exposures += 1;
            t.expected += q;
            t.variance += q * (1.0 - q);
            if *k == chosen {
                t.picks += 1;
            }
        }
        true
    }

    /// 누름 · 바꿈 사건의 TARGET vs RELATED. 다른 사건이거나 형제가 없으면 false
    pub fn observe_event(&mut self, e: &WebEvent) -> bool {
        if !matches!(e.kind, EventKind::Click | EventKind::Change) || e.rage || e.related.is_empty() {
            return false;
        }
        // 고른 것은 형제와 같은 단위(목록 항목)로 센다. 목록 항목이 따로 없으면 누른 요소
        let (Some(page), Some(target)) = (nav::page_hash(&e.href), e.item.as_ref().or(e.target.as_ref())) else {
            return false;
        };
        let Some(chosen) = choice_key(&page, target) else {
            return false;
        };
        let mut shown: Vec<String> = Vec::new();
        for r in &e.related {
            if let Some(k) = choice_key(&page, r) {
                self.labels.entry(k.clone()).or_insert_with(|| label_of(r));
                self.pages.entry(k.clone()).or_insert_with(|| page.clone());
                shown.push(k);
            }
        }
        self.labels.entry(chosen.clone()).or_insert_with(|| label_of(target));
        self.pages.entry(chosen.clone()).or_insert_with(|| page.clone());
        let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
        self.observe(&chosen, &refs)
    }
}

/// 세션 하나를 신호 · 체크포인트로 바꾼 것
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionTrace {
    pub sid: String,
    /// 페이지에 들어옴 · 요소를 다룸 (타임트라이얼 코스의 체크포인트, `at_ms` 는 세션 경과 시간)
    pub checkpoints: Vec<Checkpoint>,
    /// 마찰 신호 (`anchor` 는 요소 해시, 없으면 페이지 해시)
    pub events: Vec<Event>,
    /// 신호의 anchor → 페이지 해시
    pub page_of: HashMap<String, String>,
    /// 들어온 페이지 순서 (같은 페이지가 이어지면 하나)
    pub path: Vec<String>,
    /// 이탈(`Abandoned`)로 세지 않은 떠남 ([`AbandonPolicy`])
    pub exemptions: Vec<AbandonExemption>,
}

/// 바꾼 칸을 두고 떠난 것을 이탈 마찰(`Abandoned`)로 셀지
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbandonPolicy {
    /// 늘 센다 (예전 규칙, [`trace_session`])
    #[default]
    Always,
    /// 같은 탭 묶음의 다른 탭이 장바구니 · 결제 · 제출까지 갔다: 상품 상세에서 떠난 것은 비교 쇼핑이다
    ComparisonBrowse,
    /// 상품 상세에서 옵션 칸(select · radio · checkbox)만 보고 광클 · 다시 고침 없이 떠난 것은 옵션 확인이다
    /// (장바구니 · 결제 페이지에서 떠난 것은 그대로 이탈이다)
    ExploreAware,
}

/// 이탈로 세지 않은 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbandonExemption {
    /// 같은 탭 묶음의 다른 탭이 전환했다 (닫힌 비교 탭)
    ComparisonBrowse,
    /// 상품 상세에서 옵션만 확인했다
    ExploratoryOptionCheck,
}

fn abandon_exemption(policy: AbandonPolicy, href: &str, fields: &[String], rough: bool) -> Option<AbandonExemption> {
    if policy == AbandonPolicy::Always || AnalyticDictionary::builtin().page_kind_of(href).as_deref() != Some(GOODS_VIEW_KIND) {
        return None;
    }
    if policy == AbandonPolicy::ComparisonBrowse {
        return Some(AbandonExemption::ComparisonBrowse);
    }
    let options_only = !fields.is_empty() && fields.iter().all(|f| OPTION_FIELDS.contains(&f.as_str()));
    (options_only && !rough).then_some(AbandonExemption::ExploratoryOptionCheck)
}

/// 한 세션의 사건(같은 `sid`)을 순서대로 신호 · 체크포인트로 바꾼다
pub fn trace_session(events: &[WebEvent]) -> SessionTrace {
    trace_session_with(events, AbandonPolicy::Always)
}

/// [`trace_session`] 에 이탈 면제 규칙을 더한 것
pub fn trace_session_with(events: &[WebEvent], policy: AbandonPolicy) -> SessionTrace {
    let mut sorted: Vec<&WebEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.seq, e.t_ms));
    let mut out = SessionTrace { sid: sorted.first().map(|e| e.sid.clone()).unwrap_or_default(), ..Default::default() };
    let mut entered_at: Option<i64> = None;
    let mut acted = false;
    let mut changed: Vec<String> = Vec::new();
    let mut dirty: Vec<String> = Vec::new();
    // 이번 페이지에서 바꾼 칸의 종류, 광클 · 다시 고침이 있었나 (이탈 면제)
    let mut visit_fields: Vec<String> = Vec::new();
    let mut visit_rough = false;
    for e in sorted {
        let Some(page) = nav::page_hash(&e.href) else { continue };
        let element = e.target.as_ref().and_then(|t| element_of(&page, t));
        let anchor = element.clone().unwrap_or_else(|| page.clone());
        out.page_of.insert(anchor.clone(), page.clone());
        out.page_of.insert(page.clone(), page.clone());
        let mut push = |signal: Signal| out.events.push(Event { at_ms: e.t_ms, signal, anchor: Some(anchor.clone()) });
        if e.kind == EventKind::Enter || out.path.last() != Some(&page) {
            if out.path.last() != Some(&page) {
                out.path.push(page.clone());
                out.checkpoints.push(Checkpoint::new(e.t_ms, StepKind::Page, page.clone(), None));
                changed.clear();
                visit_fields.clear();
                visit_rough = false;
            }
            entered_at = Some(e.t_ms);
            acted = false;
        }
        let first_action = matches!(e.kind, EventKind::Click | EventKind::Change | EventKind::Submit) && !acted;
        if first_action {
            acted = true;
            if let Some(t0) = entered_at {
                let ms = e.t_ms - t0;
                if ms >= SEARCH_MIN_MS {
                    push(Signal::Searched { ms: ms.min(u32::MAX as i64) as u32 });
                }
            }
        }
        match e.kind {
            EventKind::Enter => {}
            EventKind::Click if e.rage => {
                visit_rough = true;
                push(Signal::RageClick)
            }
            EventKind::Click => {
                push(Signal::Picked);
                if let Some(h) = &element {
                    out.checkpoints.push(Checkpoint::new(e.t_ms, StepKind::Element, h.clone(), None));
                }
            }
            EventKind::Hover => {
                if e.dwell_ms.unwrap_or(0) >= HOVER_MIN_MS {
                    push(Signal::Hovered);
                }
            }
            EventKind::Change => {
                if changed.contains(&anchor) {
                    visit_rough = true;
                    push(Signal::Retried);
                } else {
                    changed.push(anchor.clone());
                    push(Signal::Engaged);
                }
                if !dirty.contains(&anchor) {
                    dirty.push(anchor.clone());
                }
                visit_fields.push(e.field.clone().unwrap_or_default().to_ascii_lowercase());
            }
            EventKind::Scroll => {
                let times = e.flips.unwrap_or(0);
                if times > 0 {
                    push(Signal::Rescrolled { times });
                }
            }
            EventKind::Submit => {
                push(Signal::Submitted);
                dirty.clear();
                // 제출은 되돌릴 수 없는 상태 변화다: 이 페이지의 마지막 체크포인트(제출 단추를 누름, 없으면 페이지)에 확정 표시를 해서
                // 코스를 다듬을 때 그 곁길이 접히지 않게 한다 (premium-discount-nav `Checkpoint::committed`)
                if let Some(c) = out.checkpoints.last_mut() {
                    c.committed = true;
                }
            }
            EventKind::Back => push(Signal::Backtracked),
            EventKind::Leave => {
                if e.dirty.unwrap_or(0) > 0 || !dirty.is_empty() {
                    match abandon_exemption(policy, &e.href, &visit_fields, visit_rough) {
                        Some(x) => out.exemptions.push(x),
                        None => push(Signal::Abandoned),
                    }
                }
                dirty.clear();
            }
        }
    }
    out
}

/// 페이지 · 요소 하나의 마찰 결정 수
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrictionTally {
    pub label: String,
    pub episodes: u32,
    pub premium: u32,
    pub discount: u32,
    pub friction_sum: f64,
}

impl FrictionTally {
    pub fn tier(&self) -> Tier {
        nav::tier(self.premium, self.discount)
    }

    pub fn mean_friction(&self) -> f64 {
        if self.episodes == 0 {
            0.0
        } else {
            self.friction_sum / self.episodes as f64
        }
    }

    fn add(&mut self, friction: f64, mark: Option<MarkType>) {
        self.episodes += 1;
        self.friction_sum += friction;
        match mark {
            Some(MarkType::Premium) => self.premium += 1,
            Some(MarkType::Discount) => self.discount += 1,
            None => {}
        }
    }
}

/// 퍼널 한 단계
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunnelStep {
    pub page: String,
    pub label: String,
    /// 앞 단계를 차례로 지나 이 단계에 닿은 세션 수
    pub reached: u32,
    /// 이 단계에 닿고 다음 단계로 가지 못한 세션 수 (마지막 단계는 0)
    pub dropped: u32,
}

/// 사이트 하나의 분석 (기기 안). 세션을 넣을 때마다 쌓인다
#[derive(Debug, Clone, Default)]
pub struct SiteAnalysis {
    pub host: String,
    pub params: BehaviorParams,
    pub choice_params: ChoiceParams,
    pub baselines: FrictionBaselines,
    pub choices: ChoiceBook,
    pub pages: BTreeMap<String, FrictionTally>,
    pub elements: BTreeMap<String, FrictionTally>,
    pub paths: Vec<Vec<String>>,
    pub sessions: u32,
    /// 위치 편향 · 가시 경쟁 풀을 고친 선택 대조 × 고른 뒤 마찰 (시나리오 5)
    pub debiased: DebiasedChoiceBook,
    /// 페이지 사이 전환 · 단축 경로 · 이탈 면제 (시나리오 5)
    pub dag: FunnelDag,
}

impl SiteAnalysis {
    pub fn new(host: &str) -> SiteAnalysis {
        SiteAnalysis { host: host.trim().to_ascii_lowercase(), ..Default::default() }
    }

    /// 세션 하나(같은 `sid` 의 사건)를 넣는다: 선택 대조, 에피소드 결정, 퍼널 경로. 이탈은 예전처럼 모두 센다
    pub fn add_session(&mut self, events: &[WebEvent]) -> Vec<(Episode, nav::Decision)> {
        self.add_session_with(events, AbandonPolicy::Always)
    }

    /// 여러 세션을 한꺼번에 넣는다: 같은 탭 묶음(`tab_group`)의 다른 탭이 10분 안에 장바구니 · 결제 · 제출까지 갔으면 이 세션의
    /// 상품 상세 이탈은 비교 쇼핑이라 세지 않고([`AbandonPolicy::ComparisonBrowse`]), 그 밖의 세션도 상품 상세에서 옵션만 보고
    /// 떠난 것은 세지 않는다([`AbandonPolicy::ExploreAware`]). 탭마다 `sid` 가 달라 세션 하나만 보고는 알 수 없는 일이다
    pub fn ingest_cohort(&mut self, sessions: &[Vec<WebEvent>]) {
        let infos: Vec<CohortInfo> = sessions.iter().map(|s| CohortInfo::of(s)).collect();
        for (i, events) in sessions.iter().enumerate() {
            let policy = if infos[i].sibling_converted(i, &infos) { AbandonPolicy::ComparisonBrowse } else { AbandonPolicy::ExploreAware };
            self.add_session_with(events, policy);
        }
    }

    /// [`SiteAnalysis::add_session`] 에 이탈 면제 규칙을 고를 수 있게 한 것
    pub fn add_session_with(&mut self, events: &[WebEvent], policy: AbandonPolicy) -> Vec<(Episode, nav::Decision)> {
        for e in events {
            self.choices.observe_event(e);
            self.debiased.observe_event(e);
        }
        for w in post_pick_windows(events, &self.params, policy) {
            self.debiased.observe_post_pick(&w.choice.stable_key, &w.page, w.friction, w.completed, w.abandoned, self.params.eps);
        }
        let labels = labels_of(events);
        let trace = trace_session_with(events, policy);
        self.dag.record(events, &trace);
        self.sessions += 1;
        if !trace.path.is_empty() {
            self.paths.push(trace.path.clone());
        }
        let page_of = |e: &Event| e.anchor.as_ref().and_then(|a| trace.page_of.get(a)).cloned();
        // 의미 거리 대신 페이지가 바뀌었는가 (같은 페이지 안의 간격은 덜 자른다)
        let episodes = nav::split_episodes(&trace.events, |a, b| if page_of(a) == page_of(b) { 0.25 } else { 1.0 }, &self.params);
        let mut out = Vec::new();
        for ep in episodes {
            let Some(page) = ep.events.last().and_then(|e| page_of(e)) else { continue };
            let region = ep.focus().filter(|a| *a != page).map(str::to_string);
            let d = self.baselines.decide(&self.host, &page, region.as_deref(), &ep, &self.params);
            let pt = self.pages.entry(page.clone()).or_default();
            if pt.label.is_empty() {
                pt.label = labels.get(&page).cloned().unwrap_or_default();
            }
            pt.add(d.friction, d.screen.mark);
            if let (Some(r), Some(j)) = (&region, &d.region) {
                let et = self.elements.entry(r.clone()).or_default();
                if et.label.is_empty() {
                    et.label = labels.get(r).cloned().unwrap_or_default();
                }
                et.add(d.friction, j.mark);
            }
            out.push((ep, d));
        }
        out
    }

    /// 페이지 순서 `steps` 의 퍼널. 세션 경로에서 단계를 차례로(사이에 다른 페이지가 있어도) 지났는지 본다
    pub fn funnel(&self, steps: &[String]) -> Vec<FunnelStep> {
        let mut reached = vec![0u32; steps.len()];
        for path in &self.paths {
            let mut at = 0usize;
            for page in path {
                if at < steps.len() && *page == steps[at] {
                    reached[at] += 1;
                    at += 1;
                }
            }
        }
        steps
            .iter()
            .enumerate()
            .map(|(i, page)| FunnelStep {
                page: page.clone(),
                label: self.pages.get(page).map(|p| p.label.clone()).unwrap_or_default(),
                reached: reached[i],
                dropped: if i + 1 < steps.len() { reached[i] - reached[i + 1] } else { 0 },
            })
            .collect()
    }

    /// 마찰이 큰(discount 쪽) 요소부터: 퍼널 병목 후보
    pub fn bottlenecks(&self) -> Vec<(&str, &FrictionTally)> {
        let mut v: Vec<(&str, &FrictionTally)> = self.elements.iter().map(|(k, t)| (k.as_str(), t)).filter(|(_, t)| t.discount > 0).collect();
        v.sort_by(|a, b| b.1.discount.cmp(&a.1.discount).then(b.1.mean_friction().total_cmp(&a.1.mean_friction())));
        v
    }

    /// 들어온 단계(`entry_page`: 페이지 해시 또는 페이지 종류, 예 `goods_view`)에서 끝 단계(`goal_page`, 예 `order_complete`)까지의
    /// 갈래. 장바구니를 거친 정석 경로와 거치지 않은 단축 경로(FastTrackCourse: 상세 → 바로 구매 → 완료)를 따로 세고, 장바구니
    /// 이탈의 분모는 장바구니에 들어간 세션뿐이다(순서 퍼널 [`SiteAnalysis::funnel`] 은 단축 경로 세션을 상품 단계의 이탈로 센다).
    /// 끝 단계가 주문 완료면 결제 페이지의 제출도 끝으로 본다(완료 페이지를 수집하지 못하는 결제 대행). 추천 경로(GoldenPath)와
    /// 1순위 병목(FrictionTrap, 없으면 DeadWeight)은 [`DebiasedChoiceBook`] 의 교차 판정이다
    pub fn branch_summary(&self, entry_page: &str, goal_page: &str) -> FunnelBranchSummary {
        let kind_is = |page: &str, want: &str| page == want || self.dag.kinds.get(page).is_some_and(|k| k == want);
        let mut out = FunnelBranchSummary { entry: entry_page.to_string(), goal: goal_page.to_string(), ..Default::default() };
        let (mut classic_ms, mut fast_ms): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
        let mut fast: BTreeMap<String, (String, u32, f64)> = BTreeMap::new();
        for s in &self.dag.sessions {
            let Some(start) = s.path.iter().position(|p| kind_is(p, entry_page)) else { continue };
            out.entered += 1;
            let page_goal = s.path.iter().enumerate().skip(start + 1).find(|(_, p)| kind_is(p, goal_page)).map(|(i, _)| (i, s.entered_ms[i]));
            let submit_goal = if goal_page == ORDER_COMPLETE_KIND {
                s.submits.iter().filter(|(i, _)| *i >= start && kind_is(&s.path[*i], CHECKOUT_KIND)).min().copied()
            } else {
                None
            };
            let goal = match (page_goal, submit_goal) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let cart_at = s.path.iter().enumerate().skip(start + 1).find(|(_, p)| kind_is(p, CART_KIND)).map(|(i, _)| i);
            let via_cart = match (cart_at, goal) {
                (Some(c), Some((g, _))) => c <= g,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if via_cart {
                out.cart_entered += 1;
            }
            let Some((_, goal_ms)) = goal else {
                if via_cart {
                    out.cart_dropped += 1;
                }
                continue;
            };
            let took = (goal_ms - s.entered_ms[start]).max(0) as f64;
            if via_cart {
                out.classic_completed += 1;
                classic_ms.push(took);
            } else {
                out.fast_track_completed += 1;
                fast_ms.push(took);
                // 단축 경로를 연 요소: 들어온 페이지에서 마지막으로 누른 것 (바로 구매 · 간편결제)
                if let Some((_, key, label)) = s.picks.iter().rev().find(|(at, _, _)| *at == start) {
                    let f = fast.entry(key.clone()).or_insert_with(|| (label.clone(), 0, 0.0));
                    f.1 += 1;
                    f.2 += took;
                }
            }
        }
        out.completed = out.classic_completed + out.fast_track_completed;
        out.classic_mean_ms = mean(&classic_ms);
        out.fast_track_mean_ms = mean(&fast_ms);
        let rows = self.debiased.diagnose_all();
        let golden: HashSet<&str> = rows.iter().filter(|r| r.kind == WebElementKind::GoldenPath).map(|r| r.stable_key.as_str()).collect();
        out.fast_track_elements = fast
            .into_iter()
            .map(|(key, (label, sessions, total))| FastTrackElement { golden: golden.contains(key.as_str()), stable_key: key, label, sessions, mean_ms: total / sessions.max(1) as f64 })
            .collect();
        out.fast_track_elements.sort_by(|a, b| b.sessions.cmp(&a.sessions).then(a.label.cmp(&b.label)));
        out.golden_paths = rows.iter().filter(|r| r.kind == WebElementKind::GoldenPath).cloned().collect();
        out.golden_paths.sort_by(|a, b| b.z_choice.unwrap_or(0.0).total_cmp(&a.z_choice.unwrap_or(0.0)));
        out.primary_bottleneck = primary_bottleneck(&rows);
        out.comparison_browse_exempt = self.dag.comparison_browse_exempt;
        out.exploratory_exempt = self.dag.exploratory_exempt;
        out
    }
}

/// 해시 → 사람이 읽는 라벨 (페이지는 URL 틀, 요소는 PUG 의 글)
fn labels_of(events: &[WebEvent]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for e in events {
        let Some(page) = nav::page_hash(&e.href) else { continue };
        out.entry(page.clone()).or_insert_with(|| nav::url_template(&e.href).unwrap_or_default());
        if let Some(t) = &e.target {
            if let Some(h) = element_of(&page, t) {
                out.entry(h).or_insert_with(|| label_of(t));
            }
        }
    }
    out
}

// ---- 시나리오 5: 가시 경쟁 풀 · 위치 편향을 고친 선택 대조 × 고른 뒤 마찰 ----

/// 슬롯 기저율을 사이트의 경험치로 고치기 시작하는 노출 합 (그 전에는 위치 감쇠 사전분포만 쓴다)
pub const SLOT_PRIOR_MIN_EXPOSURES: u64 = 50;
/// 분산이 이보다 작으면 z 를 내지 않는다
const MIN_VARIANCE: f64 = 1e-6;
/// 가로 묶음으로 보는 role (위치 감쇠를 쓰지 않는다)
const HORIZONTAL_ROLES: [&str; 5] = ["tab", "radio", "menuitemradio", "option", "switch"];

/// 위치 편향 · 가시 경쟁 풀 보정과 교차 판정의 매개변수
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebiasConfig {
    /// z 띠 (2σ)
    pub band: f64,
    /// 노출이 이만큼은 있어야 판정한다
    pub min_exposures: u32,
    /// 선호(z ≥ +띠)로 보려면 이만큼은 뽑혀야 한다 (분산이 작은 큰 목록의 안전망)
    pub min_picks_for_preferred: u32,
    /// 외면(z ≤ −띠)으로 보려면 기댓값이 이만큼은 돼야 한다 (기댓값이 작아 생기는 거짓 외면 막기)
    pub min_expected_for_passed: f64,
    /// 한 선택의 후보 상한 (고른 것 포함, 가시 뷰포트의 n_eff)
    pub max_competing_peers: usize,
    /// 세로 목록의 위치 감쇠 사전분포 `π_k = (k+1)^−α` (0 이면 고르게 1/n)
    pub position_decay_alpha: f64,
    /// 고른 뒤 마찰 z 가 이 이상이면 마찰 거부권 (함정 · 군더더기)
    pub trap_friction_z: f64,
    /// 고른 뒤 바로 떠난 비율이 이 이상이면 함정
    pub trap_abandon_rate: f64,
    /// 고른 뒤 마찰 z 가 이 이하이고 끝까지 갔으면 숨은 보석
    pub gem_friction_z: f64,
    /// 고른 뒤 마찰 z 의 기준선(같은 페이지 다른 선택, 모자라면 사이트) 표본 하한
    pub min_peer_windows: u64,
}

impl Default for DebiasConfig {
    fn default() -> Self {
        DebiasConfig {
            band: 2.0,
            min_exposures: 5,
            min_picks_for_preferred: 3,
            min_expected_for_passed: 2.5,
            max_competing_peers: 16,
            position_decay_alpha: 0.35,
            trap_friction_z: 1.0,
            trap_abandon_rate: 0.5,
            gem_friction_z: -1.0,
            min_peer_windows: 3,
        }
    }
}

/// 슬롯 하나의 선택 기록 (기저율 학습)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotStat {
    pub picks: u64,
    pub exposures: u64,
    /// 위치 감쇠 사전분포만으로 기대한 뽑힌 수 (경험치는 이것과 실제 뽑힌 수의 비다)
    pub prior_expected: f64,
}

/// 고른 뒤의 결과
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOutcome {
    /// 고른 뒤 구간 수
    pub picks: u64,
    /// 그 세션이 고른 뒤 제출까지 갔다
    pub completed: u64,
    /// 고르고 바로 떠났다 (같은 페이지에서 바꾼 칸을 두고 떠남, 또는 다른 동작 없이 떠남)
    pub abandoned: u64,
}

impl ChoiceOutcome {
    pub fn completion_rate(&self) -> Option<f64> {
        (self.picks > 0).then(|| self.completed as f64 / self.picks as f64)
    }

    pub fn abandon_rate(&self) -> Option<f64> {
        (self.picks > 0).then(|| self.abandoned as f64 / self.picks as f64)
    }
}

/// 선택 대조 × 고른 뒤 마찰의 교차 판정
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebElementKind {
    /// 많이 고르고(z ≥ +2) 고른 뒤 마찰이 낮다: 추천 경로 (premium, implicit +1)
    GoldenPath,
    /// 많이 골랐지만 고른 뒤 마찰이 크거나(z ≥ +1) 절반 이상 바로 떠났다: 마찰 거부권 (discount, 선호 +1 대신 implicit −1)
    FrictionTrap,
    /// 선택은 보통(|z| < 2)인데 고른 뒤 마찰이 낮고(z ≤ −1) 끝까지 갔다 (premium 쪽, implicit +1)
    HiddenGem,
    /// 덜 고르고(z ≤ −2) 고른 뒤 마찰도 크다 (discount, implicit −1)
    DeadWeight,
    /// 덜 골랐지만 고른 뒤 마찰이 없다: 결함 없는 비인기 옵션 (neutral, 관측을 내지 않는다)
    BenignIgnored,
    /// 노출이 모자라거나 띠 안이다 (판정 유보)
    UnratedCold,
}

impl WebElementKind {
    /// 내비게이션 번호 · 리포트의 등급. 숨은 보석의 premium 은 PD-Verdict 에서는 implicit 관측이 쌓여야(0.3 × 7 ≥ 2.0) 난다
    pub fn tier(self) -> Tier {
        match self {
            WebElementKind::GoldenPath | WebElementKind::HiddenGem => Tier::Premium,
            WebElementKind::FrictionTrap | WebElementKind::DeadWeight => Tier::Discount,
            WebElementKind::BenignIgnored => Tier::Neutral,
            WebElementKind::UnratedCold => Tier::Unrated,
        }
    }

    /// PD-Verdict web 트랙에 낼 implicit 관측의 부호. None 이면 내지 않는다(비인기 옵션이 discount 로 오염되지 않게)
    pub fn implicit_sign(self) -> Option<i32> {
        match self {
            WebElementKind::GoldenPath | WebElementKind::HiddenGem => Some(1),
            WebElementKind::FrictionTrap | WebElementKind::DeadWeight => Some(-1),
            WebElementKind::BenignIgnored | WebElementKind::UnratedCold => None,
        }
    }
}

/// 후보 하나의 진단
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosedChoiceRow {
    pub stable_key: String,
    pub label: String,
    pub page: String,
    pub kind: WebElementKind,
    pub tier: Tier,
    pub picks: u32,
    pub exposures: u32,
    pub expected: f64,
    pub variance: f64,
    /// 선택 대조 z (분산이 너무 작으면 None)
    pub z_choice: Option<f64>,
    /// 고른 뒤 마찰 z 의 평균 (구간이 없거나 기준선이 아직 없으면 None)
    pub post_friction_z: Option<f64>,
    pub post_windows: u64,
    pub completion_rate: Option<f64>,
    pub abandon_rate: Option<f64>,
}

impl DiagnosedChoiceRow {
    /// PD-Verdict web 트랙에 넣을 implicit 관측 (가중치는 레시피의 implicit 값, v2 는 0.3). 관측을 내지 않는 갈래는 None
    pub fn implicit_obs(&self, params: &Params, at_ms: i64) -> Option<Obs> {
        let sign = self.kind.implicit_sign()?;
        Some(Obs { kind: IMPLICIT.to_string(), sign, weight: params.weight(IMPLICIT), credit: 1.0, at_ms })
    }
}

/// 선택 대조 × 고른 뒤 마찰 교차 판정 (모듈 설명 7). `post_friction_z` 는 고른 뒤 마찰 z 의 평균이다(없으면 마찰이 없던 것으로 본다)
pub fn classify_choice(t: &ChoiceTally, post_friction_z: Option<f64>, outcome: &ChoiceOutcome, c: &DebiasConfig) -> WebElementKind {
    if t.exposures < c.min_exposures || t.variance <= MIN_VARIANCE {
        return WebElementKind::UnratedCold;
    }
    let z = (t.picks as f64 - t.expected) / t.variance.sqrt();
    let rough = post_friction_z.is_some_and(|f| f >= c.trap_friction_z);
    if z >= c.band && t.picks >= c.min_picks_for_preferred {
        let leaves = outcome.abandon_rate().is_some_and(|r| r >= c.trap_abandon_rate);
        return if rough || leaves { WebElementKind::FrictionTrap } else { WebElementKind::GoldenPath };
    }
    if z <= -c.band && t.expected >= c.min_expected_for_passed {
        return if rough { WebElementKind::DeadWeight } else { WebElementKind::BenignIgnored };
    }
    let smooth = post_friction_z.is_some_and(|f| f <= c.gem_friction_z);
    let finished = outcome.completion_rate().is_some_and(|r| r >= 0.5);
    if z.abs() < c.band && smooth && finished {
        return WebElementKind::HiddenGem;
    }
    WebElementKind::UnratedCold
}

fn prior_weight(slot: u32, alpha: f64) -> f64 {
    (slot as f64 + 1.0).powf(-alpha.max(0.0))
}

/// 요소의 선택 대조 키 (가변 문구를 뺀 것, [`crate::pug::canonical_choice_of`]). 셀렉터가 없으면 페이지 해시를 요소 해시로 쓴다
pub fn canonical_of(page: &str, el: &Element) -> CanonicalChoice {
    let element = element_of(page, el).unwrap_or_else(|| page.to_string());
    pug::canonical_choice_of(&element, &el.pug)
}

/// 목록 안 순번 (0부터): 같은 태그 형제 중 순서(`nth_of_type`). 조상 모양이 없으면 0
pub fn slot_of(el: &Element) -> u32 {
    el.chain.first().map_or(0, |n| n.nth_of_type.saturating_sub(1))
}

/// 후보로 셀 수 있는 요소 (글도 조상 모양도 없으면 무엇인지 모른다. [`choice_key`] 와 같은 기준)
fn identifiable(el: &Element) -> bool {
    !el.chain.is_empty() || !pug::text_of(&el.pug).is_empty()
}

/// 가로 묶음(탭 · 라디오 · 옵션): 위치 감쇠를 쓰지 않는다
fn horizontal_group(e: &WebEvent, el: &Element) -> bool {
    let node = el.chain.first();
    let role = node.is_some_and(|n| {
        n.attrs.iter().any(|(k, v)| {
            let v = v.trim().to_ascii_lowercase();
            (k.eq_ignore_ascii_case("role") && HORIZONTAL_ROLES.contains(&v.as_str())) || (k.eq_ignore_ascii_case("type") && v == "radio")
        })
    });
    role || node.is_some_and(|n| n.tag.eq_ignore_ascii_case("option")) || matches!(e.field.as_deref(), Some("radio" | "select" | "select-one"))
}

/// 선택 대조 사건: 누름 · 바꿈이고, 광클이 아니고, 형제가 있다
fn choice_event(e: &WebEvent) -> bool {
    matches!(e.kind, EventKind::Click | EventKind::Change) && !e.rage && !e.related.is_empty()
}

/// 위치 편향 · 가시 경쟁 풀을 고친 선택 대조 장부 × 고른 뒤 마찰 (모듈 설명 5 ~ 7). 예전 [`ChoiceBook`] 과 나란히 쌓는다
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DebiasedChoiceBook {
    pub config: DebiasConfig,
    /// stable_key → 포아송-이항 누적 (뽑힌 수, 노출, 기댓값 Σp, 분산 Σp(1−p))
    pub items: BTreeMap<String, ChoiceTally>,
    /// 슬롯(목록 안 순번, 0부터) → 기록. 세로 목록만 (사이트 경험 기저율)
    pub slot_priors: BTreeMap<u32, SlotStat>,
    /// stable_key → 슬롯 → 그 키의 몫. 슬롯 기저율을 볼 때 지금 견주는 후보들의 몫은 뺀다([`DebiasedChoiceBook::slot_weights`])
    pub slot_by_key: BTreeMap<String, BTreeMap<u32, SlotStat>>,
    /// stable_key → 고른 뒤 마찰 z 의 분포
    pub post_friction: BTreeMap<String, Welford>,
    /// stable_key → 고른 뒤 마찰 점수의 분포
    pub post_raw: BTreeMap<String, Welford>,
    /// 페이지 → 고른 뒤 마찰 점수의 분포 (같은 페이지 다른 선택의 기준선)
    pub page_post_raw: BTreeMap<String, Welford>,
    /// 사이트 전체의 고른 뒤 마찰 점수 분포 (페이지 기준선이 모자랄 때)
    pub site_post_raw: Welford,
    pub outcomes: BTreeMap<String, ChoiceOutcome>,
    pub labels: BTreeMap<String, String>,
    pub pages: BTreeMap<String, String>,
}

impl DebiasedChoiceBook {
    pub fn new(config: DebiasConfig) -> DebiasedChoiceBook {
        DebiasedChoiceBook { config, ..Default::default() }
    }

    /// 후보 묶음(키, 슬롯)의 슬롯 가중치 `π_k`. 가로 묶음은 모두 1(고르게)이다. 세로 목록은 위치 감쇠 사전분포 `(k+1)^−α` 이고,
    /// 노출 합이 [`SLOT_PRIOR_MIN_EXPOSURES`] 이상이면 사이트 경험치(그 슬롯의 실제 뽑힌 수 / 사전분포의 기대 수, 라플라스 평활 +1)를
    /// 곱한다. 경험치에서는 이 묶음의 후보들이 쌓은 몫을 뺀다: 늘 같은 자리에 있는 상품의 인기가 그 자리의 기저율로 새어 들어가 자기
    /// z 를 지우지 않게. 그래서 같은 슬롯의 후보는 늘 같은 가중치다
    pub fn slot_weights(&self, pool: &[(&str, u32)], horizontal: bool) -> Vec<f64> {
        if horizontal {
            return vec![1.0; pool.len()];
        }
        let total: u64 = self.slot_priors.values().map(|s| s.exposures).sum();
        pool.iter()
            .map(|(_, slot)| {
                let prior = prior_weight(*slot, self.config.position_decay_alpha);
                if total < SLOT_PRIOR_MIN_EXPOSURES {
                    return prior;
                }
                let all = self.slot_priors.get(slot).copied().unwrap_or_default();
                let (mut picks, mut expected) = (all.picks as f64, all.prior_expected);
                for (key, _) in pool {
                    if let Some(own) = self.slot_by_key.get(*key).and_then(|m| m.get(slot)) {
                        picks -= own.picks as f64;
                        expected -= own.prior_expected;
                    }
                }
                prior * (picks.max(0.0) + 1.0) / (expected.max(0.0) + 1.0)
            })
            .collect()
    }

    /// 선택 하나: `visible_peers`(뷰포트에 보인 형제, 순번과 함께) 중에서 `chosen` 을 골랐다. 같은 키는 하나로 센다. 후보가
    /// 상한보다 많으면(예전 사건의 무한 스크롤 수집본) 고른 것의 순번 ±(상한/2) 안에서 가까운 순으로 상한까지만 후보로 본다
    /// (보이지 않았을 아래쪽 카드에 거짓 외면을 쌓지 않는다). 후보가 둘보다 적으면 세지 않는다(false)
    pub fn observe_canonical(&mut self, page_hash: &str, chosen: (&CanonicalChoice, u32), visible_peers: &[(&CanonicalChoice, u32)], horizontal_menu: bool) -> bool {
        let (picked, picked_slot) = chosen;
        let mut pool: Vec<(&CanonicalChoice, u32)> = vec![(picked, picked_slot)];
        for (c, s) in visible_peers {
            if !pool.iter().any(|(q, _)| q.stable_key == c.stable_key) {
                pool.push((*c, *s));
            }
        }
        let cap = self.config.max_competing_peers.max(2);
        if pool.len() > cap {
            let half = (cap / 2) as u32;
            let mut peers: Vec<(&CanonicalChoice, u32)> = pool[1..].iter().copied().filter(|(_, s)| s.abs_diff(picked_slot) <= half).collect();
            peers.sort_by_key(|(_, s)| (s.abs_diff(picked_slot), *s));
            peers.truncate(cap - 1);
            pool.truncate(1);
            pool.extend(peers);
        }
        if pool.len() < 2 {
            return false;
        }
        let keyed: Vec<(&str, u32)> = pool.iter().map(|(c, s)| (c.stable_key.as_str(), *s)).collect();
        let weights = self.slot_weights(&keyed, horizontal_menu);
        let sum: f64 = weights.iter().sum();
        if !sum.is_finite() || sum <= 0.0 {
            return false;
        }
        let priors: Vec<f64> = pool.iter().map(|(_, s)| prior_weight(*s, self.config.position_decay_alpha)).collect();
        let prior_sum: f64 = priors.iter().sum();
        for (i, (c, slot)) in pool.iter().enumerate() {
            let p = weights[i] / sum;
            let hit = u32::from(i == 0);
            let t = self.items.entry(c.stable_key.clone()).or_default();
            t.exposures += 1;
            t.picks += hit;
            t.expected += p;
            t.variance += p * (1.0 - p);
            self.labels.entry(c.stable_key.clone()).or_insert_with(|| c.display_label.clone());
            self.pages.entry(c.stable_key.clone()).or_insert_with(|| page_hash.to_string());
            if !horizontal_menu {
                let p0 = priors[i] / prior_sum;
                for s in [self.slot_priors.entry(*slot).or_default(), self.slot_by_key.entry(c.stable_key.clone()).or_default().entry(*slot).or_default()] {
                    s.exposures += 1;
                    s.picks += u64::from(hit);
                    s.prior_expected += p0;
                }
            }
        }
        true
    }

    /// 누름 · 바꿈 사건의 고른 것(목록 항목, 없으면 누른 요소) vs 보인 형제(`related`). 다른 사건 · 광클이거나 형제가 없으면 false
    pub fn observe_event(&mut self, e: &WebEvent) -> bool {
        if !choice_event(e) {
            return false;
        }
        let (Some(page), Some(target)) = (nav::page_hash(&e.href), e.item.as_ref().or(e.target.as_ref())) else {
            return false;
        };
        if !identifiable(target) {
            return false;
        }
        let chosen = canonical_of(&page, target);
        let peers: Vec<(CanonicalChoice, u32)> = e.related.iter().filter(|r| identifiable(r)).map(|r| (canonical_of(&page, r), slot_of(r))).collect();
        let refs: Vec<(&CanonicalChoice, u32)> = peers.iter().map(|(c, s)| (c, *s)).collect();
        self.observe_canonical(&page, (&chosen, slot_of(target)), &refs, horizontal_group(e, target))
    }

    /// 고른 뒤 구간 하나 ([`post_pick_windows`]): 마찰 점수를 같은 페이지 다른 선택의 구간(모자라면 사이트 전체, 이 키의 몫은 뺌)과
    /// 견준 z 를 키에 쌓고, 완료 · 이탈을 센다. 기준선이 아직 모자라면 z 없이 결과만 센다. 쌓은 z 를 돌려준다
    pub fn observe_post_pick(&mut self, stable_key: &str, page_hash: &str, friction: f64, completed: bool, abandoned: bool, eps: f64) -> Option<f64> {
        let own = self.post_raw.get(stable_key).copied().unwrap_or_default();
        let page = self.page_post_raw.get(page_hash).copied().unwrap_or_default().without(&own);
        let site = self.site_post_raw.without(&own);
        let min = self.config.min_peer_windows.max(1);
        let base = [page, site].into_iter().find(|w| w.n >= min);
        let z = base.map(|w| (friction - w.mean) / (w.population_sd() + eps.max(1e-9)));
        if let Some(z) = z {
            self.post_friction.entry(stable_key.to_string()).or_default().push(z);
        }
        self.post_raw.entry(stable_key.to_string()).or_default().push(friction);
        self.page_post_raw.entry(page_hash.to_string()).or_default().push(friction);
        self.site_post_raw.push(friction);
        let o = self.outcomes.entry(stable_key.to_string()).or_default();
        o.picks += 1;
        o.completed += u64::from(completed);
        o.abandoned += u64::from(abandoned);
        z
    }

    /// 고른 뒤 구간 하나를 [`observe_post_pick`](Self::observe_post_pick) 으로 쌓는다. 확인 뒤 복귀(`peek`) 구간은 sign 0 이라
    /// 마찰 기준선에도 이탈 분모에도 넣지 않는다 (쌓지 않으면 None)
    pub fn observe_window(&mut self, w: &PostPick, eps: f64) -> Option<f64> {
        if w.peek {
            return None;
        }
        self.observe_post_pick(&w.choice.stable_key, &w.page, w.friction, w.completed, w.abandoned, eps)
    }

    /// 후보 하나의 교차 진단 (마찰 거부권 포함). 본 적 없는 키는 None
    pub fn diagnose(&self, stable_key: &str) -> Option<DiagnosedChoiceRow> {
        let t = self.items.get(stable_key)?;
        let post = self.post_friction.get(stable_key).copied().filter(|w| w.n > 0);
        let post_z = post.map(|w| w.mean);
        let outcome = self.outcomes.get(stable_key).copied().unwrap_or_default();
        let kind = classify_choice(t, post_z, &outcome, &self.config);
        Some(DiagnosedChoiceRow {
            stable_key: stable_key.to_string(),
            label: self.labels.get(stable_key).cloned().unwrap_or_default(),
            page: self.pages.get(stable_key).cloned().unwrap_or_default(),
            kind,
            tier: kind.tier(),
            picks: t.picks,
            exposures: t.exposures,
            expected: t.expected,
            variance: t.variance,
            z_choice: (t.variance > MIN_VARIANCE).then(|| (t.picks as f64 - t.expected) / t.variance.sqrt()),
            post_friction_z: post_z,
            post_windows: post.map_or(0, |w| w.n),
            completion_rate: outcome.completion_rate(),
            abandon_rate: outcome.abandon_rate(),
        })
    }

    /// 모든 후보의 진단 (키 순서)
    pub fn diagnose_all(&self) -> Vec<DiagnosedChoiceRow> {
        self.items.keys().filter_map(|k| self.diagnose(k)).collect()
    }
}

/// 고른 뒤 구간 하나 (PostPickFrictionTracker)
#[derive(Debug, Clone, PartialEq)]
pub struct PostPick {
    pub choice: CanonicalChoice,
    pub page: String,
    /// 구간의 마찰 점수 (찾는 데 걸린 시간 `Searched` 는 고르기 전의 일이라 뺀다)
    pub friction: f64,
    /// 그 세션이 고른 뒤 제출했다
    pub completed: bool,
    /// 고르고 바로 떠났다: 구간에 이탈(`Abandoned`)이 있거나, 다른 페이지 · 다른 선택 없이 제출하지 않고 떠나며 세션이 끝났다
    /// (이탈 면제 규칙을 쓰면 상품 상세를 그냥 닫은 것은 빼고)
    pub abandoned: bool,
    /// 확인하고 돌아옴(PeekAndReturn, [`PostPickContext::is_benign_peek_and_return`]): 다음 화면을 열어 첫 안내만 듣고 깨끗이
    /// 닫은 뒤 부모 화면에서 이어 간 구간. 마찰도 이탈도 아니며(sign 0) 이탈 분모에서 빠진다
    /// ([`DebiasedChoiceBook::observe_window`])
    pub peek: bool,
}

/// 고른 뒤 다음 화면에 다녀온 한 번의 맥락: 확인하고 돌아온 정상 행동인지 가린다
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostPickContext {
    /// 다음 화면에 머문 시간
    pub dwell_ms: u32,
    /// 읽어 주는 데 드는 시간 (`READING_MS_PER_CHAR` × 읽은 글자 수, 화면을 보는 사용자는 0)
    pub tts_budget_ms: u32,
    /// 다음 화면의 광클 · 다시 고침 수
    pub rage_or_retry_count: u32,
    /// Esc · Alt+←(뒤로 가기) 한 번으로 돌아왔다
    pub clean_dismiss_key: bool,
    /// 돌아온 뒤 부모 화면에서 다음 유효 동작을 했다
    pub parent_progressed: bool,
}

impl PostPickContext {
    /// 정상적인 확인 뒤 복귀인가: 머문 시간이 8초 + 읽는 시간 안이고, 광클 · 다시 고침이 없고, 한 번에 깨끗이 닫고, 부모 화면에서
    /// 이어 갔다. 참이면 sign 0 (마찰 · 이탈로 세지 않고 이탈 분모에서 뺀다). 기본값은 거짓이라 예전 판정과 같다
    #[inline]
    pub fn is_benign_peek_and_return(&self) -> bool {
        let limit_ms = 8_000u32.saturating_add(self.tts_budget_ms);
        self.dwell_ms <= limit_ms && self.rage_or_retry_count == 0 && self.clean_dismiss_key && self.parent_progressed
    }
}

/// 세션 하나의 고른 뒤 구간들: 선택 대조 사건마다 고른 순간부터 다른 페이지에 들어가거나 다른 선택을 할 때까지 같은 페이지에서
/// 이어진 사건을 고른 키의 몫으로 본다(인과 귀속). 마찰 신호가 입력 칸 · 페이지 해시에 찍혀도(쿠폰 단추 뒤 쿠폰 코드 칸을 다시 고침,
/// 오류 뒤 떠남) 고른 단추의 몫이 된다. `policy` 는 세션과 같은 이탈 면제 규칙이다
pub fn post_pick_windows(events: &[WebEvent], params: &BehaviorParams, policy: AbandonPolicy) -> Vec<PostPick> {
    let mut sorted: Vec<&WebEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.seq, e.t_ms));
    let mut out = Vec::new();
    for (i, e) in sorted.iter().enumerate() {
        if !choice_event(e) {
            continue;
        }
        let (Some(page), Some(chosen)) = (nav::page_hash(&e.href), e.item.as_ref().or(e.target.as_ref())) else { continue };
        if !identifiable(chosen) {
            continue;
        }
        let end = sorted
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, f)| nav::page_hash(&f.href).is_some_and(|p| p != page) || choice_event(f))
            .map_or(sorted.len(), |(j, _)| j);
        let window: Vec<WebEvent> = sorted[i + 1..end].iter().map(|f| (*f).clone()).collect();
        let trace = trace_session_with(&window, policy);
        let signals = Episode { events: trace.events.into_iter().filter(|s| !matches!(s.signal, Signal::Searched { .. })).collect() };
        let submitted_here = window.iter().any(|f| f.kind == EventKind::Submit);
        // 이탈 면제 규칙을 쓰는 묶음(ingest_cohort)에서는 상품 상세를 그냥 닫은 것도 이탈이 아니다 (비교 탭 · 옵션 확인)
        let exempt_page = policy != AbandonPolicy::Always && AnalyticDictionary::builtin().page_kind_of(&e.href).as_deref() == Some(GOODS_VIEW_KIND);
        let left_for_good = end == sorted.len() && window.iter().any(|f| f.kind == EventKind::Leave) && !submitted_here && !exempt_page;
        out.push(PostPick {
            choice: canonical_of(&page, chosen),
            page,
            friction: signals.friction(params),
            completed: sorted[i + 1..].iter().any(|f| f.kind == EventKind::Submit),
            abandoned: left_for_good || signals.events.iter().any(|s| matches!(s.signal, Signal::Abandoned)),
            peek: false,
        });
    }
    out
}

/// Phase 4 Part 3: 선택 직후 페이지 전환으로 인정하는 최대 전이 지연(ms).
pub const CROSS_BOUNDARY_TRANSITION_MS: i64 = 1_500;
/// Phase 4 Part 3: 도착 페이지 초기 정착 관찰 창(8초) 및 1-Hop 감쇠 계수(0.85).
pub const CROSS_BOUNDARY_BOUNCE_MS: i64 = 8_000;
pub const CROSS_BOUNDARY_HOP_DECAY: f64 = 0.85;

/// 화면·장면 경계를 넘는 1-Hop 역추적 창 (`post_pick_windows_v2`, Phase 4 Part 3).
///
/// 같은 페이지 내부 마찰에 더해, 선택 직후(`Δt <= 1,500ms`) 진입한 다음 페이지 `S_2` 에서
/// 정상 전진 조작이 일어나기 전 8초(`CROSS_BOUNDARY_BOUNCE_MS`) 이내에 즉시 뒤로가기(`Backtracked`) ·
/// 창 닫기 이탈(`Leave` / `Abandoned`) · 광클(`rage`)이 발생하면 `0.85 * F_arrival(S_2)` 를 출발지 선택지로 역귀속한다.
pub fn post_pick_windows_v2(events: &[WebEvent], params: &BehaviorParams, policy: AbandonPolicy) -> Vec<PostPick> {
    post_pick_windows_v2_with(events, params, policy, None)
}

/// [`post_pick_windows_v2`] 에 확인 뒤 복귀(PeekAndReturn) 면책을 켠 것. `peek_tts_budget_ms` 가 `Some(t)` 이면 다음 화면에서
/// 곧바로 돌아온 구간을 [`PostPickContext`] 로 보고(머문 시간, 읽는 시간 `t`, 광클 · 다시 고침, 뒤로 가기 한 번, 돌아온 뒤 부모
/// 화면의 다음 동작), 정상 행동이면 역귀속 마찰 · 이탈을 더하지 않고 [`PostPick::peek`] 를 켠다. 스크린리더 사용자는 첫 안내만
/// 듣고 닫는 일이 잦아 `t` 를 읽은 글자 × `READING_MS_PER_CHAR` 로 준다. `None` 이면 [`post_pick_windows_v2`] 와 같다
pub fn post_pick_windows_v2_with(events: &[WebEvent], params: &BehaviorParams, policy: AbandonPolicy, peek_tts_budget_ms: Option<u32>) -> Vec<PostPick> {
    let mut sorted: Vec<&WebEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.seq, e.t_ms));
    let mut out = Vec::new();
    for (i, e) in sorted.iter().enumerate() {
        if !choice_event(e) {
            continue;
        }
        let (Some(page), Some(chosen)) = (nav::page_hash(&e.href), e.item.as_ref().or(e.target.as_ref())) else { continue };
        if !identifiable(chosen) {
            continue;
        }
        let end = sorted
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, f)| nav::page_hash(&f.href).is_some_and(|p| p != page) || choice_event(f))
            .map_or(sorted.len(), |(j, _)| j);
        let window: Vec<WebEvent> = sorted[i + 1..end].iter().map(|f| (*f).clone()).collect();
        let trace = trace_session_with(&window, policy);
        let signals = Episode { events: trace.events.into_iter().filter(|s| !matches!(s.signal, Signal::Searched { .. })).collect() };
        let submitted_here = window.iter().any(|f| f.kind == EventKind::Submit);
        let exempt_page = policy != AbandonPolicy::Always && AnalyticDictionary::builtin().page_kind_of(&e.href).as_deref() == Some(GOODS_VIEW_KIND);
        let left_for_good = end == sorted.len() && window.iter().any(|f| f.kind == EventKind::Leave) && !submitted_here && !exempt_page;
        let mut friction = signals.friction(params);
        let mut abandoned = left_for_good || signals.events.iter().any(|s| matches!(s.signal, Signal::Abandoned));
        let mut peek = false;

        // 1-Hop 역추적: 직후 다른 페이지 S_2 로 전이된 경우 도착 초기 8초 이내의 즉발 역류·이탈·광클을 역귀속한다.
        if let Some(first_next) = sorted.get(end) {
            if let Some(next_page) = nav::page_hash(&first_next.href).filter(|p| p != &page) {
                let transition_dt = first_next.t_ms - e.t_ms;
                if (0..=CROSS_BOUNDARY_TRANSITION_MS).contains(&transition_dt) {
                    let arrive_t = first_next.t_ms;
                    let mut hop_events: Vec<WebEvent> = Vec::new();
                    let mut immediate_backtrack = false;
                    let mut immediate_leave = false;
                    let mut forward_progress = false;
                    // 돌아온 사건의 자리 (sorted 의 번호)
                    let mut returned_at: Option<usize> = None;
                    let window_ms = CROSS_BOUNDARY_BOUNCE_MS + i64::from(peek_tts_budget_ms.unwrap_or(0));
                    for (k, f) in sorted.iter().enumerate().skip(end) {
                        let dwell = f.t_ms - arrive_t;
                        if dwell > window_ms {
                            break;
                        }
                        let f_page = nav::page_hash(&f.href);
                        if f_page.as_deref() == Some(page.as_str()) {
                            // 면책을 켜면 읽는 시간만큼 늘린 창 안의 복귀도 즉시 복귀다 (끄면 예전 8초 창 그대로)
                            immediate_backtrack = dwell <= CROSS_BOUNDARY_BOUNCE_MS || peek_tts_budget_ms.is_some();
                            returned_at = Some(k);
                            break;
                        }
                        if dwell > CROSS_BOUNDARY_BOUNCE_MS {
                            continue;
                        }
                        if f_page.as_deref() != Some(next_page.as_str()) {
                            // 제3의 새 페이지로 정상 전진
                            forward_progress = true;
                            break;
                        }
                        if f.kind == EventKind::Submit {
                            forward_progress = true;
                            break;
                        }
                        if f.kind == EventKind::Leave {
                            immediate_leave = true;
                        }
                        hop_events.push((*f).clone());
                    }
                    let hop_trace = trace_session_with(&hop_events, AbandonPolicy::Always);
                    let hop_ep = Episode {
                        events: hop_trace.events.into_iter().filter(|s| !matches!(s.signal, Signal::Searched { .. })).collect(),
                    };
                    let mut arrival_friction = hop_ep.friction(params);
                    let has_hop_rage = hop_ep.events.iter().any(|s| matches!(s.signal, Signal::RageClick));
                    if immediate_backtrack && !forward_progress {
                        arrival_friction += params.backtracked;
                    }
                    if immediate_leave && !forward_progress && !exempt_page {
                        arrival_friction += params.abandoned;
                    }
                    let bounced = !forward_progress && (immediate_backtrack || (immediate_leave && !exempt_page) || has_hop_rage);
                    let benign = match (peek_tts_budget_ms, returned_at) {
                        (Some(tts), Some(k)) if !immediate_leave && !forward_progress => {
                            let back_now = sorted[k].kind == EventKind::Back;
                            let backs_before = hop_events.iter().filter(|f| f.kind == EventKind::Back).count();
                            let parent_progressed = sorted[k + 1..]
                                .iter()
                                .take_while(|g| nav::page_hash(&g.href).as_deref() == Some(page.as_str()))
                                .find(|g| g.rage || matches!(g.kind, EventKind::Leave | EventKind::Back | EventKind::Click | EventKind::Change | EventKind::Submit))
                                .is_some_and(|g| !g.rage && matches!(g.kind, EventKind::Click | EventKind::Change | EventKind::Submit));
                            let rage_or_retry = hop_ep.events.iter().filter(|s| matches!(s.signal, Signal::RageClick | Signal::Retried)).count();
                            PostPickContext {
                                dwell_ms: u32::try_from((sorted[k].t_ms - arrive_t).max(0)).unwrap_or(u32::MAX),
                                tts_budget_ms: tts,
                                rage_or_retry_count: rage_or_retry as u32,
                                clean_dismiss_key: back_now && backs_before == 0,
                                parent_progressed,
                            }
                            .is_benign_peek_and_return()
                        }
                        _ => false,
                    };
                    if benign {
                        peek = true;
                    } else if bounced {
                        friction += CROSS_BOUNDARY_HOP_DECAY * arrival_friction;
                        abandoned = true;
                    }
                }
            }
        }

        out.push(PostPick {
            choice: canonical_of(&page, chosen),
            page,
            friction,
            completed: sorted[i + 1..].iter().any(|f| f.kind == EventKind::Submit),
            abandoned,
            peek,
        });
    }
    out
}

/// 세션 하나의 갈래 기록 ([`FunnelDag`])
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DagSession {
    /// 지난 페이지 (같은 페이지가 이어지면 하나)
    pub path: Vec<String>,
    /// 각 페이지에 들어온 세션 경과 시간 (ms, `path` 와 같은 길이)
    pub entered_ms: Vec<i64>,
    /// 제출: (페이지 자리, 경과 시간)
    pub submits: Vec<(usize, i64)>,
    /// 누른 요소 (광클 제외): (페이지 자리, 선택 대조 키, 라벨)
    pub picks: Vec<(usize, String, String)>,
}

/// 단계를 건너뛰는 퍼널: 페이지 사이 전환 간선과 세션의 갈래 (모듈 설명 8)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunnelDag {
    /// 페이지 → 다음 페이지 → 세션 수 (한 세션의 같은 간선은 한 번)
    pub edges: BTreeMap<String, BTreeMap<String, u32>>,
    /// 페이지 → 페이지 종류 (사전 `analytic_page_filters`. 모달 가상 주소는 그 slug 의 종류)
    pub kinds: BTreeMap<String, String>,
    pub sessions: Vec<DagSession>,
    /// 이탈(`Abandoned`)로 센 떠남 수
    pub abandoned: u32,
    /// 비교 쇼핑 · 옵션 확인이라 이탈로 세지 않은 떠남 수
    pub comparison_browse_exempt: u32,
    pub exploratory_exempt: u32,
}

impl FunnelDag {
    /// 세션 하나를 넣는다 (`trace` 는 같은 사건을 [`trace_session_with`] 로 바꾼 것)
    pub fn record(&mut self, events: &[WebEvent], trace: &SessionTrace) {
        let dict = AnalyticDictionary::builtin();
        let mut sorted: Vec<&WebEvent> = events.iter().collect();
        sorted.sort_by_key(|e| (e.seq, e.t_ms));
        let mut s = DagSession::default();
        for e in sorted {
            let Some(page) = nav::page_hash(&e.href) else { continue };
            if s.path.last() != Some(&page) {
                if let Some(kind) = dict.page_kind_of(&e.href) {
                    self.kinds.entry(page.clone()).or_insert(kind);
                }
                s.path.push(page.clone());
                s.entered_ms.push(e.t_ms);
            }
            let at = s.path.len() - 1;
            match e.kind {
                EventKind::Submit => s.submits.push((at, e.t_ms)),
                EventKind::Click if !e.rage => {
                    if let Some(t) = e.target.as_ref().filter(|t| identifiable(t)) {
                        let c = canonical_of(&page, t);
                        s.picks.push((at, c.stable_key, c.display_label));
                    }
                }
                _ => {}
            }
        }
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        for w in s.path.windows(2) {
            if seen.insert((w[0].as_str(), w[1].as_str())) {
                *self.edges.entry(w[0].clone()).or_default().entry(w[1].clone()).or_default() += 1;
            }
        }
        for x in &trace.exemptions {
            match x {
                AbandonExemption::ComparisonBrowse => self.comparison_browse_exempt += 1,
                AbandonExemption::ExploratoryOptionCheck => self.exploratory_exempt += 1,
            }
        }
        self.abandoned += trace.events.iter().filter(|e| matches!(e.signal, Signal::Abandoned)).count() as u32;
        if !s.path.is_empty() {
            self.sessions.push(s);
        }
    }
}

/// 단축 경로를 연 요소 하나 (들어온 페이지에서 마지막으로 누른 것)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FastTrackElement {
    pub stable_key: String,
    pub label: String,
    /// 이 요소로 단축 경로를 끝낸 세션 수
    pub sessions: u32,
    /// 들어온 단계에서 끝 단계까지 걸린 평균 시간 (ms)
    pub mean_ms: f64,
    /// 선택 대조 교차 판정이 추천 경로(GoldenPath)인가
    pub golden: bool,
}

impl FastTrackElement {
    /// Phase 4 Part 3: 확정된 `FastTrackElement(golden == true)` 를 로컬 단축 고스트 스텝 열
    /// `[entry_page, fast_track_element, goal_page]` 로 합성한다.
    pub fn synthetic_ghost_steps(&self, entry_page_hash: &str, element_hash: &str, goal_page_hash: &str) -> Option<Vec<nav::StepRef>> {
        if !self.golden || self.sessions == 0 {
            return None;
        }
        Some(vec![
            nav::StepRef { kind: StepKind::Page, hash: entry_page_hash.to_string(), gesture: None },
            nav::StepRef { kind: StepKind::Element, hash: element_hash.to_string(), gesture: None },
            nav::StepRef { kind: StepKind::Page, hash: goal_page_hash.to_string(), gesture: None },
        ])
    }
}

/// 정석 경로 · 단축 경로 · 1순위 병목 요약 ([`SiteAnalysis::branch_summary`])
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunnelBranchSummary {
    pub entry: String,
    pub goal: String,
    /// 들어온 단계에 닿은 세션 수
    pub entered: u32,
    /// 끝 단계에 닿은 세션 수 (정석 + 단축)
    pub completed: u32,
    /// 장바구니를 거쳐 끝낸 세션 수
    pub classic_completed: u32,
    /// 장바구니를 거치지 않고 끝낸 세션 수 (FastTrackCourse)
    pub fast_track_completed: u32,
    /// 들어온 뒤 장바구니에 들어간 세션 수 (장바구니 이탈의 분모)
    pub cart_entered: u32,
    /// 장바구니에 들어갔다가 끝내지 못한 세션 수
    pub cart_dropped: u32,
    pub classic_mean_ms: Option<f64>,
    pub fast_track_mean_ms: Option<f64>,
    pub fast_track_elements: Vec<FastTrackElement>,
    /// 추천 경로 (z 가 큰 순)
    pub golden_paths: Vec<DiagnosedChoiceRow>,
    /// 1순위 병목: 마찰 거부권이 걸린 함정 중 고른 뒤 마찰이 가장 큰 것 (없으면 군더더기)
    pub primary_bottleneck: Option<DiagnosedChoiceRow>,
    pub comparison_browse_exempt: u32,
    pub exploratory_exempt: u32,
}

impl FunnelBranchSummary {
    /// 전환율 (끝낸 세션 / 들어온 세션). 들어온 세션이 없으면 None
    pub fn conversion_rate(&self) -> Option<f64> {
        (self.entered > 0).then(|| self.completed as f64 / self.entered as f64)
    }
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn primary_bottleneck(rows: &[DiagnosedChoiceRow]) -> Option<DiagnosedChoiceRow> {
    let worst = |kind: WebElementKind| {
        rows.iter()
            .filter(|r| r.kind == kind)
            .max_by(|a, b| a.post_friction_z.unwrap_or(0.0).total_cmp(&b.post_friction_z.unwrap_or(0.0)).then(a.picks.cmp(&b.picks)))
            .cloned()
    };
    worst(WebElementKind::FrictionTrap).or_else(|| worst(WebElementKind::DeadWeight))
}

/// 세션 하나의 탭 묶음 · 받은 시각 · 전환 (비교 쇼핑 면제를 고르는 데만 쓴다)
#[derive(Debug, Clone, Default)]
struct CohortInfo {
    tab_group: Option<String>,
    /// 받은 시각(epoch ms)의 처음 · 끝. 모르면 None
    span: Option<(i64, i64)>,
    /// 전환(장바구니 · 결제 · 완료 페이지에 들어감, 또는 제출): Some(받은 시각, 모르면 None)
    converted: Option<Option<i64>>,
}

impl CohortInfo {
    fn of(events: &[WebEvent]) -> CohortInfo {
        let dict = AnalyticDictionary::builtin();
        let tab_group = events.iter().find_map(|e| e.tab_group.clone().filter(|t| !t.trim().is_empty()));
        let times: Vec<i64> = events.iter().map(|e| e.at_ms).filter(|t| *t > 0).collect();
        let span = times.iter().min().zip(times.iter().max()).map(|(a, b)| (*a, *b));
        let mut sorted: Vec<&WebEvent> = events.iter().collect();
        sorted.sort_by_key(|e| (e.seq, e.t_ms));
        let converted = sorted
            .iter()
            .find(|e| e.kind == EventKind::Submit || matches!(dict.page_kind_of(&e.href).as_deref(), Some(CART_KIND | CHECKOUT_KIND | ORDER_COMPLETE_KIND)))
            .map(|e| (e.at_ms > 0).then_some(e.at_ms));
        CohortInfo { tab_group, span, converted }
    }

    /// 같은 탭 묶음의 다른 세션이 이 세션 앞뒤 10분 안에 전환했는가 (시각을 모르면 묶음만 본다)
    fn sibling_converted(&self, me: usize, all: &[CohortInfo]) -> bool {
        let Some(group) = &self.tab_group else { return false };
        all.iter().enumerate().any(|(j, o)| {
            j != me
                && o.tab_group.as_ref() == Some(group)
                && match (o.converted, self.span) {
                    (None, _) => false,
                    (Some(Some(at)), Some((start, end))) => at >= start - COMPARISON_WINDOW_MS && at <= end + COMPARISON_WINDOW_MS,
                    (Some(_), _) => true,
                }
        })
    }
}
