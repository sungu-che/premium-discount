//! 타임트라이얼 (`trial/1`): 사용자의 플로우를 레이싱 게임의 타임트라이얼처럼 기록하고, 다음에 같은 길을 갈 때 고스트로 안내한다
//!
//! | 레이싱 | 접근성 내비게이션 |
//! | --- | --- |
//! | 코스 | 한 앱 · 웹 안에서 시작부터 종료까지 지난 체크포인트의 순서 ([`course_hash`]). 앱은 화면(UIA 구조) · 영역, 웹은 페이지(링크값) · 요소(DOM 셀렉터)이고, 스와이프 · 스크롤이면 제스처가 붙는다 |
//! | 런(한 바퀴) | 코스를 한 번 달린 기록: 체크포인트마다 시작부터의 구간 시간과 그 에피소드의 SDS 결정 ([`Run`]) |
//! | 기록실 | 앱 · 웹마다 코스 10개 ([`Collection`]). 같은 코스를 다시 달리면 횟수(중복 카운트)가 는다 |
//! | 고스트 | 내 최고 기록, 다른 사용자의 최고 기록 · 평균. 진행 중에 다음 체크포인트와 앞섬 · 뒤짐을 알려 준다 ([`Collection::guide`]. 곁길 · 지름길 · 제자리 헤맴까지 보는 것은 [`Collection::guide_live`]) |
//!
//! - **id**: 웹의 `hashId(cc + pathname)` 처럼 앞의 id 에 다음 조각을 이어 붙여 한 번 해싱한다(구분자 없음).
//!   체크포인트 id = `hashId(<종류> + <해시> + <제스처>)` ([`checkpoint_id`]). 코스 id 는 체인이다: 첫 체크포인트에서
//!   `hashId(<앱 id> + <체크포인트 id>)`, 다음부터 `hashId(<이전 코스 id> + <체크포인트 id>)`, 결승의 값이 그 코스의 id 다
//!   ([`course_chain`], [`course_hash`]). 앞부분이 같은 코스끼리는 그 지점까지의 id 를 함께 갖고, 다른 체크포인트가 나오는
//!   순간 새 id 로 갈라지므로 같은 코스가 두 번 등록되지 않는다. 체크포인트마다 그 지점까지의 코스 id 가 붙는다(`Step::course`).
//! - 플로우 길이는 체크포인트 10개까지다([`MAX_STEPS`]). 체크포인트는 세 단계로 다듬어 코스로 만든다([`reduce_course_points`]).
//!   ① 잠깐 들렀다 돌아온 곁길(A → B → A)은 접는다. 상태를 확정한 체크포인트(`committed`: 폼 제출 · 결제 · 장착)가 든 곁길과
//!   [`MAX_DETOUR_LOOP_MS`] 보다 오래 머문 곁길은 일부러 들른 길로 보고 남긴다. 접어도 처음 도착한 시각을 그대로 두므로 헤맨 시간은
//!   다음 구간 시간에 남는다. ② 같은 자리에서 잇달아 반대로 되감은 스크롤은 주된 방향 하나로 합친다. ③ 그래도 10개보다 많으면
//!   시작과 결승은 두고, 중간은 중요한 것(상태 확정 · 따라간 고스트의 체크포인트 · 화면 · 페이지 · 오래 걸린 구간)부터 8개를 남긴다.
//!   구간 시간은 늘 진짜 시작부터 잰다(앞을 잘라 기록이 빨라지는 일이 없고, 시작이 남아 고스트 판정도 된다).
//! - 콜렉션은 앱 · 웹마다 코스 10개까지다([`MAX_COURSES`]). 새 코스가 11번째가 되면 가장 오래 달리지 않은 코스 자리를 덮어쓴다.
//! - 런의 종류(premium · discount)는 시간이 아니라 그 에피소드의 SDS 결정이다([`crate::FrictionBaselines::decide`]).
//!   코스의 등급은 결정이 난 런의 수로 정한다([`tier`]).
//! - 다른 사용자의 코스(고스트)를 따라 달려 같은 결승에 닿으면, 그 코스의 중간 체크포인트 중 따라간 것은 premium,
//!   고르지 않은(다른 길로 간) 것은 discount 다([`route_marks`]). 체크포인트마다의 이 결정이 영역 · 화면의 등급이 되고,
//!   번호 순서(premium 영역이 앞 번호)에 쓰인다. 결승이 다르면 목적이 다른 것이라 판정하지 않는다.
//! - 끝까지 간(성공한) 에피소드만 런이 된다. 벽시계 시각은 기기를 떠나지 않는다: 런에는 시작부터의 경과 시간(100 ms 단위)과
//!   결승 날짜만 있다. 개인정보 화면(`sensitive_screen`)은 체크포인트로 넣지 않는다. 개인정보 화면 잠금 · 본인인증으로 시계를 멈춘
//!   구간([`ReduceOptions::frozen_spans`])은 구간 시간 · 총 시간 · 곁길에 머문 시간에서 빼고, 결승 날짜는 실제 결승 시각으로 정한다.

use std::collections::BTreeMap;
use std::fmt;

use premium_discount_sds::matrix::line_tie_shape;
use premium_discount_sds::TieKind;
use premium_discount_verdict::{Tier, RECIPE_V2};
use serde::{Deserialize, Serialize};

use crate::gesture::{classify_stroke, Gesture, GestureParams, Stroke};
use crate::hash::hash_id;
use crate::mark::{normalized, MarkError, MarkType, MarkVector};

pub const TRIAL_SCHEMA: &str = "trial/1";
/// 플로우(코스) 하나의 체크포인트 수 상한
pub const MAX_STEPS: usize = 10;
/// 앱 · 웹마다 콜렉션에 두는 코스 수 상한
pub const MAX_COURSES: usize = 10;
/// 구간 시간의 단위. 이 단위로 반올림해 기기 · 사람마다의 미세한 시간 차이를 지운다
pub const SPLIT_UNIT_MS: u32 = 100;
/// 런 하나의 최대 길이 (에피소드는 5분 넘는 간격에서 잘리므로 10 체크포인트면 이 안에 든다)
pub const MAX_RUN_MS: u32 = 3_600_000;
/// 코스 등급을 매기려면 결정이 난 런이 이만큼 있어야 한다
pub const TIER_MIN_DECIDED: u32 = 3;
/// 곁길(A → B → A)을 접는 가장 긴 머문 시간. 이보다 오래 머문 곁길은 일부러 들른 것으로 보고 남긴다
pub const MAX_DETOUR_LOOP_MS: i64 = 60_000;
/// 같은 자리에서 이 시간 안에 잇달아 반대 방향으로 스크롤했으면 위치를 맞추려 되감은 것으로 보고 주된 방향 하나로 합친다
pub const SCROLL_REWIND_MS: i64 = 400;
/// 영역 · 요소를 고르는 획은 이만큼 밀어야 스와이프다. 스와이프 기준(50 px)을 겨우 넘긴 획은 누르기 직전의 손떨림으로 본다
pub const SELECTION_SWIPE_MIN_PX: f32 = 65.0;

const DAY_MS: i64 = 86_400_000;
/// 지름길 권하기: 공개 premium 코스의 평균 완주 시간이 내 최고 기록보다 이 몫 이상, 그리고 이 시간 이상 빨라야 한다
const UPGRADE_MIN_SHARE: f64 = 0.15;
const UPGRADE_MIN_MS: i64 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// 앱: 화면에 들어왔다 ([`crate::screen_hash`], UIA 구조 지문)
    Screen,
    /// 앱: 영역(번호)을 골랐다 ([`crate::region_hash`])
    Region,
    /// 웹: 페이지에 들어왔다. 링크값(URL 틀, 해시 라우팅 포함)으로 정한다 ([`crate::page_hash`])
    Page,
    /// 웹: 요소를 다뤘다. DOM 셀렉터 틀로 정한다 ([`crate::element_hash`], [`crate::selector_template`])
    Element,
}

impl StepKind {
    pub fn name(self) -> &'static str {
        match self {
            StepKind::Screen => "screen",
            StepKind::Region => "region",
            StepKind::Page => "page",
            StepKind::Element => "element",
        }
    }
}

/// 누름(클릭 · 탭)은 기본 동작이라 체크포인트에 붙이지 않는다. 그래서 마우스로 누른 사람과 손가락으로 누른 사람이 같은 코스가 된다
fn significant(gesture: Option<Gesture>) -> Option<Gesture> {
    gesture.filter(|g| *g != Gesture::Press)
}

/// 코스의 체크포인트 하나. 스와이프 · 스크롤로 지났으면 `gesture` 가 붙는다(같은 해시라도 누름과 다른 체크포인트다)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepRef {
    pub kind: StepKind,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<Gesture>,
}

impl StepRef {
    /// 사람이 읽는 체크포인트 이름: `<종류>:<해시>` 또는 `<종류>:<해시>@<제스처>` (같은 체크포인트인지 견줄 때 쓴다)
    pub fn key(&self) -> String {
        let hash = self.hash.trim().to_ascii_lowercase();
        match significant(self.gesture) {
            Some(g) => format!("{}:{hash}@{}", self.kind.name(), g.name()),
            None => format!("{}:{hash}", self.kind.name()),
        }
    }

    /// 체크포인트 id ([`checkpoint_id`])
    pub fn id(&self) -> Option<String> {
        checkpoint_id(self)
    }
}

/// 체크포인트 id = `hashId(<종류> + <해시> + <제스처>)` (예: `region0x2222…`, `page0xaaaa…scroll-down`). 해시가 0x + 40자로
/// 길이가 정해져 있어 구분자 없이 이어도 겹치지 않는다. 해시가 앱 · 사이트를 담고 있으므로 다른 앱의 체크포인트와도 겹치지 않는다.
/// 고스트 따라가기 판정의 수(nav_counts)도 이 id 로 센다. 서버도 같은 글로 계산한다
pub fn checkpoint_id(step: &StepRef) -> Option<String> {
    let gesture = significant(step.gesture).map_or("", Gesture::name);
    hash_id(&format!("{}{}{gesture}", step.kind.name(), step.hash.trim().to_ascii_lowercase()))
}

/// 기기 안의 체크포인트: 화면 · 페이지에 들어왔거나, 영역 · 요소를 다뤘다. `at_ms` 는 기기 시계이고 기기를 떠나지 않는다.
/// `gesture` 는 [`crate::classify_stroke`] · [`crate::scroll_bursts`] 로 가른 것이다(없으면 이동 · 누름).
/// `committed` 는 폼 제출 · 결제 · 장착 확정처럼 되돌릴 수 없는 상태 변화를 함께 했다는 표시다. 이 체크포인트가 든 곁길은 접지 않는다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub at_ms: i64,
    pub kind: StepKind,
    pub hash: String,
    #[serde(default)]
    pub gesture: Option<Gesture>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub committed: bool,
}

impl Checkpoint {
    /// 상태 확정 표시가 없는 체크포인트
    pub fn new(at_ms: i64, kind: StepKind, hash: impl Into<String>, gesture: Option<Gesture>) -> Checkpoint {
        Checkpoint {
            at_ms,
            kind,
            hash: hash.into(),
            gesture,
            committed: false,
        }
    }
}

/// 런의 체크포인트: 시작부터의 구간 시간과, 있으면 그 화면 · 영역의 벡터(라벨 글 granite · 영역 조각 비전)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// 체크포인트 id ([`checkpoint_id`]). 서버가 다시 계산해 맞춰 본다
    pub id: String,
    /// 시작부터 이 체크포인트까지의 코스 id ([`course_chain`]). 마지막 체크포인트의 값이 런의 코스 id 다
    pub course: String,
    pub kind: StepKind,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<Gesture>,
    pub split_ms: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vectors: Vec<MarkVector>,
}

/// 코스를 한 번 달린 기록. 기기에 쌓고, 인증한 뒤에만 서버로 올린다(`POST /nav/trials`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub schema: String,
    /// 앱 · 웹 해시 ([`crate::app_hash`])
    pub app: String,
    /// 코스 해시 ([`course_hash`]). 서버는 체크포인트로 다시 계산해 맞춰 본다
    pub course: String,
    pub steps: Vec<Step>,
    /// 결승까지 걸린 시간 (= 마지막 체크포인트의 구간 시간)
    pub total_ms: u32,
    /// 이 런(에피소드)의 SDS 결정. 띠 안이거나 기준선이 없으면 None
    #[serde(rename = "type")]
    pub kind: Option<MarkType>,
    /// 결승 날짜 (UTC, 1970-01-01 부터 센 날)
    pub day: i64,
    pub recipe: String,
    /// 이 런이 따라간 다른 사용자의 코스(고스트, 공개 코스의 해시). 서버는 이 코스와 견줘 체크포인트를 판정한다([`route_marks`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghost: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialError {
    /// 앱 · 체크포인트 해시가 `0x` + hex 40자가 아니다
    Hash,
    /// 체크포인트가 둘(시작 · 결승)보다 적다
    TooShort,
    /// 체크포인트가 시간순이 아니다
    Order,
    /// [`MAX_RUN_MS`] 를 넘는다
    TooLong,
}

impl fmt::Display for TrialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrialError::Hash => "app and checkpoint hashes must be addresses (0x + 40 hex)",
            TrialError::TooShort => "a run needs a start and a finish checkpoint",
            TrialError::Order => "checkpoints must be in time order",
            TrialError::TooLong => "a run must finish within an hour",
        })
    }
}

impl std::error::Error for TrialError {}

fn address(hash: &str) -> Option<String> {
    let hash = hash.trim().to_ascii_lowercase();
    (hash.len() == 42 && hash.starts_with("0x") && hash[2..].chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

fn round_split(ms: i64) -> u32 {
    let unit = SPLIT_UNIT_MS as i64;
    (((ms.max(0) + unit / 2) / unit) * unit) as u32
}

/// `[from, to]` 와 겹치는 동결 구간(개인정보 화면 잠금 · 본인인증)의 합 (ms). 구간은 순서가 섞이거나 서로 겹쳐 와도 되고(겹친 곳은
/// 한 번만 센다), 끝이 시작보다 앞서거나 같은 구간은 버린다
fn frozen_within(from: i64, to: i64, spans: &[(i64, i64)]) -> i64 {
    // 개인정보 잠금의 동결 구간을 만드는 쪽(resume)과 같은 셈을 쓴다: 겹친 구간은 한 번만 센다
    crate::resume::frozen_overlap_ms(from, to, spans)
}

/// `start` 부터 `at` 까지의 유효 경과 시간: 기기 시계의 차이에서 그 사이의 동결 구간을 뺀 것. `at` 이 늦을수록 줄지 않는다
fn effective(start: i64, at: i64, spans: &[(i64, i64)]) -> i64 {
    (at - start) - frozen_within(start, at, spans)
}

/// 코스 id 체인: 체크포인트마다 그 지점까지의 코스 id. 첫 값은 `hashId(<앱 id> + <체크포인트 id>)`, 다음부터는
/// `hashId(<이전 코스 id> + <체크포인트 id>)` 다. 앞부분이 같은 코스는 그 지점까지 같은 id 를 갖고, 체크포인트 하나 · 순서 하나만
/// 달라도 그 뒤로는 다른 id 가 된다. 서버(`workers/www/src/navtrials.ts`)도 같은 글로 다시 계산한다
pub fn course_chain(app: &str, steps: &[StepRef]) -> Option<Vec<String>> {
    let mut previous = app.trim().to_ascii_lowercase();
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        previous = hash_id(&format!("{previous}{}", checkpoint_id(step)?))?;
        out.push(previous.clone());
    }
    Some(out)
}

/// 코스 id: 코스 id 체인([`course_chain`])의 마지막 값(결승까지의 코스 id). 서버는 이 id 가 없으면 새 코스를 만든다
pub fn course_hash(app: &str, steps: &[StepRef]) -> Option<String> {
    course_chain(app, steps)?.pop()
}

/// 코스 다듬기의 선택 ([`reduce_course_points`], [`Run::from_checkpoints_with`], [`Collection::guide_live`])
#[derive(Debug, Clone, Copy)]
pub struct ReduceOptions<'a> {
    /// 따라가는(또는 견줄) 고스트의 체크포인트. 중간을 솎을 때 먼저 남겨 고스트 판정([`route_marks`])이 볼 체크포인트를 지킨다.
    /// [`Collection::guide_live`] 에서는 이 체크포인트를 모두 가진 공개 코스를 계속 따라간다(곁길에 들어서도 고스트가 바뀌지 않는다)
    pub ghost_steps: &'a [StepRef],
    /// 시계를 멈춘 구간 `(start_ms, end_ms)` (기기 시계): 개인정보 화면 잠금 · 본인인증. 구간 시간 · 총 시간 · 한 시간 상한 · 곁길에
    /// 머문 시간에서 뺀다. 겹치거나 순서가 섞여 와도 된다. 아직 끝나지 않은 잠금은 끝을 지금 시각으로 넘긴다
    pub frozen_spans: &'a [(i64, i64)],
    /// 이보다 오래 머문 곁길은 접지 않는다 (기본 [`MAX_DETOUR_LOOP_MS`])
    pub max_detour_loop_ms: i64,
}

impl Default for ReduceOptions<'_> {
    fn default() -> Self {
        ReduceOptions {
            ghost_steps: &[],
            frozen_spans: &[],
            max_detour_loop_ms: MAX_DETOUR_LOOP_MS,
        }
    }
}

/// 다듬는 중의 체크포인트: 처음 도착한 기기 시각, 체크포인트, 상태 확정 표시
#[derive(Debug, Clone)]
struct Point {
    at: i64,
    step: StepRef,
    committed: bool,
}

/// 시간순 · 해시를 확인하고, 되감은 스크롤을 합치고(Stage 2), 잇달아 같은 체크포인트를 처음 도착한 것 하나로 합치고, 곁길을
/// 접는다(Stage 1). `finish` 면 마지막 체크포인트를 결승으로 본다: 결승이 시작으로 돌아온 왕복은 접지 않는다(접으면 런이 없어진다)
fn canonical(checkpoints: &[Checkpoint], opts: &ReduceOptions<'_>, finish: bool) -> Result<Vec<Point>, TrialError> {
    let mut points: Vec<Point> = Vec::with_capacity(checkpoints.len());
    let mut prev = i64::MIN;
    for c in checkpoints {
        if c.at_ms < prev {
            return Err(TrialError::Order);
        }
        prev = c.at_ms;
        points.push(Point {
            at: c.at_ms,
            step: StepRef {
                kind: c.kind,
                hash: address(&c.hash).ok_or(TrialError::Hash)?,
                gesture: significant(c.gesture),
            },
            committed: c.committed,
        });
    }
    unwind_scrolls(&mut points);
    let mut deduped: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        match deduped.last_mut() {
            Some(last) if last.step == p.step => last.committed |= p.committed,
            _ => deduped.push(p),
        }
    }
    Ok(erase_loops(deduped, opts, finish))
}

/// 스크롤 제스처의 (세로 축인가, 아래 · 오른쪽인가)
fn scroll_axis(gesture: Option<Gesture>) -> Option<(bool, bool)> {
    match gesture? {
        Gesture::ScrollDown => Some((true, true)),
        Gesture::ScrollUp => Some((true, false)),
        Gesture::ScrollRight => Some((false, true)),
        Gesture::ScrollLeft => Some((false, false)),
        _ => None,
    }
}

/// Stage 2: 되감은 스크롤 합치기. 같은 자리(종류 · 해시)에서 [`SCROLL_REWIND_MS`] 안에 잇달아 찍힌 같은 축의 스크롤이 방향을
/// 뒤집었으면(아래로 내린 뒤 위치를 맞추려 살짝 올림) 모두 주된 방향 하나로 바꾼다. 체크포인트에는 움직인 거리가 없어서 더 많이
/// 찍힌 방향을 주된 방향으로 보고, 같으면 먼저 찍힌 방향이다. 바꾼 체크포인트는 다음 단계에서 하나로 합쳐진다
fn unwind_scrolls(points: &mut [Point]) {
    let mut i = 0;
    while i < points.len() {
        let Some((vertical, _)) = scroll_axis(points[i].step.gesture) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < points.len()
            && points[j].step.kind == points[i].step.kind
            && points[j].step.hash == points[i].step.hash
            && scroll_axis(points[j].step.gesture).is_some_and(|(v, _)| v == vertical)
            && points[j].at - points[j - 1].at <= SCROLL_REWIND_MS
        {
            j += 1;
        }
        let first = points[i].step.gesture;
        let same = points[i..j].iter().filter(|p| p.step.gesture == first).count();
        let main = if same * 2 < j - i {
            points[i..j].iter().map(|p| p.step.gesture).find(|g| *g != first).unwrap_or(first)
        } else {
            first
        };
        for p in &mut points[i..j] {
            p.step.gesture = main;
        }
        i = j;
    }
}

/// Stage 1: 곁길 접기 (스택으로 걷는 loop-erased walk). 지금 체크포인트를 이미 지나왔으면 그 사이(곁길)를 접는다. 곁길에 상태를
/// 확정한 체크포인트가 있거나 곁길에 머문 유효 시간이 `max_detour_loop_ms` 를 넘으면 일부러 들른 길로 보고 남긴다. 접을 때 앞
/// 체크포인트의 처음 도착 시각을 그대로 두므로, 곁길에서 쓴 시간은 다음 체크포인트의 구간 시간에 남는다(헤맨 런이 빨라지지 않는다)
fn erase_loops(points: Vec<Point>, opts: &ReduceOptions<'_>, finish: bool) -> Vec<Point> {
    let last = points.len().saturating_sub(1);
    let mut stack: Vec<Point> = Vec::with_capacity(points.len());
    for (i, p) in points.into_iter().enumerate() {
        if let Some(at) = stack.iter().rposition(|q| q.step == p.step) {
            let detour = &stack[at + 1..];
            let dwell = detour.first().map_or(0, |d| effective(d.at, p.at, opts.frozen_spans));
            let round_trip = finish && i == last && at == 0;
            if !round_trip && dwell <= opts.max_detour_loop_ms && !detour.iter().any(|q| q.committed) {
                stack.truncate(at + 1);
                stack[at].committed |= p.committed;
                continue;
            }
        }
        stack.push(p);
    }
    stack
}

/// Stage 3: 그래도 [`MAX_STEPS`] 개보다 많으면 시작과 결승은 그대로 두고, 중간은 중요도가 높은 것부터 [`MAX_STEPS`] − 2 개를 시간순으로
/// 남긴다. 중요도 = 4 · 상태 확정 + 3 · 고스트의 체크포인트 + 2 · 화면 · 페이지 + min(1.5, 5 · 그 체크포인트까지의 구간 시간 / 총 시간).
/// 같으면 결승에 가까운 것을 남긴다. 남길 때 양옆(이미 남긴 것 중 가장 가까운 앞 · 뒤)과 같은 체크포인트면 건너뛴다: 서버는 같은
/// 체크포인트가 잇달아 오는 런을 받지 않는다. 뒤에 남긴 것이 사이를 벌려 줄 수 있으니 더 남길 것이 없을 때까지 다시 훑는다
fn thin(points: Vec<Point>, opts: &ReduceOptions<'_>) -> Vec<Point> {
    if points.len() <= MAX_STEPS {
        return points;
    }
    let last = points.len() - 1;
    let elapsed: Vec<i64> = points.iter().map(|p| effective(points[0].at, p.at, opts.frozen_spans)).collect();
    let total = elapsed[last].max(1) as f64;
    let flag = |on: bool| if on { 1.0 } else { 0.0 };
    let mut ranked: Vec<(usize, f64)> = (1..last)
        .map(|i| {
            let p = &points[i];
            let share = (elapsed[i] - elapsed[i - 1]) as f64 / total;
            let weight = 4.0 * flag(p.committed) + 3.0 * flag(opts.ghost_steps.contains(&p.step)) + 2.0 * flag(matches!(p.step.kind, StepKind::Screen | StepKind::Page)) + (5.0 * share).min(1.5);
            (i, weight)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[last] = true;
    let mut kept = 0;
    loop {
        let before = kept;
        for &(i, _) in &ranked {
            if kept == MAX_STEPS - 2 {
                break;
            }
            if keep[i] {
                continue;
            }
            let prev = (0..i).rev().find(|&j| keep[j]).unwrap_or(0);
            let next = (i + 1..=last).find(|&j| keep[j]).unwrap_or(last);
            if points[prev].step == points[i].step || points[next].step == points[i].step {
                continue;
            }
            keep[i] = true;
            kept += 1;
        }
        if kept == before || kept == MAX_STEPS - 2 {
            break;
        }
    }
    points.into_iter().zip(keep).filter_map(|(p, k)| k.then_some(p)).collect()
}

/// 체크포인트를 코스로 다듬는다: 시간순 · 해시 확인, 되감은 스크롤 합치기, 곁길 접기, 시작 · 결승을 둔 중요도 솎기.
/// 결과는 [`MAX_STEPS`] 개 이하의 체크포인트와, 진짜 시작부터의 유효 경과 시간(ms, 동결 구간을 뺀 것, 첫 값은 0, 반올림 전)이다
pub fn reduce_course_points(checkpoints: &[Checkpoint], opts: &ReduceOptions<'_>) -> Result<Vec<(i64, StepRef)>, TrialError> {
    let points = thin(canonical(checkpoints, opts, true)?, opts);
    let start = points.first().map_or(0, |p| p.at);
    Ok(points.into_iter().map(|p| (effective(start, p.at, opts.frozen_spans), p.step)).collect())
}

/// 영역 · 요소를 고른 획의 제스처 ([`crate::classify_stroke`] 대신 쓴다). 스와이프 기준을 겨우 넘긴 짧은 획(시작에서 끝까지
/// [`SELECTION_SWIPE_MIN_PX`] 보다 짧은 것)은 누르기 직전의 손떨림으로 보고 누름으로 바꾼다. 그래서 마우스 · 손가락 · 펜으로 같은
/// 요소를 고른 사람이 같은 코스가 된다. 체크포인트에는 움직인 거리가 없어서 이 정규화는 획을 가르는 쪽에서 한다
pub fn selection_gesture(stroke: &Stroke, p: &GestureParams) -> Option<Gesture> {
    let gesture = classify_stroke(stroke, p)?;
    let swipe = matches!(gesture, Gesture::SwipeLeft | Gesture::SwipeRight | Gesture::SwipeUp | Gesture::SwipeDown);
    let travel = match (stroke.samples.first(), stroke.samples.last()) {
        (Some(a), Some(b)) => (b.x - a.x).hypot(b.y - a.y),
        _ => 0.0,
    };
    Some(if swipe && travel < SELECTION_SWIPE_MIN_PX { Gesture::Press } else { gesture })
}

impl Run {
    /// 성공한 에피소드의 체크포인트로 런을 만든다. `kind` 는 그 에피소드의 SDS 결정이다 (선택은 기본값, [`Run::from_checkpoints_with`])
    pub fn from_checkpoints(app: &str, checkpoints: &[Checkpoint], kind: Option<MarkType>) -> Result<Run, TrialError> {
        Run::from_checkpoints_with(app, checkpoints, kind, &ReduceOptions::default())
    }

    /// [`Run::from_checkpoints`] 에 선택을 더한 것: 따라간 고스트의 체크포인트(솎을 때 먼저 남긴다), 시계를 멈춘 구간(구간 시간 ·
    /// 총 시간 · 한 시간 상한에서 뺀다), 곁길을 접는 시간. 결승 날짜는 동결 구간을 빼지 않은 실제 결승 시각으로 정한다
    pub fn from_checkpoints_with(app: &str, checkpoints: &[Checkpoint], kind: Option<MarkType>, opts: &ReduceOptions<'_>) -> Result<Run, TrialError> {
        let app = address(app).ok_or(TrialError::Hash)?;
        let points = thin(canonical(checkpoints, opts, true)?, opts);
        let (Some(start), Some(finish)) = (points.first().map(|p| p.at), points.last().map(|p| p.at)) else {
            return Err(TrialError::TooShort);
        };
        if points.len() < 2 {
            return Err(TrialError::TooShort);
        }
        let elapsed: Vec<i64> = points.iter().map(|p| effective(start, p.at, opts.frozen_spans)).collect();
        if effective(start, finish, opts.frozen_spans) > MAX_RUN_MS as i64 {
            return Err(TrialError::TooLong);
        }
        let refs: Vec<StepRef> = points.into_iter().map(|p| p.step).collect();
        let chain = course_chain(&app, &refs).ok_or(TrialError::Hash)?;
        let steps: Vec<Step> = refs
            .iter()
            .zip(&elapsed)
            .zip(&chain)
            .map(|((s, ms), course)| Step {
                id: s.id().unwrap_or_default(),
                course: course.clone(),
                kind: s.kind,
                hash: s.hash.clone(),
                gesture: s.gesture,
                split_ms: round_split(*ms),
                vectors: Vec::new(),
            })
            .collect();
        Ok(Run {
            schema: TRIAL_SCHEMA.to_string(),
            course: chain.last().cloned().ok_or(TrialError::TooShort)?,
            app,
            total_ms: steps.last().map_or(0, |s| s.split_ms),
            steps,
            kind,
            day: finish.div_euclid(DAY_MS),
            recipe: RECIPE_V2.to_string(),
            ghost: None,
        })
    }

    /// 체크포인트 하나에 벡터를 붙인다 (L2 정규화). 공간 이름이 비었거나 겹치면 [`MarkError::Space`]
    pub fn attach_vectors(&mut self, step: usize, vectors: &[(&str, &[f32])]) -> Result<(), MarkError> {
        let target = self.steps.get_mut(step).ok_or(MarkError::Vector)?;
        let mut out: Vec<MarkVector> = Vec::with_capacity(vectors.len());
        for (space, vector) in vectors {
            let space = space.trim();
            if space.is_empty() || out.iter().any(|v| v.space == space) {
                return Err(MarkError::Space);
            }
            out.push(MarkVector {
                space: space.to_string(),
                vector: normalized(vector).ok_or(MarkError::Vector)?,
            });
        }
        target.vectors = out;
        Ok(())
    }

    pub fn refs(&self) -> Vec<StepRef> {
        self.steps
            .iter()
            .map(|s| StepRef {
                kind: s.kind,
                hash: s.hash.clone(),
                gesture: significant(s.gesture),
            })
            .collect()
    }

    pub fn splits(&self) -> Vec<u32> {
        self.steps.iter().map(|s| s.split_ms).collect()
    }
}

/// 코스의 등급: 결정이 난 런(premium + discount)이 [`TIER_MIN_DECIDED`] 개보다 적으면 Unrated, premium 이 2/3 이상이면
/// Premium, discount 가 2/3 이상이면 Discount, 그 사이는 Neutral. 서버도 같은 규칙이다(정수 비교).
/// 공식 판정(PD-Verdict v2 의 nav 트랙, 피어 순위 포함)이 서버에 생기면 그 값을 쓴다
pub fn tier(premium: u32, discount: u32) -> Tier {
    let decided = premium + discount;
    if decided < TIER_MIN_DECIDED {
        Tier::Unrated
    } else if premium * 3 >= decided * 2 {
        Tier::Premium
    } else if discount * 3 >= decided * 2 {
        Tier::Discount
    } else {
        Tier::Neutral
    }
}

/// 체크포인트 판정 결과 4-상태 (Phase 4 Part 1 `route_marks 2.0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkOutcome {
    /// 검증된 순항 통과 (+1)
    Premium,
    /// 쾌적·정속/고속 우회로 기존 스텝이 불필요함을 증명 (-1)
    Discount,
    /// 동일 `(kind, hash)` 를 다른 입력 모달리티(예: 스와이프 ↔ 클릭)로 통과 — 감점 금지 (0)
    EquivalentPass,
    /// 둔속·고마찰 우회 또는 국소 병목 억지 돌파 — 표 오염 방지 기권 (0)
    Abstain,
}

impl MarkOutcome {
    pub fn sign(self) -> Option<i32> {
        match self {
            MarkOutcome::Premium => Some(1),
            MarkOutcome::Discount => Some(-1),
            MarkOutcome::EquivalentPass | MarkOutcome::Abstain => None,
        }
    }
}

/// Phase 4 Part 1 `route_marks_v2` 평가 문맥.
#[derive(Debug, Clone)]
pub struct RouteEvalContext<'a> {
    pub run_total_ms: u32,
    pub run_kind: Option<MarkType>,
    pub ghost_avg_ms: u32,
    pub run_splits: &'a [u32],
    pub ghost_splits: &'a [u32],
    /// 비트 `i` 가 켜져 있으면 런의 `i` 번째 스텝에서 국소 고마찰 병목(`z >= 1.0`, FC-05)이 발생했다.
    pub fault_steps_mask: u16,
    /// 비트 `i` 가 켜져 있으면 사전 경고된 함정(`WarnedTrap`, FC-06)으로 긍정 보상을 차단한다.
    pub warned_trap_mask: u16,
    /// 10스텝 솎아내기에서 생략됐으나 실제로 통과한 고스트 체크포인트 증인 목록.
    pub passed_witness: &'a [StepRef],
    /// 우회 Discount 성립을 위한 속도 허용 비율 (기본 0.05 = +5%).
    pub pace_eps: f64,
    /// 반올림 지터(100ms)를 감안한 최소 속도 데드밴드 (기본 500ms).
    pub pace_deadband_ms: u32,
    /// Phase 7 Part 4 개인 기저 속도비 `ρ_u >= 1.0` (기본 1.0).
    pub user_pace_ratio: f64,
}

impl Default for RouteEvalContext<'_> {
    fn default() -> Self {
        Self {
            run_total_ms: 0,
            run_kind: None,
            ghost_avg_ms: 0,
            run_splits: &[],
            ghost_splits: &[],
            fault_steps_mask: 0,
            warned_trap_mask: 0,
            passed_witness: &[],
            pace_eps: 0.05,
            pace_deadband_ms: 500,
            user_pace_ratio: 1.0,
        }
    }
}

fn same_base_step(a: &StepRef, b: &StepRef) -> bool {
    a.kind == b.kind && a.hash.eq_ignore_ascii_case(&b.hash)
}

/// 오염 방지 `route_marks 2.0` (Phase 4 Part 1 + Phase 7 Part 4 개인 속도비 정규화).
///
/// * 시작과 결승이 고스트와 다르면 `None` (모달리티만 다른 동일 기점/결승도 허용하지 않고 기존 계약처럼 일치 요구).
/// * 정확 통과: 런 무마찰 또는 `fault_steps_mask` 순항 보호 시 `Premium(+1)`, 병목/경고 함정/역순 +25% 지연 시 `Abstain(0)`.
/// * 등가 모달리티 통과(`(kind, hash)` 일치, `gesture` 만 상이): `EquivalentPass(0)` 로 상호 감점 차단.
/// * 미통과 우회: `run_kind != Discount` 이고 개인 속도 정규화 완주 시간 `T_run / ρ_u <= max(T_ghost * (1 + pace_eps), T_ghost + pace_deadband_ms)` 일 때만 `Discount(-1)`, 둔속·고마찰 우회는 `Abstain(0)`.
pub fn route_marks_v2(run: &[StepRef], ghost: &[StepRef], ctx: &RouteEvalContext<'_>) -> Option<Vec<(StepRef, MarkOutcome)>> {
    if run.first()? != ghost.first()? || run.last()? != ghost.last()? {
        return None;
    }
    let pace_ratio = crate::portfolio::PaceScale::from_rho_u(ctx.user_pace_ratio).get();
    let norm_total = ctx.run_total_ms as f64 / pace_ratio;
    let avg = ctx.ghost_avg_ms as f64;
    let pace_ceiling = (avg * (1.0 + ctx.pace_eps.max(0.0))).max(avg + ctx.pace_deadband_ms as f64);
    let fast_enough_for_bypass = ctx.ghost_avg_ms == 0 || norm_total <= pace_ceiling;
    let clean_for_bypass = ctx.run_kind != Some(MarkType::Discount) && fast_enough_for_bypass;

    let mut out: Vec<(StepRef, MarkOutcome)> = Vec::new();
    let mut last_matched_run_idx: Option<usize> = None;

    for (gi, step) in ghost.iter().enumerate().skip(1).take(ghost.len().saturating_sub(2)) {
        if out.iter().any(|(s, _)| s == step) {
            continue;
        }
        if let Some(ri) = run.iter().position(|s| s == step) {
            let bit = if ri < 16 { 1u16 << ri } else { 0 };
            let step_faulted = (ctx.fault_steps_mask & bit) != 0;
            let step_warned = (ctx.warned_trap_mask & bit) != 0;
            let out_of_order = last_matched_run_idx.is_some_and(|prev| ri < prev);
            let out_of_order_slow = out_of_order && {
                let r_seg = ctx.run_splits.get(ri).copied().unwrap_or(0) as f64 / pace_ratio;
                let g_seg = ctx.ghost_splits.get(gi).copied().unwrap_or(0) as f64;
                g_seg > 0.0 && r_seg > g_seg * 1.25
            };
            // `fault_steps_mask` 가 제공되면 순항 스텝은 전체 런이 discount 여도 보호(FC-05); 미제공 시 전체 discount 런은 병목 세탁 차단.
            let run_level_blocked = ctx.run_kind == Some(MarkType::Discount) && ctx.fault_steps_mask == 0;
            let outcome = if step_faulted || step_warned || run_level_blocked || out_of_order_slow {
                MarkOutcome::Abstain
            } else {
                MarkOutcome::Premium
            };
            last_matched_run_idx = Some(last_matched_run_idx.map_or(ri, |prev| prev.max(ri)));
            out.push((step.clone(), outcome));
        } else if ctx.passed_witness.contains(step) {
            let outcome = if ctx.run_kind == Some(MarkType::Discount) && ctx.fault_steps_mask == 0 {
                MarkOutcome::Abstain
            } else {
                MarkOutcome::Premium
            };
            out.push((step.clone(), outcome));
        } else if run.iter().any(|s| same_base_step(s, step)) {
            out.push((step.clone(), MarkOutcome::EquivalentPass));
        } else {
            let outcome = if clean_for_bypass { MarkOutcome::Discount } else { MarkOutcome::Abstain };
            out.push((step.clone(), outcome));
        }
    }
    Some(out)
}

/// 다른 사용자의 코스(고스트)를 따라 달린 런의 체크포인트 판정. 시작과 결승이 고스트와 같을 때만 판정한다(같은 목적지로 가는
/// 길의 비교). 고스트의 중간 체크포인트(시작 · 결승을 뺀 것)마다, 런이 그 체크포인트를 지났으면 premium, 지나지 않았으면
/// (다른 길로 갔으면) discount 다. 런에만 있는 체크포인트는 판정하지 않는다. 시작이나 결승이 다르면 None.
/// 서버(`workers/www/src/navtrials.ts` 의 routeMarks)도 같은 규칙이다
pub fn route_marks(run: &[StepRef], ghost: &[StepRef]) -> Option<Vec<(StepRef, MarkType)>> {
    if run.first()? != ghost.first()? || run.last()? != ghost.last()? {
        return None;
    }
    let mut out: Vec<(StepRef, MarkType)> = Vec::new();
    for step in ghost.iter().skip(1).take(ghost.len().saturating_sub(2)) {
        if out.iter().any(|(s, _)| s == step) {
            continue;
        }
        let kind = if run.contains(step) { MarkType::Premium } else { MarkType::Discount };
        out.push((step.clone(), kind));
    }
    Some(out)
}

/// 체크포인트 하나의 판정 수 (다른 사용자들이 그 체크포인트를 따라갔는가 · 비켜 갔는가)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointTally {
    pub premium: u32,
    pub discount: u32,
}

impl CheckpointTally {
    pub fn tier(&self) -> Tier {
        tier(self.premium, self.discount)
    }
}

/// 서버 PD-Verdict v2 (`nav` 트랙) 판정 요약 (Phase 5 Part 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictSummary {
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
    #[serde(default)]
    pub evidence: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default = "default_nav_recipe")]
    pub recipe: String,
}

fn default_nav_recipe() -> String {
    RECIPE_V2.to_string()
}

/// Phase 7 Part 4: 코스 물리 조작 요구 특성 비트 (신체·장애 정보 수집 0%, 코스 구조만으로 결정론적 산출).
pub const TRAIT_NO_FINE_GESTURE: u16 = 1 << 0;
pub const TRAIT_TREE_ACCESSIBLE: u16 = 1 << 1;
pub const TRAIT_LOW_DENSITY_CALM: u16 = 1 << 2;

/// 콜렉션의 코스 하나 (내 기록)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CourseEntry {
    pub course: String,
    pub steps: Vec<StepRef>,
    /// 달린 횟수 (중복 카운트)
    pub runs: u32,
    pub premium: u32,
    pub discount: u32,
    /// 최고 기록의 구간 시간
    pub best: Vec<u32>,
    /// 마지막 런의 구간 시간
    pub last: Vec<u32>,
    pub last_day: i64,
    /// 덮어쓸 순서: 클수록 최근에 달렸다
    pub touched: u64,
    /// 사용자가 [▲] 핀으로 고정한 주력 코스 (Pinned-SLRU 퇴출 면제, 최대 3개, Phase 5 Part 2)
    #[serde(default)]
    pub pinned: bool,
}

impl CourseEntry {
    pub fn best_ms(&self) -> u32 {
        self.best.last().copied().unwrap_or(0)
    }

    pub fn tier(&self) -> Tier {
        tier(self.premium, self.discount)
    }
}

/// 다른 사용자의 기록 (서버 `GET /nav/trials` 의 `others`). 같은 코스를 달린 런이 공개 하한을 넘은 코스만 온다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicCourse {
    pub course: String,
    pub steps: Vec<StepRef>,
    /// 모든 사용자의 달린 횟수 (계정마다 하루 한 번으로 셈)
    pub runs: u32,
    pub premium: u32,
    pub discount: u32,
    /// 최고 기록의 구간 시간
    pub record: Vec<u32>,
    /// 평균 구간 시간 (100 ms 단위)
    pub average: Vec<u32>,
    /// 체크포인트마다의 판정 수 (`steps` 와 같은 순서, [`route_marks`] 로 쌓인 것)
    #[serde(default)]
    pub checkpoints: Vec<CheckpointTally>,
    /// 서버 PD-Verdict v2 (`nav` 트랙) 공식 판정 요약 (있으면 2/3 비율 대신 우선 사용, Phase 5 Part 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<VerdictSummary>,
    /// 코스 물리 조작 요구 특성 비트마스크 (`TRAIT_*`, Phase 7 Part 4)
    #[serde(default)]
    pub interaction_traits: u16,
}

/// 런을 콜렉션에 넣은 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// 새 코스
    New,
    /// 이미 있던 코스를 다시 달렸다. `best` 면 최고 기록을 갈았다
    Repeat { best: bool },
    /// 새 코스가 11번째라 가장 오래 달리지 않은 코스(`evicted`)를 덮어썼다
    Replaced { evicted: String },
}

/// 진행 중인 런의 안내
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub course: String,
    /// 지나온 체크포인트 수 (코스 앞부분과 같은 수)
    pub passed: usize,
    /// 코스의 체크포인트 수
    pub length: usize,
    /// 다음 체크포인트. None 이면 결승선을 지났다. 앱은 이 해시를 지금 화면의 번호로 바꿔 보여 준다
    pub next: Option<StepRef>,
    /// 다음 체크포인트의 등급: 다른 사용자들이 그 체크포인트를 따라갔는가(premium) · 비켜 갔는가(discount)
    pub next_tier: Tier,
    /// 지금 체크포인트에서의 차이(ms, + 면 뒤짐): 내 최고 기록 · 다른 사용자 최고 기록 · 다른 사용자 평균
    pub vs_best: Option<i64>,
    pub vs_record: Option<i64>,
    pub vs_average: Option<i64>,
    /// 내가 달린 횟수, 모든 사용자가 달린 횟수(공개된 코스만)
    pub my_runs: u32,
    pub all_runs: Option<u32>,
    /// 공개된 코스면 모든 사용자의 결정으로, 아니면 내 결정으로
    pub tier: Tier,
}

/// 진행 중인 런과 안내하는 코스(고스트) 사이의 맞춤 ([`LiveGuide::sync_state`])
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhostSyncState {
    /// 시작부터 지금까지 코스의 앞부분과 똑같다
    ExactPrefix,
    /// 시작은 같지만 지금 체크포인트가 코스에 없다(곁길 · 지름길을 가는 중). 고스트는 버리지 않는다. `last_matched_index` 는 마지막으로
    /// 지난 코스 체크포인트의 자리, `rejoin_hint` 는 그 뒤로 아직 지나지 않은 첫 체크포인트(다시 들어설 곳)다
    Detouring { last_matched_index: usize, rejoin_hint: Option<StepRef> },
    /// 중간 체크포인트를 건너뛰었거나 곁길을 돌아 코스의 뒤쪽 체크포인트(`ghost_index`)에 다시 들어섰다. 구간 시간은 그 자리에서
    /// 다시 견준다. `skipped_steps` 는 지나지 않은 코스의 중간 체크포인트 수다
    Rejoined { ghost_index: usize, skipped_steps: usize },
}

/// 진행 중인 런의 실시간 안내 ([`Collection::guide_live`]). 내 기록과 다른 사용자의 기록을 따로 맞춘다(서로 다른 코스일 수 있다)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveGuide {
    /// [`Guide`] 와 같은 칸. `course` · `passed` · `length` · `next` · `next_tier` · `tier` 는 안내하는 코스(지름길을 권하거나, 내 코스가
    /// 없거나, 내 코스에서는 곁길인데 공개 코스와는 맞으면 그 공개 코스, 아니면 내 코스)의 것이고, `vs_best` · `my_runs` 는 내 코스,
    /// `vs_record` · `vs_average` · `all_runs` 는 따라가는 공개 코스의 것이다. 곁길에서는 마지막으로 지난 코스 체크포인트에서 견준
    /// 차이를 그대로 둔다
    pub base: Guide,
    /// 내 최고 기록을 가져온 내 코스
    pub my_best_course: Option<String>,
    /// 다른 사용자의 최고 · 평균을 가져온 공개 코스. 결승하면 이 값을 런의 `ghost` 로 보낸다(서버가 [`route_marks`] 로 판정한다)
    pub tracked_public_ghost: Option<String>,
    /// 안내하는 코스와의 맞춤
    pub sync_state: GhostSyncState,
    /// 지금 시각의 유효 경과 시간이 안내하는 코스의 다음 체크포인트 도착 예정(공개 코스면 평균, 내 코스뿐이면 내 최고)을 넘긴 시간
    /// (100 ms 단위로 내림). 제자리에서 헤매면 체크포인트를 밟지 않아도 바로 늘어난다
    pub live_overrun_ms: u32,
    /// 내 습관 코스와 다음 갈림길이 다른 공개 premium 코스가 평균으로 내 최고 기록보다 15 % 이상 · 1.5초 이상 빠르면 줄일 시간.
    /// 이때 `base.next` 는 그 공개 코스의 다음 체크포인트다
    pub upgrade_over_habit_ms: Option<u32>,
    /// 다음 갈림길이 두 길로 팽팽하면(코스 효용 점수가 [`TieKind::PairConfusion`]) 안내하지 않은 쪽의 다음 체크포인트와 그 코스의 등급
    pub alt_next: Option<(StepRef, Tier)>,
}

fn tier_score(tier: Tier) -> f64 {
    match tier {
        Tier::Premium => 2.0,
        Tier::Discount => -3.0,
        Tier::Neutral | Tier::Unrated => 0.0,
    }
}

/// 100 · 등급 점수 + 10 · ln(1 + 달린 횟수) − 완주 시간(초)
fn utility(tier: Tier, runs: u32, finish_ms: u32) -> f64 {
    100.0 * tier_score(tier) + 10.0 * (1.0 + runs as f64).ln() - finish_ms as f64 / 1000.0
}

/// 공개 코스의 효용 점수: 100 · 등급 점수(premium +2, neutral · unrated 0, discount −3) + 10 · ln(1 + 달린 횟수) − 평균 완주 시간(초).
/// 많이 달린 길보다 premium 이고 빠른 길을 고스트로 고른다. 사람이 많이 지나도 마찰이 큰 길(discount)은 권하지 않는다
pub fn ghost_utility(course: &PublicCourse) -> f64 {
    utility(tier(course.premium, course.discount), course.runs, course.average.last().copied().unwrap_or(0))
}

/// Phase 5 Part 1: PD-Verdict v2 (`grade` · 연속 `rank` · `superseded`)를 반영한 연속 효용 점수.
/// `course.verdict` 가 없으면 기존 [`ghost_utility`] 로 매끄럽게 폴백한다.
pub fn ghost_utility_v3(course: &PublicCourse) -> f64 {
    let finish_ms = course.average.last().copied().unwrap_or(0);
    let Some(v) = &course.verdict else {
        return ghost_utility(course);
    };
    let r = v.rank.filter(|x| x.is_finite()).unwrap_or(0.0).clamp(-1.0, 1.0);
    let superseded = v.reasons.iter().any(|s| s == "superseded");
    let v_score = if superseded {
        -3.5
    } else {
        match v.tier {
            Tier::Premium => 1.5 + if v.grade == Some(2) { 0.5 } else { 0.0 } + 0.4 * r.max(0.0),
            Tier::Neutral => {
                if v.rank.is_some() {
                    0.25 * r
                } else {
                    0.0
                }
            }
            Tier::Discount => -2.5 - if v.grade == Some(-2) { 0.5 } else { 0.0 },
            Tier::Unrated => 0.0,
        }
    };
    100.0 * v_score + 10.0 * (1.0 + course.runs as f64).ln() - finish_ms as f64 / 1000.0
}

/// Phase 5 Part 1: 상위 호환 지름길 및 패치 폐쇄에 의한 구 코스 대체(`superseded`) 판정.
/// 동일 기점·결승을 가진 신규 코스가 PD-Verdict v2 `Premium(grade +2)` 이고, 구 코스의 부분집합 직행이거나
/// 갈림길 맞대결에서 결정적 우위(wins >= 5, 승률 >= 0.80, 평균 25% 이상 단축 + 구 코스 최근 마찰 악화)를 입증하면 true.
pub fn is_course_superseded(old: &PublicCourse, new: &PublicCourse, duel_wins: u32, duel_losses: u32, old_recent_friction_worse: bool) -> bool {
    if old.course == new.course || old.steps.first() != new.steps.first() || old.steps.last() != new.steps.last() {
        return false;
    }
    let new_strong_premium = new
        .verdict
        .as_ref()
        .is_some_and(|v| v.tier == Tier::Premium && v.grade == Some(2));
    if !new_strong_premium {
        return false;
    }
    let old_avg = old.average.last().copied().unwrap_or(0) as f64;
    let new_avg = new.average.last().copied().unwrap_or(0) as f64;
    if old_avg <= 0.0 || new_avg >= old_avg {
        return false;
    }
    // 조건 A: 엄격 부분집합 직행 단축 (C_new.steps ⊂ C_old.steps)
    let strict_subset = new.steps.len() < old.steps.len() && new.steps.iter().all(|s| old.steps.contains(s));
    if strict_subset {
        return true;
    }
    // 조건 B: 갈림길 맞대결 결정적 우위 + 25% 이상 단축 + 구 코스 최근 마찰 악화
    let total_duels = duel_wins + duel_losses;
    let win_rate = if total_duels == 0 { 0.0 } else { duel_wins as f64 / total_duels as f64 };
    let speedup = (old_avg - new_avg) / old_avg;
    duel_wins >= 5 && win_rate >= 0.80 && speedup >= 0.25 && old_recent_friction_worse
}

/// Phase 7 Part 4: 코스의 물리 조작 요구 특성 비트마스크를 결정론적으로 산출한다.
pub fn compute_interaction_traits(steps: &[StepRef], splits: &[u32], has_vision_only: bool, discount: u32, runs: u32) -> u16 {
    let mut traits = 0u16;
    let no_fine_gesture = steps.iter().all(|s| {
        !matches!(
            s.gesture,
            Some(Gesture::SwipeLeft | Gesture::SwipeRight | Gesture::SwipeUp | Gesture::SwipeDown)
        )
    });
    if no_fine_gesture {
        traits |= TRAIT_NO_FINE_GESTURE;
    }
    if !has_vision_only {
        traits |= TRAIT_TREE_ACCESSIBLE;
    }
    let no_rapid_chain = splits.windows(2).all(|w| w[1].saturating_sub(w[0]) >= 800);
    let low_friction_rate = runs == 0 || (discount as f64 / runs as f64) <= 0.15;
    if no_rapid_chain && low_friction_rate {
        traits |= TRAIT_LOW_DENSITY_CALM;
    }
    traits
}

/// 기기 로컬 전용 접근성 라우팅 선호 (서버 전송 0바이트, Phase 7 Part 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityRoutePrefs {
    pub require_tree_accessible: bool,
    pub prefer_no_fine_gesture: bool,
    pub prefer_calm: bool,
}

/// 로컬 접근성 조건과 [`ghost_utility_v3`] 를 결합해 포용적 고스트 우선순위를 산출한다.
pub fn contextual_ghost_utility(course: &PublicCourse, prefs: &AccessibilityRoutePrefs) -> f64 {
    let mut score = ghost_utility_v3(course);
    let traits = if course.interaction_traits != 0 {
        course.interaction_traits
    } else {
        compute_interaction_traits(&course.steps, &course.average, false, course.discount, course.runs)
    };
    if prefs.require_tree_accessible && (traits & TRAIT_TREE_ACCESSIBLE) == 0 {
        score -= 500.0;
    }
    if prefs.prefer_no_fine_gesture {
        if (traits & TRAIT_NO_FINE_GESTURE) != 0 {
            score += 120.0;
        } else {
            score -= 120.0;
        }
    }
    if prefs.prefer_calm && (traits & TRAIT_LOW_DENSITY_CALM) != 0 {
        score += 40.0;
    }
    score
}

/// Phase 7 Part 4: 기기 로컬 개인 속도비 `ρ_u = median(Δt_user / Δt_public) >= 1.0`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonalPaceProfile {
    ratios: Vec<f64>,
}

impl PersonalPaceProfile {
    pub fn observe_segment(&mut self, user_ms: u32, public_avg_ms: u32) {
        if user_ms == 0 || public_avg_ms == 0 {
            return;
        }
        let r = (user_ms as f64 / public_avg_ms as f64).clamp(1.0, 20.0);
        self.ratios.push(r);
        if self.ratios.len() > 64 {
            self.ratios.remove(0);
        }
    }

    pub fn ratio(&self) -> f64 {
        if self.ratios.is_empty() {
            return 1.0;
        }
        let mut sorted = self.ratios.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let mid = sorted.len() / 2;
        if sorted.len() % 2 == 1 {
            sorted[mid]
        } else {
            0.5 * (sorted[mid - 1] + sorted[mid])
        }
    }
}

/// Phase 4 Part 2: 구간 증분 지름길 대결 최소 단축량(500ms) 및 최소 단축률(8%).
pub const MIN_SHORTCUT_SAVED_MS: u32 = 500;
pub const MIN_SHORTCUT_MARGIN: f32 = 0.08;

/// 지름길 분기 유형: 대안 경유(`AltBranch`) vs 중간 스텝 생략 직행(`DirectBypass`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutKind {
    AltBranch,
    DirectBypass,
}

/// 구간 증분 시간 대결(Window-Local Split Duel) 결과 (Phase 4 Part 2 + Phase 7 Part 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortcutWinV2 {
    pub kind: ShortcutKind,
    pub fork_anchor_id: String,
    pub merge_anchor_id: String,
    pub skipped: StepRef,
    pub skipped_id: String,
    pub taken: StepRef,
    pub taken_id: String,
    pub saved_ms: u32,
    pub margin: f32,
    pub window_run_ms: u32,
    pub window_ghost_ms: u32,
}

/// LCS 공통 앵커 창 `[a → b]` 내부의 순수 구간 소요 시간만 비교하는 `shortcut_wins 2.0`.
///
/// * 1차 분기에서 시간을 잃고 2차 분기에서 만회한 런의 무임승차를 차단하고, 초반 실수가 있어도 후반 진성 지름길을 구제한다.
/// * 깔끔한 화면(`run.kind == None`)에서도 비함정(`kind != Discount`)·창 내부 무병목이면 정상 포착한다.
/// * `user_pace_ratio` (`ρ_u >= 1.0`)로 보조기기 사용자의 구간 소요 시간을 개인 정규화해 동등한 지름길 승리를 인정한다.
pub fn shortcut_wins_v2(run: &Run, ghost: &PublicCourse, fault_steps_mask: u16, user_pace_ratio: f64) -> Vec<ShortcutWinV2> {
    let refs = run.refs();
    if refs.len() < 2
        || ghost.steps.len() < 2
        || ghost.average.len() != ghost.steps.len()
        || run.kind == Some(MarkType::Discount)
        || refs.first() != ghost.steps.first()
        || refs.last() != ghost.steps.last()
    {
        return Vec::new();
    }
    let pace_ratio = crate::portfolio::PaceScale::from_rho_u(user_pace_ratio).get();
    let mut out = Vec::new();
    for w in common_steps(&refs, &ghost.steps).windows(2) {
        let ((i0, j0), (i1, j1)) = (w[0], w[1]);
        if j1 <= j0 + 1 {
            continue;
        }
        let skipped = &ghost.steps[j0 + 1];
        if refs.contains(skipped) {
            continue;
        }
        // 창 내부 런 스텝에 국소 마찰 병목이 있으면 기각 (Clean-Segment Gate).
        let window_faulted = (i0..=i1).any(|idx| idx < 16 && (fault_steps_mask & (1u16 << idx)) != 0);
        if window_faulted {
            continue;
        }
        let raw_run_dt = run.steps[i1].split_ms.saturating_sub(run.steps[i0].split_ms);
        let norm_run_dt = (raw_run_dt as f64 / pace_ratio).round() as u32;
        let ghost_dt = ghost.average[j1].saturating_sub(ghost.average[j0]);
        if ghost_dt == 0 || norm_run_dt >= ghost_dt {
            continue;
        }
        let saved_ms = ghost_dt - norm_run_dt;
        let margin = saved_ms as f32 / ghost_dt as f32;
        if saved_ms < MIN_SHORTCUT_SAVED_MS || margin < MIN_SHORTCUT_MARGIN {
            continue;
        }
        let kind = if i1 > i0 + 1 { ShortcutKind::AltBranch } else { ShortcutKind::DirectBypass };
        let taken = refs[i0 + 1].clone();
        out.push(ShortcutWinV2 {
            kind,
            fork_anchor_id: ghost.steps[j0].id().unwrap_or_default(),
            merge_anchor_id: ghost.steps[j1].id().unwrap_or_default(),
            skipped: skipped.clone(),
            skipped_id: skipped.id().unwrap_or_default(),
            taken_id: taken.id().unwrap_or_default(),
            taken,
            saved_ms,
            margin,
            window_run_ms: norm_run_dt,
            window_ghost_ms: ghost_dt,
        });
    }
    out
}

/// Phase 4 Part 3: 미완주·중도 이탈/사망 에피소드의 생존자 편향을 깨뜨리는 단일 말단 함정 보고(전체 동선 무유출).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadEndMark {
    pub app: String,
    pub checkpoint_id: String,
    pub recipe: String,
    pub day: i64,
}

/// 3대 비함정 이탈 면책 게이트(Universal Abandon Exemption) 입력.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AbandonContext {
    /// 같은 브라우저/작업 세션의 인접 탭·창이 10분 내 목표 전환 완료 (비교 탐색 면책)
    pub sibling_converted_within_10m: bool,
    /// 광클·재시도·에러 없이 단순 조회 후 이탈 (탐색 면책)
    pub clean_exploratory: bool,
    /// 패배 직후 자책 발화 방패 활성
    pub defeat_shielded: bool,
    /// Alt-Tab 비활성/장기 유휴 자연 종료
    pub afk_unfocused: bool,
    /// 말단 체크포인트의 마찰 z 점수 (고확신 병목 하한 1.5)
    pub friction_z: f64,
    /// 인게임 즉사/패배(`vision.game_defeat`) 발생
    pub game_defeat: bool,
}

pub const DEAD_END_MIN_FRICTION_Z: f64 = 1.5;

/// 비면책 고확신 중단·패배 에피소드에서 원인 체크포인트 단 1개의 [`DeadEndMark`] 를 추출한다.
pub fn dead_end_mark_if_eligible(app: &str, checkpoint_id: &str, day: i64, ctx: &AbandonContext) -> Option<DeadEndMark> {
    if ctx.sibling_converted_within_10m || ctx.clean_exploratory || ctx.defeat_shielded || ctx.afk_unfocused {
        return None;
    }
    if !ctx.game_defeat && (!ctx.friction_z.is_finite() || ctx.friction_z < DEAD_END_MIN_FRICTION_Z) {
        return None;
    }
    let app = address(app)?;
    let cp = address(checkpoint_id)?;
    Some(DeadEndMark {
        app,
        checkpoint_id: cp,
        recipe: RECIPE_V2.to_string(),
        day,
    })
}

/// 지름길 대결 한 판 ([`shortcut_wins`]): 고스트가 간 체크포인트(`skipped`)를 지나지 않고 다른 체크포인트(`taken`)로 가서 고스트의
/// 평균보다 빨리 결승했다. 기기 안의 대결 장부에 `taken` 의 1승으로 넣는다:
/// `ledger.record_confusion(&scope, &taken.key(), &skipped.key(), margin)` (`premium_discount_sds::Ledger`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortcutWin {
    pub skipped: StepRef,
    pub taken: StepRef,
    /// 고스트 평균 완주 시간보다 줄인 시간
    pub saved_ms: u32,
    /// 줄인 몫 (`saved_ms` / 고스트 평균 완주 시간, 0 ~ 1). 대결 장부의 차이(margin)로 쓴다
    pub margin: f32,
}

/// 결승한 런을 따라간 고스트와 견줘 지름길 대결의 승리를 낸다. 시작 · 결승이 고스트와 같고([`route_marks`] 가 판정하는 경우),
/// 런이 premium 이고, 고스트의 평균 완주 시간보다 빨랐을 때만 낸다. 두 체크포인트 열의 가장 긴 공통 부분열로 갈림길을 찾고, 고스트가
/// 다음 공통 체크포인트 전에 지난 체크포인트를 런이 한 번도 지나지 않았으면 그 갈림길에서 고스트가 간 첫 체크포인트(`skipped`)와
/// 런이 간 첫 체크포인트(`taken`)를 한 쌍으로 낸다. 서버 판정은 런에만 있는 체크포인트를 판정하지 않으므로 이 승리는 기기 안에만 쌓는다
pub fn shortcut_wins(run: &Run, ghost: &PublicCourse) -> Vec<ShortcutWin> {
    let refs = run.refs();
    let Some(&average) = ghost.average.last() else {
        return Vec::new();
    };
    if refs.len() < 2 || ghost.steps.len() < 2 || run.kind != Some(MarkType::Premium) || run.total_ms >= average || route_marks(&refs, &ghost.steps).is_none() {
        return Vec::new();
    }
    let saved_ms = average - run.total_ms;
    let margin = saved_ms as f32 / average as f32;
    common_steps(&refs, &ghost.steps)
        .windows(2)
        .filter(|w| w[1].1 > w[0].1 + 1 && !refs.contains(&ghost.steps[w[0].1 + 1]))
        .map(|w| ShortcutWin {
            skipped: ghost.steps[w[0].1 + 1].clone(),
            taken: refs[w[0].0 + 1].clone(),
            saved_ms,
            margin,
        })
        .collect()
}

/// 시작 · 결승이 같은 두 체크포인트 열의 가장 긴 공통 부분열 (짝지은 자리들, 시작과 결승 포함). 두 열 모두 2개 이상이어야 한다
fn common_steps(a: &[StepRef], b: &[StepRef]) -> Vec<(usize, usize)> {
    let (inner_a, inner_b) = (&a[1..a.len() - 1], &b[1..b.len() - 1]);
    let mut table = vec![vec![0usize; inner_b.len() + 1]; inner_a.len() + 1];
    for i in (0..inner_a.len()).rev() {
        for j in (0..inner_b.len()).rev() {
            table[i][j] = if inner_a[i] == inner_b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut out = vec![(0, 0)];
    let (mut i, mut j) = (0, 0);
    while i < inner_a.len() && j < inner_b.len() {
        if inner_a[i] == inner_b[j] {
            out.push((i + 1, j + 1));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out.push((a.len() - 1, b.len() - 1));
    out
}

/// 진행 중인 체크포인트와 코스 하나의 맞춤 ([`Collection::guide_live`] 안에서만 쓴다)
struct Fit {
    state: GhostSyncState,
    /// 마지막으로 지난 코스 체크포인트의 자리 (다음 체크포인트는 그 뒤)
    matched: usize,
    /// 그 코스 체크포인트를 지난 진행 체크포인트의 자리 (구간 시간을 견줄 때 쓴다)
    matched_ref: usize,
}

impl Fit {
    fn rank(&self) -> u8 {
        match self.state {
            GhostSyncState::ExactPrefix => 0,
            GhostSyncState::Rejoined { .. } => 1,
            GhostSyncState::Detouring { .. } => 2,
        }
    }

    fn on_course(&self) -> bool {
        self.rank() < 2
    }
}

/// 진행 중인 체크포인트를 코스에 맞춘다. 시작이 다르면 None. 앞부분이 같으면 ExactPrefix, 아니면 진행 체크포인트마다 코스에서
/// 마지막으로 지난 자리 뒤의 같은 체크포인트를 찾아 나아가고, 지금 체크포인트가 코스에 있으면 Rejoined, 없으면 Detouring 이다
fn fit(refs: &[StepRef], course: &[StepRef]) -> Option<Fit> {
    if refs.first()? != course.first()? {
        return None;
    }
    let last = refs.len() - 1;
    if refs.len() <= course.len() && course[..refs.len()] == refs[..] {
        return Some(Fit {
            state: GhostSyncState::ExactPrefix,
            matched: last,
            matched_ref: last,
        });
    }
    let (mut matched, mut matched_ref) = (0, 0);
    let mut visited = vec![false; course.len()];
    for (i, step) in refs.iter().enumerate().skip(1) {
        if let Some(m) = (matched + 1..course.len()).find(|&m| course[m] == *step) {
            matched = m;
            matched_ref = i;
            visited[m] = true;
        }
    }
    let state = if matched_ref == last {
        GhostSyncState::Rejoined {
            ghost_index: matched,
            skipped_steps: (1..matched).filter(|&m| !visited[m]).count(),
        }
    } else {
        GhostSyncState::Detouring {
            last_matched_index: matched,
            rejoin_hint: course.get(matched + 1).cloned(),
        }
    };
    Some(Fit { state, matched, matched_ref })
}

/// 다음 갈림길 후보 하나를 넣는다 (같은 체크포인트면 효용 점수가 높은 쪽)
fn offer(options: &mut Vec<(StepRef, f64, Tier)>, step: Option<&StepRef>, score: f64, tier: Tier) {
    let Some(step) = step else {
        return;
    };
    match options.iter_mut().find(|(s, _, _)| s == step) {
        Some(option) if option.1 < score => {
            option.1 = score;
            option.2 = tier;
        }
        Some(_) => {}
        None => options.push((step.clone(), score, tier)),
    }
}

/// 앱 · 웹마다 코스 [`MAX_COURSES`] 개까지 두는 기록실 (기기 안)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    /// 앱 해시 → 코스들
    pub apps: BTreeMap<String, Vec<CourseEntry>>,
    #[serde(default)]
    pub clock: u64,
}

/// Pinned-SLRU 수동 핀 고정 최대 슬롯 수 (Phase 5 Part 2)
pub const SLRU_MAX_PINNED: usize = 3;
/// Pinned-SLRU 숙련 보호 세그먼트 진입 최소 주행 수
pub const SLRU_PROTECTED_MIN_RUNS: u32 = 3;

impl Collection {
    pub fn courses(&self, app: &str) -> &[CourseEntry] {
        self.apps.get(app).map_or(&[], |v| v.as_slice())
    }

    /// 코스를 수동 핀(`[▲]`)으로 고정하거나 해제한다 (앱당 최대 [`SLRU_MAX_PINNED`] 개).
    pub fn pin_course(&mut self, app: &str, course: &str, pinned: bool) -> bool {
        let Some(courses) = self.apps.get_mut(app) else {
            return false;
        };
        if pinned {
            let currently_pinned = courses.iter().filter(|e| e.pinned && e.course != course).count();
            if currently_pinned >= SLRU_MAX_PINNED {
                return false;
            }
        }
        let Some(entry) = courses.iter_mut().find(|e| e.course == course) else {
            return false;
        };
        entry.pinned = pinned;
        true
    }

    /// 런을 넣는다. 같은 코스면 횟수 · 결정 수 · 최고 · 마지막 기록을 고치고, 새 코스가 넘치면
    /// 2-세그먼트 Pinned-SLRU 규칙으로 일회성 Probation 코스 중 가장 오래된 것을 우선 퇴출한다.
    pub fn add(&mut self, run: &Run) -> Added {
        self.add_with_cap(run, MAX_COURSES)
    }

    /// 보관 상한(`max_courses`, 예: 서버 10 · raon-zena 로컬 존 금고 64)을 지정해 Pinned-SLRU 로 런을 추가한다.
    pub fn add_with_cap(&mut self, run: &Run, max_courses: usize) -> Added {
        let cap = max_courses.max(1);
        self.clock += 1;
        let clock = self.clock;
        let courses = self.apps.entry(run.app.clone()).or_default();
        let splits = run.splits();
        let (premium, discount) = match run.kind {
            Some(MarkType::Premium) => (1, 0),
            Some(MarkType::Discount) => (0, 1),
            None => (0, 0),
        };
        if let Some(entry) = courses.iter_mut().find(|e| e.course == run.course) {
            let best = run.total_ms < entry.best_ms();
            entry.runs += 1;
            entry.premium += premium;
            entry.discount += discount;
            if best {
                entry.best = splits.clone();
            }
            entry.last = splits;
            entry.last_day = entry.last_day.max(run.day);
            entry.touched = clock;
            return Added::Repeat { best };
        }
        let entry = CourseEntry {
            course: run.course.clone(),
            steps: run.refs(),
            runs: 1,
            premium,
            discount,
            best: splits.clone(),
            last: splits,
            last_day: run.day,
            touched: clock,
            pinned: false,
        };
        if courses.len() < cap {
            courses.push(entry);
            return Added::New;
        }
        // Pinned-SLRU 퇴출 순서:
        // 1. Tier 0 Probation (미고정 & runs < 3) 중 touched 최소
        // 2. 모두 보호(runs >= 3)면 미고정 Tier 1 Protected 중 retention = 2*ln(runs) - idle_days/14 최소
        // 3. 최후 폴백: 전체 touched 최소
        let probation_victim = courses
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.pinned && e.runs < SLRU_PROTECTED_MIN_RUNS)
            .min_by_key(|(_, e)| e.touched)
            .map(|(i, _)| i);
        let victim = probation_victim.unwrap_or_else(|| {
            courses
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.pinned)
                .min_by(|(_, a), (_, b)| {
                    let retention = |e: &CourseEntry| {
                        let idle_days = (run.day - e.last_day).max(0) as f64;
                        2.0 * (e.runs.max(1) as f64).ln() - idle_days / 14.0
                    };
                    retention(a).total_cmp(&retention(b)).then(a.touched.cmp(&b.touched))
                })
                .map(|(i, _)| i)
                .unwrap_or_else(|| courses.iter().enumerate().min_by_key(|(_, e)| e.touched).map_or(0, |(i, _)| i))
        });
        Added::Replaced {
            evicted: std::mem::replace(&mut courses[victim], entry).course,
        }
    }

    /// 진행 중인 체크포인트(시작부터 지금까지)로 고스트를 찾는다. 체크포인트 순서가 앞부분과 같은 코스 중에서 내가 가장 많이
    /// 달린 코스, 없으면 다른 사용자가 가장 많이 달린 코스를 고른다. 맞는 코스가 없으면 None.
    /// 진행 중인 체크포인트는 런과 같이 다듬는다(되감은 스크롤 합치기 · 곁길 접기): 메뉴를 잠깐 열었다 닫아도 코스의 앞부분과 맞는다.
    /// 솎기는 하지 않는다(코스는 10개까지라 그보다 긴 진행은 어느 코스의 앞부분도 아니다). 곁길 · 지름길 · 제자리 헤맴까지 보는
    /// 안내는 [`Collection::guide_live`] 다
    pub fn guide(&self, app: &str, progress: &[Checkpoint], others: &[PublicCourse]) -> Option<Guide> {
        let points = canonical(progress, &ReduceOptions::default(), false).ok()?;
        let start = points.first()?.at;
        let now = points.last()?.at;
        let elapsed = round_split(now - start) as i64;
        let refs: Vec<StepRef> = points.iter().map(|p| p.step.clone()).collect();
        let fits = |steps: &[StepRef]| steps.len() >= refs.len() && steps[..refs.len()] == refs[..];

        let mine: Vec<&CourseEntry> = self.courses(app).iter().filter(|e| fits(&e.steps)).collect();
        let theirs: Vec<&PublicCourse> = others.iter().filter(|c| fits(&c.steps)).collect();
        let course = match mine.iter().max_by_key(|e| (e.runs, e.touched)) {
            Some(e) => e.course.clone(),
            None => theirs.iter().max_by_key(|c| c.runs)?.course.clone(),
        };
        let my = mine.iter().find(|e| e.course == course).copied();
        let their = theirs.iter().find(|c| c.course == course).copied();
        let steps = my.map(|e| &e.steps).or(their.map(|c| &c.steps))?;
        let at = refs.len() - 1;
        let delta = |splits: &[u32]| splits.get(at).map(|s| elapsed - *s as i64);
        Some(Guide {
            passed: refs.len(),
            length: steps.len(),
            next: steps.get(refs.len()).cloned(),
            next_tier: their.and_then(|c| c.checkpoints.get(refs.len())).map_or(Tier::Unrated, CheckpointTally::tier),
            vs_best: my.and_then(|e| delta(&e.best)),
            vs_record: their.and_then(|c| delta(&c.record)),
            vs_average: their.and_then(|c| delta(&c.average)),
            my_runs: my.map_or(0, |e| e.runs),
            all_runs: their.map(|c| c.runs),
            tier: match (their, my) {
                (Some(c), _) => tier(c.premium, c.discount),
                (None, Some(e)) => e.tier(),
                (None, None) => Tier::Unrated,
            },
            course,
        })
    }

    /// 실시간 안내. 진행 중인 체크포인트를 다듬고(곁길 접기 · 되감은 스크롤 합치기, 동결 구간 빼기) 시작이 같은 코스마다 맞춘다
    /// ([`GhostSyncState`]: 앞부분이 같음 · 다시 들어섬 · 곁길). 내 코스와 공개 코스를 따로 고른다:
    /// - 내 코스: 맞춤이 좋은 것(앞부분이 같음 → 다시 들어섬 → 곁길), 그다음 많이 달린 것 · 최근에 달린 것
    /// - 공개 코스: `opts.ghost_steps` 를 모두 가진 코스(따라가던 고스트) → 맞춤 → 효용 점수([`ghost_utility`]). 사람이 많아도 discount 인
    ///   길보다 premium 이고 빠른 길이 고스트가 된다
    ///
    /// 내 코스와 공개 코스의 다음 갈림길이 다르고, 결승이 같고, 공개 코스가 premium 이고, 그 평균 완주 시간이 내 최고 기록보다 15 % 이상 ·
    /// 1.5초 이상 빠르면 공개 코스로 안내한다(`upgrade_over_habit_ms`). 내 코스에서는 곁길인데 공개 코스와는 맞을 때도 공개 코스로
    /// 안내한다. 구간 시간의 차이는 코스마다 마지막으로 지난 체크포인트에서
    /// 견주고(다시 들어서면 그 자리에서 다시 견준다), `live_overrun_ms` 는 지금 시각 `now_ms` 로 잰다. 시작이 같은 코스가 없으면 None
    pub fn guide_live(&self, app: &str, progress: &[Checkpoint], others: &[PublicCourse], now_ms: i64, opts: &ReduceOptions<'_>) -> Option<LiveGuide> {
        let points = canonical(progress, opts, false).ok()?;
        let start = points.first()?.at;
        let latest = points.last()?.at;
        let spans = opts.frozen_spans;
        let elapsed: Vec<i64> = points.iter().map(|p| round_split(effective(start, p.at, spans)) as i64).collect();
        let refs: Vec<StepRef> = points.into_iter().map(|p| p.step).collect();

        let mine_all: Vec<(&CourseEntry, Fit)> = self.courses(app).iter().filter_map(|e| Some((e, fit(&refs, &e.steps)?))).collect();
        let theirs_all: Vec<(&PublicCourse, Fit)> = others.iter().filter_map(|c| Some((c, fit(&refs, &c.steps)?))).collect();
        let pinned = |c: &PublicCourse| !opts.ghost_steps.is_empty() && opts.ghost_steps.iter().all(|s| c.steps.contains(s));
        let mine = mine_all
            .iter()
            .min_by(|(a, x), (b, y)| x.rank().cmp(&y.rank()).then(b.runs.cmp(&a.runs)).then(b.touched.cmp(&a.touched)));
        let theirs = theirs_all
            .iter()
            .min_by(|(a, x), (b, y)| pinned(b).cmp(&pinned(a)).then(x.rank().cmp(&y.rank())).then(ghost_utility(b).total_cmp(&ghost_utility(a))));

        let upgrade = match (mine, theirs) {
            (Some((e, x)), Some((c, y))) if e.course != c.course && x.on_course() && y.on_course() && e.steps.last() == c.steps.last() && tier(c.premium, c.discount) == Tier::Premium => {
                match (e.steps.get(x.matched + 1), c.steps.get(y.matched + 1), c.average.last()) {
                    (Some(habit), Some(shortcut), Some(&average)) if habit != shortcut => {
                        let best = e.best_ms() as i64;
                        let saved = best - average as i64;
                        (saved >= UPGRADE_MIN_MS && saved as f64 >= best as f64 * UPGRADE_MIN_SHARE).then_some(saved as u32)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        // 안내하는 코스: 지름길을 권하면 공개 코스, 공개 코스에 더 잘 맞으면(내 코스에서는 곁길인데 공개 코스에서는 아님) 공개 코스, 아니면 내 코스
        let public_main = upgrade.is_some()
            || match (mine, theirs) {
                (Some((_, x)), Some((_, y))) => y.rank() < x.rank(),
                (mine, _) => mine.is_none(),
            };
        let (course, steps, at) = match (mine, theirs) {
            (None, None) => return None,
            (Some((e, x)), _) if !public_main => (e.course.as_str(), e.steps.as_slice(), x),
            (_, Some((c, y))) => (c.course.as_str(), c.steps.as_slice(), y),
            (Some((e, x)), None) => (e.course.as_str(), e.steps.as_slice(), x),
        };
        let next_index = at.matched + 1;
        let next = steps.get(next_index).cloned();
        // 안내하는 코스가 공개돼 있으면 그 기록, 아니면 따라가는 공개 코스에서 다음 체크포인트의 판정 수를 찾는다
        let public = others.iter().find(|c| c.course == course);
        let next_tier = next
            .as_ref()
            .and_then(|n| {
                let c = public.or(theirs.map(|(c, _)| *c))?;
                let i = if c.steps.get(next_index) == Some(n) { next_index } else { c.steps.iter().position(|s| s == n)? };
                c.checkpoints.get(i).map(CheckpointTally::tier)
            })
            .unwrap_or(Tier::Unrated);
        let tier_now = match (public, mine) {
            (Some(c), _) => tier(c.premium, c.discount),
            (None, Some((e, _))) if e.course == course => e.tier(),
            _ => Tier::Unrated,
        };
        let delta = |s: &Fit, splits: &[u32]| splits.get(s.matched).map(|ms| elapsed[s.matched_ref] - *ms as i64);
        let expected = match public {
            Some(c) => c.average.get(next_index).or(c.record.get(next_index)),
            None => mine.filter(|(e, _)| e.course == course).and_then(|(e, _)| e.best.get(next_index)),
        };
        let unit = SPLIT_UNIT_MS as i64;
        let now = effective(start, now_ms.max(latest), spans);
        let live_overrun_ms = expected.map_or(0, |ms| ((now - *ms as i64).max(0) / unit * unit).min(u32::MAX as i64) as u32);

        // 박빙 갈림길: 맞춤이 좋은 코스들의 다음 체크포인트를 효용 점수로 견줘 두 길이 팽팽하면 다른 쪽을 함께 알린다
        let mut options: Vec<(StepRef, f64, Tier)> = Vec::new();
        for (e, s) in mine_all.iter().filter(|(_, s)| s.on_course()) {
            offer(&mut options, e.steps.get(s.matched + 1), utility(e.tier(), e.runs, e.best_ms()), e.tier());
        }
        for (c, s) in theirs_all.iter().filter(|(_, s)| s.on_course()) {
            offer(&mut options, c.steps.get(s.matched + 1), ghost_utility(c), tier(c.premium, c.discount));
        }
        let scores: Vec<Vec<f32>> = options.iter().map(|o| vec![o.1 as f32]).collect();
        let alt_next = match line_tie_shape(&scores, 0, f32::NEG_INFINITY) {
            Some((TieKind::PairConfusion, top, second, _, _)) => {
                let pick = if next.as_ref() == Some(&options[top].0) { second } else { top };
                Some((options[pick].0.clone(), options[pick].2))
            }
            _ => None,
        };

        Some(LiveGuide {
            base: Guide {
                course: course.to_string(),
                passed: at.matched + 1,
                length: steps.len(),
                next,
                next_tier,
                vs_best: mine.and_then(|(e, s)| delta(s, &e.best)),
                vs_record: theirs.and_then(|(c, s)| delta(s, &c.record)),
                vs_average: theirs.and_then(|(c, s)| delta(s, &c.average)),
                my_runs: mine.map_or(0, |(e, _)| e.runs),
                all_runs: theirs.map(|(c, _)| c.runs),
                tier: tier_now,
            },
            my_best_course: mine.map(|(e, _)| e.course.clone()),
            tracked_public_ghost: theirs.map(|(c, _)| c.course.clone()),
            sync_state: at.state.clone(),
            live_overrun_ms,
            upgrade_over_habit_ms: upgrade,
            alt_next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_hash;

    const S1: &str = "0x1111111111111111111111111111111111111111";
    const R2: &str = "0x2222222222222222222222222222222222222222";
    const S3: &str = "0x3333333333333333333333333333333333333333";
    // ethers 6.17 로 뽑은 값 (www 시험 navtrials.test.ts 와 같은 기준값)
    const APP: &str = "0x39206c971297059b89e4e6fc35844ee87feb0407";
    // 코스 S1 → R2 → S3 의 코스 id 체인 (마지막이 코스 id)
    const CHAIN: [&str; 3] = [
        "0x826797e39016c80c7db39259352ad3af843f0acf",
        "0x517cbe9e479a79a6ef66e403072e9776000d90fc",
        "0xa9c45e19240962f6cb2dfb7aa8381e3ec3d80340",
    ];
    const COURSE: &str = CHAIN[2];
    // 영역 R2 의 체크포인트 id
    const CP_R2: &str = "0x4e9fa56d3569702eb1ec03106e8bd1896a716af5";
    const DAY: i64 = 20_774;

    fn cp(at_ms: i64, kind: StepKind, hash: &str) -> Checkpoint {
        Checkpoint::new(at_ms, kind, hash, None)
    }

    // 상태를 확정한 체크포인트 (폼 제출 · 결제 · 장착)
    fn committed(at_ms: i64, kind: StepKind, hash: &str) -> Checkpoint {
        Checkpoint {
            committed: true,
            ..cp(at_ms, kind, hash)
        }
    }

    // 코스 체크포인트를 `ms` 에 지난다
    fn pass(ms: i64, s: &StepRef) -> Checkpoint {
        Checkpoint::new(ms, s.kind, s.hash.clone(), s.gesture)
    }

    fn step(kind: StepKind, hash: &str) -> StepRef {
        StepRef {
            kind,
            hash: hash.into(),
            gesture: None,
        }
    }

    fn hash_n(n: usize) -> String {
        format!("0x{n:040x}")
    }

    // 시작 · 영역 · 결승을 `ms` 간격으로 지나는 런
    fn lap(ms: [i64; 2], kind: Option<MarkType>) -> Run {
        let t0 = DAY * DAY_MS;
        let points = [cp(t0, StepKind::Screen, S1), cp(t0 + ms[0], StepKind::Region, R2), cp(t0 + ms[0] + ms[1], StepKind::Screen, S3)];
        Run::from_checkpoints(APP, &points, kind).unwrap()
    }

    #[test]
    fn app_checkpoint_and_course_ids_match_ethers() {
        assert_eq!(app_hash("Shop.Example.com ").unwrap(), APP);
        let refs = [step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, S3)];
        // 체크포인트 id = hashId(종류 + 해시 + 제스처)
        assert_eq!(refs[1].id().unwrap(), CP_R2);
        assert_eq!(hash_id(&format!("region{R2}")).unwrap(), CP_R2);
        assert_ne!(step(StepKind::Screen, R2).id().unwrap(), CP_R2, "종류가 다르면 다른 체크포인트");
        // 코스 id 체인: hashId(앱 id + 체크포인트 id) → hashId(이전 코스 id + 체크포인트 id) → …
        let ids: Vec<String> = refs.iter().map(|r| r.id().unwrap()).collect();
        assert_eq!(hash_id(&format!("{APP}{}", ids[0])).unwrap(), CHAIN[0]);
        assert_eq!(hash_id(&format!("{}{}", CHAIN[0], ids[1])).unwrap(), CHAIN[1]);
        assert_eq!(hash_id(&format!("{}{}", CHAIN[1], ids[2])).unwrap(), CHAIN[2]);
        assert_eq!(course_chain(APP, &refs).unwrap(), CHAIN);
        assert_eq!(course_hash(APP, &refs).unwrap(), COURSE);
        let run = lap([1000, 2000], None);
        assert_eq!(run.steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), ids);
        assert_eq!(run.steps.iter().map(|s| s.course.clone()).collect::<Vec<_>>(), CHAIN);
        assert_eq!(run.course, COURSE);
        // 앞부분이 같으면 그 지점까지의 코스 id 를 함께 갖고, 다른 체크포인트부터 갈라진다
        let branch = course_chain(APP, &[step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, &hash_n(0x66))]).unwrap();
        assert_eq!(&branch[..2], &CHAIN[..2]);
        assert_ne!(branch[2], COURSE);
        // 결승에서 멈춘 짧은 코스(S1 → R2)의 id 는 긴 코스의 중간 id 와 같다: 같은 길은 한 번만 등록된다
        assert_eq!(course_hash(APP, &refs[..2]).unwrap(), CHAIN[1]);
        // 순서 · 종류가 다르면 다른 코스
        let swapped = [step(StepKind::Region, R2), step(StepKind::Screen, S1), step(StepKind::Screen, S3)];
        assert_ne!(course_hash(APP, &swapped).unwrap(), COURSE);
        let kind = [step(StepKind::Screen, S1), step(StepKind::Screen, R2), step(StepKind::Screen, S3)];
        assert_ne!(course_hash(APP, &kind).unwrap(), COURSE);
    }

    // 웹 코스: 페이지(링크값) · 요소(DOM 셀렉터)와 제스처. ethers 6.17 로 뽑은 값 (www 시험 navtrials.test.ts 와 같은 기준값)
    const WEB_COURSE: &str = "0xe8c5279f506af6687f43eee19360173c51f44cce";

    fn gcp(at_ms: i64, kind: StepKind, fill: char, gesture: Option<Gesture>) -> Checkpoint {
        Checkpoint::new(at_ms, kind, format!("0x{}", fill.to_string().repeat(40)), gesture)
    }

    #[test]
    fn web_courses_use_pages_elements_and_gestures() {
        let points = [
            gcp(0, StepKind::Page, 'a', None),
            gcp(400, StepKind::Element, 'b', Some(Gesture::Press)), // 누름은 붙지 않는다
            gcp(900, StepKind::Page, 'a', Some(Gesture::ScrollDown)),
            gcp(1000, StepKind::Page, 'a', Some(Gesture::ScrollDown)), // 이어진 스크롤은 하나로
            gcp(1500, StepKind::Element, 'c', Some(Gesture::SwipeLeft)),
            gcp(2100, StepKind::Page, 'd', None),
        ];
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.course, WEB_COURSE);
        let keys: Vec<String> = run.refs().iter().map(StepRef::key).collect();
        assert_eq!(keys[1], format!("element:0x{}", "b".repeat(40)));
        assert_eq!(keys[2], format!("page:0x{}@scroll-down", "a".repeat(40)));
        assert_eq!(run.splits(), vec![0, 400, 900, 1500, 2100]);
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["steps"][3]["kind"], "element");
        assert_eq!(json["steps"][3]["gesture"], "swipe-left");
        assert!(json["steps"][1].get("gesture").is_none(), "누름은 칸이 없다");
        // 마우스로 누른 사람과 손가락으로 누른 사람은 같은 코스, 스와이프로 지난 사람은 다른 코스
        let mut tapped = points.clone();
        tapped[1].gesture = None;
        assert_eq!(Run::from_checkpoints(APP, &tapped, None).unwrap().course, WEB_COURSE);
        tapped[4].gesture = Some(Gesture::Press);
        assert_ne!(Run::from_checkpoints(APP, &tapped, None).unwrap().course, WEB_COURSE);
    }

    #[test]
    fn run_rounds_splits_and_keeps_the_finish_day() {
        let t0 = DAY * DAY_MS + DAY_MS - 1_000; // 자정 1초 전 출발 → 다음 날 결승
        let points = [
            cp(t0, StepKind::Screen, &S1.to_uppercase().replace("0X", "0x")),
            cp(t0 + 1_249, StepKind::Region, R2),
            cp(t0 + 1_300, StepKind::Region, R2), // 같은 체크포인트를 잇달아 → 하나로 (처음 도착 시각)
            cp(t0 + 3_351, StepKind::Screen, S3),
        ];
        let run = Run::from_checkpoints(APP, &points, Some(MarkType::Premium)).unwrap();
        assert_eq!(run.splits(), vec![0, 1_200, 3_400]);
        assert_eq!(run.total_ms, 3_400);
        assert_eq!(run.day, DAY + 1);
        assert_eq!(run.steps[0].hash, S1);
        assert_eq!(run.course, COURSE);

        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["schema"], "trial/1");
        assert_eq!(json["type"], "premium");
        assert_eq!(json["recipe"], "pd-verdict/2");
        assert_eq!(json["steps"][1], serde_json::json!({ "id": CP_R2, "course": CHAIN[1], "kind": "region", "hash": R2, "split_ms": 1200 }));
        assert_eq!(serde_json::to_value(lap([1, 1], None)).unwrap()["type"], serde_json::Value::Null);
        assert_eq!(serde_json::from_value::<Run>(json).unwrap(), run);
    }

    // 예전에는 끝 쪽 10개만 남겨 시작을 버리고 시계를 남은 첫 체크포인트에서 다시 쟀다(총 9초). 이제 시작 · 결승을 두고 중간을 솎는다
    #[test]
    fn long_flows_keep_the_start_and_the_finish() {
        let points: Vec<Checkpoint> = (0..13).map(|i| cp(i as i64 * 1_000, StepKind::Screen, &hash_n(i + 1))).collect();
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.steps.len(), MAX_STEPS);
        assert_eq!(run.steps[0].hash, hash_n(1), "시작은 버리지 않는다");
        assert_eq!(run.steps[0].split_ms, 0);
        assert_eq!(run.steps[MAX_STEPS - 1].hash, hash_n(13));
        assert_eq!(run.total_ms, 12_000, "시계는 진짜 시작에서 잰다");
        // 중요도가 같으면 결승에 가까운 것을 남긴다: 중간 11개 중 앞의 3개(2 ~ 4)를 솎는다
        assert_eq!(run.steps[1].hash, hash_n(5));
        assert_eq!(run.splits()[1], 4_000);
    }

    #[test]
    fn invalid_runs_are_refused() {
        let ok = [cp(0, StepKind::Screen, S1), cp(1_000, StepKind::Screen, S3)];
        assert_eq!(Run::from_checkpoints("0x12", &ok, None), Err(TrialError::Hash));
        assert_eq!(Run::from_checkpoints(APP, &[cp(0, StepKind::Screen, "nope"), ok[1].clone()], None), Err(TrialError::Hash));
        assert_eq!(Run::from_checkpoints(APP, &ok[..1], None), Err(TrialError::TooShort));
        assert_eq!(Run::from_checkpoints(APP, &[ok[0].clone(), cp(5, StepKind::Screen, S1)], None), Err(TrialError::TooShort));
        assert_eq!(Run::from_checkpoints(APP, &[ok[1].clone(), ok[0].clone()], None), Err(TrialError::Order));
        let slow = [ok[0].clone(), cp(MAX_RUN_MS as i64 + 1, StepKind::Screen, S3)];
        assert_eq!(Run::from_checkpoints(APP, &slow, None), Err(TrialError::TooLong));
    }

    #[test]
    fn vectors_are_normalized_per_checkpoint() {
        let mut run = lap([1000, 1000], None);
        run.attach_vectors(1, &[("granite-97m-r2-cls-v1", &[3.0, 4.0])]).unwrap();
        assert_eq!(run.steps[1].vectors[0].vector, vec![0.6, 0.8]);
        assert_eq!(run.attach_vectors(1, &[("a", &[1.0]), ("a", &[1.0])]), Err(MarkError::Space));
        assert_eq!(run.attach_vectors(1, &[("a", &[0.0])]), Err(MarkError::Vector));
        assert_eq!(run.attach_vectors(9, &[("a", &[1.0])]), Err(MarkError::Vector));
    }

    #[test]
    fn repeats_count_up_and_keep_the_best() {
        let mut c = Collection::default();
        assert_eq!(c.add(&lap([2000, 2000], Some(MarkType::Discount))), Added::New);
        assert_eq!(c.add(&lap([1000, 1500], Some(MarkType::Premium))), Added::Repeat { best: true });
        assert_eq!(c.add(&lap([3000, 3000], None)), Added::Repeat { best: false });
        let e = &c.courses(APP)[0];
        assert_eq!((e.runs, e.premium, e.discount), (3, 1, 1));
        assert_eq!(e.best, vec![0, 1000, 2500]);
        assert_eq!(e.last, vec![0, 3000, 6000]);
        assert_eq!(c.courses(APP).len(), 1);
    }

    #[test]
    fn the_eleventh_course_replaces_the_one_run_longest_ago() {
        let mut c = Collection::default();
        let course = |i: usize| Run::from_checkpoints(APP, &[cp(0, StepKind::Screen, &hash_n(100 + i)), cp(1000, StepKind::Screen, S3)], None).unwrap();
        for i in 0..MAX_COURSES {
            assert_eq!(c.add(&course(i)), Added::New);
        }
        // 0번 코스를 다시 달려 가장 최근이 된다 → 다음에 밀려나는 것은 1번
        c.add(&course(0));
        assert_eq!(c.add(&course(10)), Added::Replaced { evicted: course(1).course });
        assert_eq!(c.courses(APP).len(), MAX_COURSES);
        assert!(c.courses(APP).iter().any(|e| e.course == course(0).course && e.runs == 2));
        // 다른 앱 · 웹은 따로 센다
        let other = Run::from_checkpoints(S1, &[cp(0, StepKind::Screen, S1), cp(1, StepKind::Screen, S3)], None).unwrap();
        assert_eq!(c.add(&other), Added::New);
        assert_eq!(c.courses(S1).len(), 1);
    }

    #[test]
    fn tier_needs_three_decided_runs_and_a_two_thirds_share() {
        assert_eq!(tier(2, 0), Tier::Unrated);
        assert_eq!(tier(2, 1), Tier::Premium);
        assert_eq!(tier(1, 2), Tier::Discount);
        assert_eq!(tier(3, 2), Tier::Neutral);
        assert_eq!(tier(4, 2), Tier::Premium);
        assert_eq!(tier(0, 3), Tier::Discount);
    }

    // www 시험 navtrials.test.ts 와 같은 사례: 고스트 S1 → A → B → 결승, 런 S1 → A → C → 결승
    #[test]
    fn following_the_ghost_is_premium_and_the_road_not_taken_is_discount() {
        let (a, b, c) = (step(StepKind::Region, R2), step(StepKind::Region, &hash_n(0x44)), step(StepKind::Region, &hash_n(0x55)));
        let (start, finish) = (step(StepKind::Screen, S1), step(StepKind::Screen, S3));
        let ghost = [start.clone(), a.clone(), b.clone(), finish.clone()];
        let run = [start.clone(), a.clone(), c.clone(), finish.clone()];
        assert_eq!(route_marks(&run, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Discount)]));
        // 고스트를 그대로 따라가면 중간 체크포인트가 모두 premium
        assert_eq!(route_marks(&ghost, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Premium)]));
        // 순서가 달라도 지났으면 따라간 것이다
        let swapped = [start.clone(), b.clone(), a.clone(), finish.clone()];
        assert_eq!(route_marks(&swapped, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Premium)]));
        // 지름길(중간 없이 결승)이면 고스트의 중간 체크포인트는 모두 discount
        assert_eq!(route_marks(&[start.clone(), finish.clone()], &ghost), Some(vec![(a.clone(), MarkType::Discount), (b.clone(), MarkType::Discount)]));
        // 결승 · 시작이 다르면 목적이 다른 것이라 판정하지 않는다. 중간이 없는 고스트는 판정할 것이 없다
        assert_eq!(route_marks(&[start.clone(), a.clone(), c.clone()], &ghost), None);
        assert_eq!(route_marks(&[c.clone(), a.clone(), finish.clone()], &ghost), None);
        assert_eq!(route_marks(&[], &ghost), None);
        assert_eq!(route_marks(&run, &[start.clone(), finish.clone()]), Some(vec![]));
        assert_eq!(CheckpointTally { premium: 1, discount: 3 }.tier(), Tier::Discount);
    }

    #[test]
    fn a_run_names_the_ghost_it_followed() {
        let mut run = lap([1000, 1000], None);
        assert!(serde_json::to_value(&run).unwrap().get("ghost").is_none(), "고스트가 없으면 칸도 없다");
        run.ghost = Some(COURSE.into());
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["ghost"], COURSE);
        assert_eq!(serde_json::from_value::<Run>(json).unwrap(), run);
    }

    #[test]
    fn guide_follows_the_ghost_to_the_finish() {
        let mut c = Collection::default();
        c.add(&lap([1000, 2000], Some(MarkType::Premium)));
        let t0 = 5 * DAY_MS;
        let others = vec![PublicCourse {
            course: COURSE.into(),
            steps: vec![step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, S3)],
            runs: 42,
            premium: 20,
            discount: 4,
            record: vec![0, 800, 1500],
            average: vec![0, 1400, 3500],
            checkpoints: vec![CheckpointTally::default(), CheckpointTally { premium: 1, discount: 3 }, CheckpointTally::default()],
            verdict: None,
            interaction_traits: 0,
        }];

        // 출발만 했을 때: 다음은 영역 R2, 차이는 0
        let g = c.guide(APP, &[cp(t0, StepKind::Screen, S1)], &others).unwrap();
        assert_eq!((g.passed, g.length), (1, 3));
        assert_eq!(g.next, Some(step(StepKind::Region, R2)));
        assert_eq!((g.vs_best, g.vs_record, g.vs_average), (Some(0), Some(0), Some(0)));
        assert_eq!((g.my_runs, g.all_runs, g.tier), (1, Some(42), Tier::Premium));
        // 다음 체크포인트(R2)는 다른 사용자 넷 중 셋이 비켜 갔다
        assert_eq!(g.next_tier, Tier::Discount);

        // 1.3초에 R2: 내 최고보다 0.3초, 기록보다 0.5초 뒤짐, 평균보다 0.1초 앞섬
        let progress = [cp(t0, StepKind::Screen, S1), cp(t0 + 1_300, StepKind::Region, R2)];
        let g = c.guide(APP, &progress, &others).unwrap();
        assert_eq!(g.next, Some(step(StepKind::Screen, S3)));
        assert_eq!((g.vs_best, g.vs_record, g.vs_average), (Some(300), Some(500), Some(-100)));
        assert_eq!(g.next_tier, Tier::Unrated);

        // 결승선을 지나면 다음은 없다
        let done = [progress[0].clone(), progress[1].clone(), cp(t0 + 2_900, StepKind::Screen, S3)];
        let g = c.guide(APP, &done, &others).unwrap();
        assert_eq!(g.next, None);
        assert_eq!(g.vs_best, Some(-100));

        // 처음 가는 사용자(내 기록 없음)도 다른 사용자의 고스트로 안내받는다
        let g = Collection::default().guide(APP, &progress, &others).unwrap();
        assert_eq!((g.vs_best, g.vs_record, g.my_runs), (None, Some(500), 0));
        // 코스와 다른 길이면 안내하지 않는다
        assert_eq!(c.guide(APP, &[cp(t0, StepKind::Screen, S3)], &others), None);
        assert_eq!(c.guide(APP, &[], &others), None);
    }

    // 공개 코스 하나 (체크포인트 판정 수는 비워 둔다)
    fn public(steps: Vec<StepRef>, runs: u32, premium: u32, discount: u32, record: Vec<u32>, average: Vec<u32>) -> PublicCourse {
        let interaction_traits = compute_interaction_traits(&steps, &average, false, discount, runs);
        PublicCourse {
            course: course_hash(APP, &steps).unwrap(),
            checkpoints: vec![CheckpointTally::default(); steps.len()],
            steps,
            runs,
            premium,
            discount,
            record,
            average,
            verdict: None,
            interaction_traits,
        }
    }

    fn no_repeats(run: &Run) -> bool {
        run.refs().windows(2).all(|w| w[0] != w[1])
    }

    #[test]
    fn cn01_subloop_is_erased_and_elapsed_kept() {
        let (s0, s1, modal, s2) = (
            step(StepKind::Screen, &hash_n(0x10)),
            step(StepKind::Screen, &hash_n(0x11)),
            step(StepKind::Screen, &hash_n(0x12)),
            step(StepKind::Screen, &hash_n(0x13)),
        );
        let t0 = DAY * DAY_MS;
        let points = [pass(t0, &s0), pass(t0 + 5_000, &s1), pass(t0 + 8_000, &modal), pass(t0 + 15_000, &s1), pass(t0 + 20_000, &s2)];
        let direct = vec![s0.clone(), s1.clone(), s2.clone()];
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.refs(), direct);
        assert_eq!(run.course, course_hash(APP, &direct).unwrap(), "모달 왕복을 접으면 직행 코스와 같은 코스");
        assert_eq!(run.splits(), vec![0, 5_000, 20_000], "모달에서 쓴 시간은 다음 구간(S2)에 남는다");
        assert_eq!(run.total_ms, 20_000);
        // 다듬은 결과는 진짜 시작부터의 유효 경과 시간
        assert_eq!(reduce_course_points(&points, &ReduceOptions::default()).unwrap(), vec![(0, s0), (5_000, s1), (20_000, s2)]);
    }

    #[test]
    fn cn02_long_runs_keep_the_origin_the_total_and_route_marks() {
        let (s0, goal) = (step(StepKind::Screen, &hash_n(0x20)), step(StepKind::Screen, &hash_n(0x2f)));
        // 경유지 13개(5초 간격, 영역). 그중 셋째와 아홉째가 고스트의 중간 체크포인트
        let waypoints: Vec<StepRef> = (1..=13).map(|k| step(StepKind::Region, &hash_n(0x200 + k))).collect();
        let (g1, g2) = (waypoints[2].clone(), waypoints[8].clone());
        let ghost = vec![s0.clone(), g1.clone(), g2.clone(), goal.clone()];
        let t0 = DAY * DAY_MS;
        let mut points = vec![pass(t0, &s0)];
        points.extend(waypoints.iter().enumerate().map(|(k, w)| pass(t0 + 5_000 * (k as i64 + 1), w)));
        points.push(pass(t0 + 95_000, &goal));
        assert_eq!(points.len(), 15);

        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.steps.len(), MAX_STEPS);
        assert_eq!((run.steps[0].hash.as_str(), run.steps[0].split_ms), (s0.hash.as_str(), 0));
        assert_eq!(run.total_ms, 95_000, "앞을 잘라 빨라지지 않는다");
        assert!(no_repeats(&run));
        assert!(route_marks(&run.refs(), &ghost).is_some(), "시작 · 결승이 남아 고스트 판정이 된다");

        // 따라간 고스트의 체크포인트를 알려 주면 솎을 때 먼저 남긴다: G1 · G2 모두 지나간 것으로 판정된다
        let opts = ReduceOptions {
            ghost_steps: &ghost,
            ..ReduceOptions::default()
        };
        let run = Run::from_checkpoints_with(APP, &points, None, &opts).unwrap();
        assert_eq!((run.steps.len(), run.total_ms), (MAX_STEPS, 95_000));
        assert_eq!(route_marks(&run.refs(), &ghost), Some(vec![(g1, MarkType::Premium), (g2, MarkType::Premium)]));
    }

    #[test]
    fn cn03_committed_loops_survive_and_thinning_never_repeats_a_checkpoint() {
        let (hub, shop, boss) = (step(StepKind::Screen, &hash_n(0x30)), step(StepKind::Screen, &hash_n(0x31)), step(StepKind::Screen, &hash_n(0x32)));
        let t0 = DAY * DAY_MS;
        let mut points = vec![pass(t0, &hub), committed(t0 + 10_000, shop.kind, &shop.hash), pass(t0 + 20_000, &hub), pass(t0 + 30_000, &boss)];
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.refs(), vec![hub.clone(), shop.clone(), hub.clone(), boss.clone()], "상점에서 사고 돌아온 왕복은 접지 않는다");
        assert_eq!(run.splits(), vec![0, 10_000, 20_000, 30_000]);
        // 확정이 없으면 같은 왕복은 곁길이다
        points[1].committed = false;
        assert_eq!(Run::from_checkpoints(APP, &points, None).unwrap().refs(), vec![hub.clone(), boss.clone()]);

        // 13개짜리 런: 오래 머물러(79초) 접지 않은 곁길 x → y → x 가 있어, 중요도 순으로만 8개를 고르면 x 가 잇달아 남는다
        points[1].committed = true;
        let (x, y, finish) = (step(StepKind::Screen, &hash_n(0x33)), step(StepKind::Region, &hash_n(0x34)), step(StepKind::Screen, &hash_n(0x3f)));
        points.extend([pass(t0 + 40_000, &x), pass(t0 + 41_000, &y), pass(t0 + 120_000, &x)]);
        points.extend((1..=5).map(|k| pass(t0 + 120_000 + 1_000 * k as i64, &step(StepKind::Region, &hash_n(0x340 + k)))));
        points.push(pass(t0 + 130_000, &finish));
        assert_eq!(points.len(), 13);
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.steps.len(), MAX_STEPS);
        assert!(run.refs().contains(&shop), "상태를 확정한 체크포인트는 솎지 않는다");
        assert!(no_repeats(&run), "{:?}", run.refs());
        assert_eq!((&run.refs()[0], &run.refs()[MAX_STEPS - 1]), (&hub, &finish));
        assert_eq!(run.total_ms, 130_000);
        assert!(run.splits().windows(2).all(|w| w[0] <= w[1] && w[1] % SPLIT_UNIT_MS == 0));
    }

    // 서버(workers/www/src/navtrials.ts parseRun)의 규칙: 체크포인트 2 ~ 10개, 첫 구간 0, 100 ms 단위로 줄지 않는 구간, 총 시간 = 마지막
    // 구간, 같은 체크포인트가 잇달아 오지 않음, 코스 id 체인을 다시 계산하면 같음
    #[test]
    fn server_rules_hold_for_messy_long_runs() {
        let mut seed: u64 = 7;
        let mut draw = |m: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        for _ in 0..50 {
            let mut t = DAY * DAY_MS;
            let points: Vec<Checkpoint> = (0..60)
                .map(|_| {
                    t += 100 + draw(20_000) as i64;
                    let kind = [StepKind::Screen, StepKind::Region, StepKind::Page, StepKind::Element][draw(4) as usize];
                    let gesture = [None, None, Some(Gesture::ScrollDown), Some(Gesture::ScrollUp), Some(Gesture::SwipeLeft)][draw(5) as usize];
                    Checkpoint {
                        committed: draw(10) == 0,
                        ..Checkpoint::new(t, kind, hash_n(1 + draw(6) as usize), gesture)
                    }
                })
                .collect();
            let start = points[0].at_ms;
            let lock = [(start + 30_000, start + 90_000), (start + 60_000, start + 200_000)];
            let opts = ReduceOptions {
                frozen_spans: &lock,
                ..ReduceOptions::default()
            };
            let run = Run::from_checkpoints_with(APP, &points, None, &opts).unwrap();
            let splits = run.splits();
            assert!((2..=MAX_STEPS).contains(&run.steps.len()));
            assert_eq!(splits[0], 0);
            assert!(splits.windows(2).all(|w| w[0] <= w[1]) && splits.iter().all(|s| s % SPLIT_UNIT_MS == 0));
            assert_eq!(run.total_ms, *splits.last().unwrap());
            assert!(no_repeats(&run), "{:?}", run.refs());
            assert_eq!(run.steps.iter().map(|s| s.course.clone()).collect::<Vec<_>>(), course_chain(APP, &run.refs()).unwrap());
            assert_eq!(run.course, course_hash(APP, &run.refs()).unwrap());
        }
    }

    #[test]
    fn ps11_an_auth_lock_is_bridged_and_the_finish_day_kept() {
        let t0 = DAY * DAY_MS + DAY_MS - 10_000; // 자정 10초 전 출발
        let points = [cp(t0, StepKind::Screen, S1), cp(t0 + 8_000, StepKind::Region, R2), cp(t0 + 55_000, StepKind::Screen, S3)];
        // 본인인증 잠금 45초 (겹친 두 구간으로 와도 한 번만 센다)
        let lock = [(t0 + 35_000, t0 + 54_000), (t0 + 9_000, t0 + 40_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        let run = Run::from_checkpoints_with(APP, &points, Some(MarkType::Premium), &opts).unwrap();
        assert_eq!(run.splits(), vec![0, 8_000, 10_000]);
        assert_eq!(run.total_ms, 10_000);
        assert_eq!(run.steps.iter().map(|s| s.course.as_str()).collect::<Vec<_>>(), CHAIN, "한 런으로 이어진다");
        assert_eq!(run.course, COURSE);
        assert_eq!(run.day, DAY + 1, "결승 날짜는 실제 결승 시각(자정 45초 뒤)");
        // 시계를 멈추지 않으면 잠금 시간이 구간에 들어간다
        assert_eq!(Run::from_checkpoints(APP, &points, None).unwrap().splits(), vec![0, 8_000, 55_000]);

        // 출발 + 유효 시간(10초)은 자정 전이어도 실제 결승이 자정 뒤면 다음 날이다
        let t1 = DAY * DAY_MS + DAY_MS - 20_000;
        let late = [cp(t1, StepKind::Screen, S1), cp(t1 + 8_000, StepKind::Region, R2), cp(t1 + 55_000, StepKind::Screen, S3)];
        let lock = [(t1 + 9_000, t1 + 54_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        let run = Run::from_checkpoints_with(APP, &late, None, &opts).unwrap();
        assert_eq!((run.total_ms, run.day), (10_000, DAY + 1));

        // 잠금이 체크포인트를 가로지르면 각 구간에서 겹친 만큼만 빠진다 (6초 ~ 50초 잠금: S2 까지 2초, S3 까지 44초)
        let lock = [(t0 + 6_000, t0 + 50_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        assert_eq!(Run::from_checkpoints_with(APP, &points, None, &opts).unwrap().splits(), vec![0, 6_000, 11_000]);
    }

    #[test]
    fn frozen_time_counts_neither_against_the_hour_nor_the_detour() {
        // 20분 본인인증이 낀 70분짜리 런: 유효 시간이 50분이라 받는다
        let points = [cp(0, StepKind::Screen, S1), cp(4_200_000, StepKind::Screen, S3)];
        assert_eq!(Run::from_checkpoints(APP, &points, None), Err(TrialError::TooLong));
        let lock = [(1_000, 1_201_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        assert_eq!(Run::from_checkpoints_with(APP, &points, None, &opts).unwrap().total_ms, 3_000_000);

        // 곁길에 머문 90초는 접지 않지만, 그중 40초가 잠금이면 50초라 접는다
        let (a, b, c) = (step(StepKind::Screen, &hash_n(0x50)), step(StepKind::Screen, &hash_n(0x51)), step(StepKind::Screen, &hash_n(0x52)));
        let points = [pass(0, &a), pass(10_000, &b), pass(100_000, &a), pass(110_000, &c)];
        assert_eq!(Run::from_checkpoints(APP, &points, None).unwrap().refs(), vec![a.clone(), b.clone(), a.clone(), c.clone()]);
        let lock = [(20_000, 60_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        let run = Run::from_checkpoints_with(APP, &points, None, &opts).unwrap();
        assert_eq!(run.refs(), vec![a.clone(), c.clone()]);
        assert_eq!(run.splits(), vec![0, 70_000]);
        // 곁길을 접는 시간은 바꿀 수 있다
        let opts = ReduceOptions {
            max_detour_loop_ms: 120_000,
            ..ReduceOptions::default()
        };
        assert_eq!(Run::from_checkpoints_with(APP, &points, None, &opts).unwrap().refs(), vec![a, c]);
    }

    #[test]
    fn frozen_spans_are_clipped_sorted_and_counted_once() {
        // [0, 100] 안: (0, 5) + (10, 40) + (90, 100). 끝이 시작보다 앞선 구간은 버린다
        assert_eq!(frozen_within(0, 100, &[(10, 30), (90, 200), (20, 40), (-50, 5), (70, 60)]), 45);
        assert_eq!(frozen_within(50, 40, &[(0, 100)]), 0);
        assert_eq!(frozen_within(0, 100, &[]), 0);
        let spans = [(10, 20), (15, 30)];
        let walk: Vec<i64> = (0..=40).step_by(5).map(|t| effective(0, t, &spans)).collect();
        assert_eq!(walk, vec![0, 5, 10, 10, 10, 10, 10, 15, 20]);
    }

    #[test]
    fn a_round_trip_back_to_the_start_stays_a_run() {
        let (a, b) = (step(StepKind::Screen, &hash_n(0x60)), step(StepKind::Screen, &hash_n(0x61)));
        let run = Run::from_checkpoints(APP, &[pass(0, &a), pass(5_000, &b), pass(9_000, &a)], None).unwrap();
        assert_eq!(run.refs(), vec![a.clone(), b.clone(), a.clone()], "결승이 시작이면 접지 않는다 (접으면 런이 없어진다)");
        // 결승 전에 시작으로 돌아온 것은 곁길이다
        let run = Run::from_checkpoints(APP, &[pass(0, &a), pass(5_000, &b), pass(9_000, &a), pass(12_000, &step(StepKind::Screen, S3))], None).unwrap();
        assert_eq!((run.steps.len(), run.total_ms), (2, 12_000));
    }

    #[test]
    fn rewound_scrolls_collapse_to_the_main_direction() {
        let base = [
            gcp(0, StepKind::Page, 'a', None),
            gcp(1_000, StepKind::Page, 'a', Some(Gesture::ScrollDown)),
            gcp(2_000, StepKind::Element, 'b', None),
            gcp(3_000, StepKind::Page, 'd', None),
        ];
        let direct = Run::from_checkpoints(APP, &base, None).unwrap();
        // 내린 뒤 300 ms 만에 살짝 올려 위치를 맞췄다: 같은 코스, 처음 내린 시각
        let mut rewound = base.to_vec();
        rewound.insert(2, gcp(1_300, StepKind::Page, 'a', Some(Gesture::ScrollUp)));
        let run = Run::from_checkpoints(APP, &rewound, None).unwrap();
        assert_eq!(run.course, direct.course);
        assert_eq!(run.splits(), vec![0, 1_000, 2_000, 3_000]);
        // 올림이 더 많이 찍혔으면 올림이 주된 방향이다
        let mut up = rewound.clone();
        up.insert(3, gcp(1_600, StepKind::Page, 'a', Some(Gesture::ScrollUp)));
        assert_eq!(Run::from_checkpoints(APP, &up, None).unwrap().refs()[1].gesture, Some(Gesture::ScrollUp));
        // 400 ms 를 넘겨 반대로 스크롤했으면 따로 남긴다
        let mut apart = base.to_vec();
        apart.insert(2, gcp(1_500, StepKind::Page, 'a', Some(Gesture::ScrollUp)));
        assert_eq!(Run::from_checkpoints(APP, &apart, None).unwrap().steps.len(), 5);
    }

    #[test]
    fn selection_strokes_drop_boundary_swipes_to_a_press() {
        use crate::gesture::{PointerKind, PointerSample};
        let stroke = |pointer: PointerKind, dx: f32, ms: i64| Stroke {
            pointer,
            samples: vec![PointerSample { t_ms: 0, x: 100.0, y: 100.0 }, PointerSample { t_ms: ms, x: 100.0 + dx, y: 100.0 }],
        };
        let p = GestureParams::default();
        // 55 px: 스와이프 기준(50 px)은 넘지만 고르는 획으로는 누름
        assert_eq!(classify_stroke(&stroke(PointerKind::Touch, 55.0, 150), &p), Some(Gesture::SwipeRight));
        assert_eq!(selection_gesture(&stroke(PointerKind::Touch, 55.0, 150), &p), Some(Gesture::Press));
        // 분명한 스와이프 · 누름 · 끌기는 그대로
        assert_eq!(selection_gesture(&stroke(PointerKind::Touch, -120.0, 200), &p), Some(Gesture::SwipeLeft));
        assert_eq!(selection_gesture(&stroke(PointerKind::Mouse, 3.0, 80), &p), Some(Gesture::Press));
        assert_eq!(selection_gesture(&stroke(PointerKind::Mouse, 200.0, 300), &p), None);
    }

    #[test]
    fn committed_is_optional_on_the_wire() {
        let c: Checkpoint = serde_json::from_value(serde_json::json!({ "at_ms": 5, "kind": "screen", "hash": S1, "gesture": null })).unwrap();
        assert_eq!(c, cp(5, StepKind::Screen, S1));
        assert!(serde_json::to_value(&c).unwrap().get("committed").is_none(), "확정 표시가 없으면 칸도 없다");
        let c = committed(5, StepKind::Screen, S1);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["committed"], true);
        assert_eq!(serde_json::from_value::<Checkpoint>(json).unwrap(), c);
    }

    #[test]
    fn guide_follows_through_a_menu_opened_and_closed() {
        let mut c = Collection::default();
        c.add(&lap([1000, 2000], Some(MarkType::Premium)));
        let t0 = 5 * DAY_MS;
        // S1 → 메뉴 → S1 → R2: 메뉴 왕복은 곁길이라 코스 앞부분(S1 → R2)과 맞는다
        let progress = [
            cp(t0, StepKind::Screen, S1),
            cp(t0 + 400, StepKind::Screen, &hash_n(0x99)),
            cp(t0 + 900, StepKind::Screen, S1),
            cp(t0 + 1_500, StepKind::Region, R2),
        ];
        let g = c.guide(APP, &progress, &[]).unwrap();
        assert_eq!((g.passed, g.next), (2, Some(step(StepKind::Screen, S3))));
        assert_eq!(g.vs_best, Some(500), "메뉴에서 쓴 시간은 R2 까지의 시간에 남는다");
    }

    #[test]
    fn guide_live_agrees_with_guide_on_a_plain_course() {
        let mut c = Collection::default();
        c.add(&lap([1000, 2000], Some(MarkType::Premium)));
        let empty = Collection::default();
        let t0 = 5 * DAY_MS;
        let mut others = vec![public(
            vec![step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, S3)],
            42,
            20,
            4,
            vec![0, 800, 1500],
            vec![0, 1400, 3500],
        )];
        others[0].checkpoints[1] = CheckpointTally { premium: 1, discount: 3 };
        let progress = [cp(t0, StepKind::Screen, S1), cp(t0 + 1_300, StepKind::Region, R2), cp(t0 + 2_900, StepKind::Screen, S3)];
        for n in 1..=progress.len() {
            for collection in [&c, &empty] {
                let live = collection.guide_live(APP, &progress[..n], &others, progress[n - 1].at_ms, &ReduceOptions::default()).unwrap();
                assert_eq!(Some(live.base.clone()), collection.guide(APP, &progress[..n], &others));
                assert_eq!(live.sync_state, GhostSyncState::ExactPrefix);
                assert_eq!((live.upgrade_over_habit_ms, live.alt_next.clone(), live.live_overrun_ms), (None, None, 0));
                assert_eq!(live.tracked_public_ghost.as_deref(), Some(COURSE));
            }
        }
        assert_eq!(c.guide_live(APP, &[cp(t0, StepKind::Screen, S3)], &others, t0, &ReduceOptions::default()), None);
        assert_eq!(c.guide_live(APP, &[], &others, t0, &ReduceOptions::default()), None);
    }

    #[test]
    fn cn07_public_premium_shortcut_beats_my_habit() {
        let (s0, m1, p1, p2, goal) = (
            step(StepKind::Screen, &hash_n(0x70)),
            step(StepKind::Region, &hash_n(0x71)),
            step(StepKind::Region, &hash_n(0x72)),
            step(StepKind::Region, &hash_n(0x73)),
            step(StepKind::Screen, &hash_n(0x7f)),
        );
        // 내 습관 코스: 3번 달렸고 discount, 최고 30초
        let habit = vec![s0.clone(), m1.clone(), goal.clone()];
        let mut c = Collection::default();
        c.apps.insert(
            APP.into(),
            vec![CourseEntry {
                course: course_hash(APP, &habit).unwrap(),
                steps: habit,
                runs: 3,
                premium: 0,
                discount: 3,
                best: vec![0, 14_000, 30_000],
                last: vec![0, 15_000, 31_000],
                last_day: DAY,
                touched: 1,
                pinned: false,
            }],
        );
        let mine = c.courses(APP)[0].course.clone();
        // 인기 함정 코스(40명, discount, 평균 28초)와 빠른 premium 코스(15명, 평균 12초)
        let trap = public(vec![s0.clone(), p1.clone(), goal.clone()], 40, 2, 20, vec![0, 8_000, 20_000], vec![0, 12_000, 28_000]);
        let fast = public(vec![s0.clone(), p2.clone(), goal.clone()], 15, 12, 1, vec![0, 3_000, 9_000], vec![0, 5_000, 12_000]);
        assert!(ghost_utility(&fast) > ghost_utility(&trap) && trap.runs > fast.runs);
        let t0 = 7 * DAY_MS;
        let start = [cp(t0, StepKind::Screen, &s0.hash)];
        let live = c.guide_live(APP, &start, &[trap.clone(), fast.clone()], t0, &ReduceOptions::default()).unwrap();
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(fast.course.as_str()), "discount 인 인기 코스는 고스트가 되지 않는다");
        assert_eq!(live.my_best_course.as_deref(), Some(mine.as_str()));
        assert_eq!(live.base.next, Some(p2.clone()));
        assert_eq!(live.base.course, fast.course);
        assert_eq!((live.base.vs_best, live.base.vs_record, live.base.vs_average), (Some(0), Some(0), Some(0)));
        assert_eq!(live.upgrade_over_habit_ms, Some(18_000));
        assert_eq!(
            (live.sync_state.clone(), live.base.tier, live.base.my_runs, live.base.all_runs),
            (GhostSyncState::ExactPrefix, Tier::Premium, 3, Some(15))
        );
        assert_eq!(live.alt_next, None, "효용 점수가 크게 벌어져 박빙이 아니다");

        // 공개 코스가 15 % 넘게 빠르지 않으면(30초 → 27초) 내 습관 코스를 그대로 안내한다
        let slow = public(vec![s0.clone(), p2.clone(), goal.clone()], 15, 12, 1, vec![0, 9_000, 25_000], vec![0, 11_000, 27_000]);
        let live = c.guide_live(APP, &start, &[trap.clone(), slow.clone()], t0, &ReduceOptions::default()).unwrap();
        assert_eq!(live.upgrade_over_habit_ms, None);
        assert_eq!((live.base.course.as_str(), live.base.next.clone()), (mine.as_str(), Some(m1.clone())));
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(slow.course.as_str()));
        // 결승이 다른 코스는 견주지 않는다
        let elsewhere = public(
            vec![s0.clone(), p2.clone(), step(StepKind::Screen, &hash_n(0x7e))],
            15,
            12,
            1,
            vec![0, 3_000, 9_000],
            vec![0, 5_000, 12_000],
        );
        let live = c.guide_live(APP, &start, &[elsewhere], t0, &ReduceOptions::default()).unwrap();
        assert_eq!((live.upgrade_over_habit_ms, live.base.next), (None, Some(m1)));
    }

    #[test]
    fn cn08_detour_keeps_the_ghost_and_rejoin_resumes_splits() {
        let (s0, g1, g2, goal, alt) = (
            step(StepKind::Screen, &hash_n(0x80)),
            step(StepKind::Region, &hash_n(0x81)),
            step(StepKind::Region, &hash_n(0x82)),
            step(StepKind::Screen, &hash_n(0x8f)),
            step(StepKind::Region, &hash_n(0x8a)),
        );
        let ghost = public(
            vec![s0.clone(), g1.clone(), g2.clone(), goal.clone()],
            20,
            9,
            1,
            vec![0, 4_000, 10_000, 18_000],
            vec![0, 5_000, 12_000, 20_000],
        );
        let others = [ghost.clone()];
        let c = Collection::default();
        let t0 = 9 * DAY_MS;
        let opts = ReduceOptions::default();

        // Alt(3초): 곁길. 고스트는 그대로 두고, 다음은 아직 지나지 않은 G1
        let mut progress = vec![pass(t0, &s0), pass(t0 + 3_000, &alt)];
        let live = c.guide_live(APP, &progress, &others, t0 + 3_000, &opts).unwrap();
        assert_eq!(
            live.sync_state,
            GhostSyncState::Detouring {
                last_matched_index: 0,
                rejoin_hint: Some(g1.clone())
            }
        );
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(ghost.course.as_str()));
        assert_eq!((live.base.next.clone(), live.base.vs_average), (Some(g1.clone()), Some(0)));
        assert_eq!(c.guide(APP, &progress, &others), None, "앞부분만 맞추는 안내는 곁길에서 끊긴다");

        // G2(9초): 다시 들어섰다. 고스트의 G2 자리에서 다시 견준다
        progress.push(pass(t0 + 9_000, &g2));
        let live = c.guide_live(APP, &progress, &others, t0 + 9_000, &opts).unwrap();
        assert_eq!(live.sync_state, GhostSyncState::Rejoined { ghost_index: 2, skipped_steps: 1 });
        assert_eq!((live.base.vs_average, live.base.vs_record), (Some(-3_000), Some(-1_000)));
        assert_eq!((live.base.passed, live.base.length, live.base.next.clone()), (3, 4, Some(goal.clone())));

        // 결승(16초): 런에 따라간 고스트를 적어 보내면 route_marks 가 G1 discount · G2 premium 으로 판정한다
        progress.push(pass(t0 + 16_000, &goal));
        let live = c.guide_live(APP, &progress, &others, t0 + 16_000, &opts).unwrap();
        assert_eq!((live.base.next.clone(), live.sync_state.clone()), (None, GhostSyncState::Rejoined { ghost_index: 3, skipped_steps: 1 }));
        let follow = ReduceOptions { ghost_steps: &ghost.steps, ..opts };
        let mut run = Run::from_checkpoints_with(APP, &progress, Some(MarkType::Premium), &follow).unwrap();
        run.ghost = live.tracked_public_ghost.clone();
        assert_eq!(run.ghost.as_deref(), Some(ghost.course.as_str()));
        assert_eq!(route_marks(&run.refs(), &ghost.steps), Some(vec![(g1.clone(), MarkType::Discount), (g2.clone(), MarkType::Premium)]));
        // 지름길 대결: G1 대신 Alt 로 가서 고스트 평균(20초)보다 4초 빨리 결승했다
        assert_eq!(
            shortcut_wins(&run, &ghost),
            vec![ShortcutWin {
                skipped: g1,
                taken: alt,
                saved_ms: 4_000,
                margin: 0.2,
            }]
        );
    }

    #[test]
    fn cn09_live_overrun_counts_the_stall_without_the_auth_lock() {
        let (s0, g1, g2, goal) = (
            step(StepKind::Screen, &hash_n(0x90)),
            step(StepKind::Region, &hash_n(0x91)),
            step(StepKind::Region, &hash_n(0x92)),
            step(StepKind::Screen, &hash_n(0x9f)),
        );
        let splits = vec![0, 5_000, 10_000, 20_000];
        let ghost = public(vec![s0.clone(), g1.clone(), g2.clone(), goal], 20, 9, 1, splits.clone(), splits);
        let others = [ghost];
        let c = Collection::default();
        let t0 = 11 * DAY_MS;
        let progress = [pass(t0, &s0), pass(t0 + 4_000, &g1)];
        // 10초 ~ 20초 본인인증 잠금, 지금 25초: 유효 경과 15초, 고스트는 10초에 G2
        let lock = [(t0 + 10_000, t0 + 20_000)];
        let opts = ReduceOptions {
            frozen_spans: &lock,
            ..ReduceOptions::default()
        };
        let live = c.guide_live(APP, &progress, &others, t0 + 25_000, &opts).unwrap();
        assert_eq!(live.sync_state, GhostSyncState::ExactPrefix);
        assert_eq!(live.base.vs_record, Some(-1_000), "G1 을 지난 차이는 그대로");
        assert_eq!(live.base.next, Some(g2));
        assert_eq!(live.live_overrun_ms, 5_000);
        // 잠금을 빼지 않으면 15초 늦은 것으로 보인다
        assert_eq!(c.guide_live(APP, &progress, &others, t0 + 25_000, &ReduceOptions::default()).unwrap().live_overrun_ms, 15_000);
        // 도착 예정 전이면 0, 넘긴 뒤로는 100 ms 단위로 내린다
        assert_eq!(c.guide_live(APP, &progress, &others, t0 + 9_950, &opts).unwrap().live_overrun_ms, 0);
        assert_eq!(c.guide_live(APP, &progress, &others, t0 + 20_150, &opts).unwrap().live_overrun_ms, 100);
    }

    #[test]
    fn ghost_steps_pin_the_followed_ghost_through_a_detour() {
        let (s0, g1, g2, goal, alt, x) = (
            step(StepKind::Screen, &hash_n(0xa0)),
            step(StepKind::Region, &hash_n(0xa1)),
            step(StepKind::Region, &hash_n(0xa2)),
            step(StepKind::Screen, &hash_n(0xaf)),
            step(StepKind::Region, &hash_n(0xaa)),
            step(StepKind::Region, &hash_n(0xab)),
        );
        let followed = public(vec![s0.clone(), g1.clone(), g2, goal.clone()], 20, 9, 1, vec![0, 4_000, 10_000, 18_000], vec![0, 5_000, 12_000, 20_000]);
        let other = public(vec![s0.clone(), alt.clone(), x, goal], 30, 20, 1, vec![0, 1_000, 4_000, 8_000], vec![0, 2_000, 6_000, 10_000]);
        let both = [followed.clone(), other.clone()];
        let progress = [pass(0, &s0), pass(3_000, &alt)];
        let live = Collection::default().guide_live(APP, &progress, &both, 3_000, &ReduceOptions::default()).unwrap();
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(other.course.as_str()), "곁길이 다른 공개 코스의 앞부분이면 그 코스로 옮긴다");
        let opts = ReduceOptions {
            ghost_steps: &followed.steps,
            ..ReduceOptions::default()
        };
        let live = Collection::default().guide_live(APP, &progress, &both, 3_000, &opts).unwrap();
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(followed.course.as_str()), "따라가던 고스트는 곁길에서도 그대로");
        assert_eq!(
            live.sync_state,
            GhostSyncState::Detouring {
                last_matched_index: 0,
                rejoin_hint: Some(g1)
            }
        );
    }

    #[test]
    fn guide_live_follows_the_course_the_run_is_actually_on() {
        let (s0, m1, x, y, goal) = (
            step(StepKind::Screen, &hash_n(0xd0)),
            step(StepKind::Region, &hash_n(0xd1)),
            step(StepKind::Region, &hash_n(0xd2)),
            step(StepKind::Region, &hash_n(0xd3)),
            step(StepKind::Screen, &hash_n(0xdf)),
        );
        let habit = vec![s0.clone(), m1.clone(), goal.clone()];
        let mut c = Collection::default();
        c.add(&Run::from_checkpoints(APP, &habit.iter().enumerate().map(|(i, s)| pass(i as i64 * 4_000, s)).collect::<Vec<_>>(), None).unwrap());
        let other = public(vec![s0.clone(), x.clone(), y.clone(), goal], 12, 3, 3, vec![0, 2_000, 5_000, 9_000], vec![0, 3_000, 6_000, 10_000]);
        // 처음에는 내 습관 코스로 안내하고, X 로 들어서면(내 코스에서는 곁길) 맞는 공개 코스로 안내한다
        let live = c.guide_live(APP, &[pass(0, &s0)], &[other.clone()], 0, &ReduceOptions::default()).unwrap();
        assert_eq!(live.base.next, Some(m1.clone()));
        let live = c.guide_live(APP, &[pass(0, &s0), pass(2_500, &x)], &[other.clone()], 2_500, &ReduceOptions::default()).unwrap();
        assert_eq!(
            (live.base.course.as_str(), live.base.next.clone(), live.sync_state.clone()),
            (other.course.as_str(), Some(y), GhostSyncState::ExactPrefix)
        );
        assert_eq!(live.my_best_course.as_deref(), Some(c.courses(APP)[0].course.as_str()));
        assert_eq!(
            (live.base.vs_best, live.base.vs_record, live.base.my_runs),
            (Some(0), Some(500), 1),
            "내 최고는 마지막으로 지난 S0 에서 견준다"
        );
    }

    #[test]
    fn a_close_second_branch_is_offered_as_alt_next() {
        let (s0, goal) = (step(StepKind::Screen, &hash_n(0xb0)), step(StepKind::Screen, &hash_n(0xbf)));
        let branch = |n: usize| step(StepKind::Region, &hash_n(0xb0 + n));
        let fast = public(vec![s0.clone(), branch(1), goal.clone()], 15, 12, 1, vec![0, 3_000, 9_000], vec![0, 5_000, 12_000]);
        let close = public(vec![s0.clone(), branch(2), goal.clone()], 15, 12, 1, vec![0, 3_500, 10_000], vec![0, 5_500, 13_000]);
        let trap = public(vec![s0.clone(), branch(3), goal.clone()], 40, 2, 20, vec![0, 8_000, 20_000], vec![0, 12_000, 28_000]);
        let start = [pass(0, &s0)];
        let live = Collection::default()
            .guide_live(APP, &start, &[trap.clone(), close, fast.clone()], 0, &ReduceOptions::default())
            .unwrap();
        assert_eq!(live.tracked_public_ghost.as_deref(), Some(fast.course.as_str()));
        assert_eq!(live.base.next, Some(branch(1)));
        assert_eq!(live.alt_next, Some((branch(2), Tier::Premium)), "효용 점수가 두 길로 팽팽하다 (PairConfusion)");
        // 둘째 길이 크게 처지면 알리지 않는다
        let unrated = public(vec![s0.clone(), branch(2), goal.clone()], 15, 1, 1, vec![0, 3_500, 50_000], vec![0, 5_500, 60_000]);
        let live = Collection::default().guide_live(APP, &start, &[trap, unrated, fast], 0, &ReduceOptions::default()).unwrap();
        assert_eq!(live.alt_next, None);
    }

    #[test]
    fn shortcut_wins_need_a_faster_premium_finish_on_the_same_route() {
        let (s0, g1, g2, goal) = (
            step(StepKind::Screen, &hash_n(0xc0)),
            step(StepKind::Region, &hash_n(0xc1)),
            step(StepKind::Region, &hash_n(0xc2)),
            step(StepKind::Screen, &hash_n(0xcf)),
        );
        let ghost = public(
            vec![s0.clone(), g1.clone(), g2.clone(), goal.clone()],
            20,
            9,
            1,
            vec![0, 4_000, 10_000, 18_000],
            vec![0, 5_000, 12_000, 20_000],
        );
        let run = |route: &[&StepRef], total: i64, kind: Option<MarkType>| {
            let gap = total / (route.len() as i64 - 1);
            let points: Vec<Checkpoint> = route.iter().enumerate().map(|(i, s)| pass(gap * i as i64, s)).collect();
            Run::from_checkpoints(APP, &points, kind).unwrap()
        };
        // G1 을 건너뛰고 바로 G2 로: 갈림길에서 고스트는 G1, 런은 G2
        let wins = shortcut_wins(&run(&[&s0, &g2, &goal], 12_000, Some(MarkType::Premium)), &ghost);
        assert_eq!(
            wins.iter().map(|w| (w.skipped.clone(), w.taken.clone(), w.saved_ms)).collect::<Vec<_>>(),
            vec![(g1.clone(), g2.clone(), 8_000)]
        );
        assert!((wins[0].margin - 0.4).abs() < 1e-6);
        // premium 이 아니거나, 고스트 평균보다 느리거나, 결승이 다르거나, 건너뛴 것이 없으면 대결이 아니다
        assert!(shortcut_wins(&run(&[&s0, &g2, &goal], 12_000, None), &ghost).is_empty());
        assert!(shortcut_wins(&run(&[&s0, &g2, &goal], 24_000, Some(MarkType::Premium)), &ghost).is_empty());
        assert!(shortcut_wins(&run(&[&s0, &g2, &g1], 12_000, Some(MarkType::Premium)), &ghost).is_empty());
        assert!(shortcut_wins(&run(&[&s0, &g2, &g1, &goal], 12_000, Some(MarkType::Premium)), &ghost).is_empty());
    }

    #[test]
    fn cn245_route_marks_v2_blocks_slow_lost_wanderer_and_broken_step_whitewashing() {
        let s0 = step(StepKind::Screen, &hash_n(0xd0));
        let a = step(StepKind::Region, &hash_n(0xd1));
        let b = StepRef { kind: StepKind::Element, hash: hash_n(0xd2), gesture: Some(Gesture::SwipeLeft) };
        let alt = step(StepKind::Region, &hash_n(0xd3));
        let x1 = step(StepKind::Region, &hash_n(0xd4));
        let x2 = step(StepKind::Region, &hash_n(0xd5));
        let finish = step(StepKind::Screen, &hash_n(0xdf));
        let ghost = vec![s0.clone(), a.clone(), b.clone(), finish.clone()];
        let g_splits = [0u32, 5_000, 10_000, 15_000];

        // ③ 길 잃은 둔속 고마찰 우회(45s, discount): 본선 A, B 모두 Abstain(0) 보호!
        let slow_ctx = RouteEvalContext {
            run_total_ms: 45_000,
            run_kind: Some(MarkType::Discount),
            ghost_avg_ms: 15_000,
            ghost_splits: &g_splits,
            ..Default::default()
        };
        let slow_res = route_marks_v2(&[s0.clone(), x1, x2, finish.clone()], &ghost, &slow_ctx).unwrap();
        assert_eq!(slow_res, vec![(a.clone(), MarkOutcome::Abstain), (b.clone(), MarkOutcome::Abstain)]);

        // ② 고속 지름길 우회(11s, premium): 통과한 A는 Premium(+1), 건너뛴 B는 Discount(-1)!
        let fast_ctx = RouteEvalContext {
            run_total_ms: 11_000,
            run_kind: Some(MarkType::Premium),
            ghost_avg_ms: 15_000,
            ghost_splits: &g_splits,
            ..Default::default()
        };
        let fast_res = route_marks_v2(&[s0.clone(), a.clone(), alt, finish.clone()], &ghost, &fast_ctx).unwrap();
        assert_eq!(fast_res, vec![(a.clone(), MarkOutcome::Premium), (b.clone(), MarkOutcome::Discount)]);

        // ⑤ 국소 병목 억지 돌파(A 순항 bit=0, B 병목 bit 2 켜짐, 전체 discount): A만 +1 보호, B는 Abstain!
        let fault_ctx = RouteEvalContext {
            run_total_ms: 32_000,
            run_kind: Some(MarkType::Discount),
            ghost_avg_ms: 15_000,
            ghost_splits: &g_splits,
            fault_steps_mask: 1u16 << 2,
            ..Default::default()
        };
        let fault_res = route_marks_v2(&[s0, a.clone(), b.clone(), finish], &ghost, &fault_ctx).unwrap();
        assert_eq!(fault_res, vec![(a, MarkOutcome::Premium), (b, MarkOutcome::Abstain)]);
    }

    #[test]
    fn cn247_route_marks_v2_protects_cross_modality_equivalent_pass() {
        let s0 = step(StepKind::Page, &hash_n(0xe0));
        let a = step(StepKind::Element, &hash_n(0xe1));
        let b_swipe = StepRef { kind: StepKind::Element, hash: hash_n(0xe2), gesture: Some(Gesture::SwipeLeft) };
        let b_click = StepRef { kind: StepKind::Element, hash: hash_n(0xe2), gesture: None };
        let finish = step(StepKind::Page, &hash_n(0xef));
        let ghost = vec![s0.clone(), a.clone(), b_swipe.clone(), finish.clone()];
        let ctx = RouteEvalContext {
            run_total_ms: 13_800,
            run_kind: Some(MarkType::Premium),
            ghost_avg_ms: 15_000,
            ..Default::default()
        };
        let res = route_marks_v2(&[s0, a.clone(), b_click, finish], &ghost, &ctx).unwrap();
        assert_eq!(res, vec![(a, MarkOutcome::Premium), (b_swipe, MarkOutcome::EquivalentPass)]);
        assert_eq!(res[1].1.sign(), None, "no cross-modality discount");
    }

    #[test]
    fn cn249_shortcut_wins_v2_window_local_splits_block_free_rider_and_rescue_late_shortcut() {
        let s0 = step(StepKind::Screen, &hash_n(0xf0));
        let g1 = step(StepKind::Region, &hash_n(0xf1));
        let mid = step(StepKind::Screen, &hash_n(0xf2));
        let g2 = step(StepKind::Region, &hash_n(0xf3));
        let goal = step(StepKind::Screen, &hash_n(0xff));
        let alt1 = step(StepKind::Region, &hash_n(0xf4));
        let alt2 = step(StepKind::Region, &hash_n(0xf5));
        // Ghost: S0(0s) -> G1(5s) -> Mid(10s) -> G2(20s) -> Goal(30s)
        let ghost = public(
            vec![s0.clone(), g1.clone(), mid.clone(), g2.clone(), goal.clone()],
            20,
            15,
            1,
            vec![0, 4_000, 9_000, 18_000, 27_000],
            vec![0, 5_000, 10_000, 20_000, 30_000],
        );
        // Case A (Free-rider check): Window 1 via alt1 LOSES +3s (13s vs 10s), Window 2 via alt2 SAVES -10s (10s vs 20s); total 23s (-7s), kind=None (clean UI!).
        let run_a = Run::from_checkpoints(
            APP,
            &[pass(0, &s0), pass(6_000, &alt1), pass(13_000, &mid), pass(18_000, &alt2), pass(23_000, &goal)],
            None,
        )
        .unwrap();
        let wins_a = shortcut_wins_v2(&run_a, &ghost, 0, 1.0);
        assert_eq!(wins_a.len(), 1, "free-rider alt1 must be rejected, only alt2 wins");
        assert_eq!(wins_a[0].taken, alt2);
        assert_eq!(wins_a[0].skipped, g2);
        assert_eq!(wins_a[0].saved_ms, 10_000);
        assert_eq!(wins_a[0].kind, ShortcutKind::AltBranch);
        assert_eq!(wins_a[0].taken_id, alt2.id().unwrap());

        // Case B (Sunk-delay rescue): Window 1 is +8s slower (18s vs 10s), Window 2 saves -6s (14s vs 20s); total 32s (> 30s ghost avg!).
        let run_b = Run::from_checkpoints(
            APP,
            &[pass(0, &s0), pass(10_000, &g1), pass(18_000, &mid), pass(25_000, &alt2), pass(32_000, &goal)],
            None,
        )
        .unwrap();
        let wins_b = shortcut_wins_v2(&run_b, &ghost, 0, 1.0);
        assert_eq!(wins_b.len(), 1, "late-stage genuine shortcut must be rescued despite early delay");
        assert_eq!(wins_b[0].taken, alt2);
        assert_eq!(wins_b[0].saved_ms, 6_000);

        // Case C (DirectBypass): skipping G2 straight from Mid -> Goal separates DirectBypass from AltBranch.
        let run_c = Run::from_checkpoints(
            APP,
            &[pass(0, &s0), pass(5_000, &g1), pass(10_000, &mid), pass(22_000, &goal)],
            None,
        )
        .unwrap();
        let wins_c = shortcut_wins_v2(&run_c, &ghost, 0, 1.0);
        assert_eq!(wins_c.len(), 1);
        assert_eq!(wins_c[0].kind, ShortcutKind::DirectBypass);
        assert_eq!(wins_c[0].saved_ms, 8_000);
    }

    #[test]
    fn cn259_ghost_utility_v3_ranks_grade_and_rank_and_demotes_superseded() {
        let s0 = step(StepKind::Screen, &hash_n(0x90));
        let a = step(StepKind::Region, &hash_n(0x91));
        let b = step(StepKind::Region, &hash_n(0x92));
        let goal = step(StepKind::Screen, &hash_n(0x9f));

        let mut strong = public(vec![s0.clone(), a.clone(), goal.clone()], 15, 13, 1, vec![0, 4_000, 10_000], vec![0, 5_000, 12_000]);
        strong.verdict = Some(VerdictSummary {
            tier: Tier::Premium,
            grade: Some(2),
            rank: Some(0.92),
            evidence: 5.5,
            reasons: vec!["rank".into()],
            recipe: RECIPE_V2.into(),
        });

        let mut held = public(vec![s0.clone(), b.clone(), goal.clone()], 15, 13, 1, vec![0, 4_000, 10_000], vec![0, 5_000, 12_000]);
        held.verdict = Some(VerdictSummary {
            tier: Tier::Premium,
            grade: Some(1),
            rank: Some(0.32),
            evidence: 4.0,
            reasons: vec!["held".into()],
            recipe: RECIPE_V2.into(),
        });

        let mut old_route = public(vec![s0.clone(), a.clone(), b.clone(), goal.clone()], 40, 30, 5, vec![0, 4_000, 12_000, 20_000], vec![0, 5_000, 14_000, 22_000]);
        assert!(is_course_superseded(&old_route, &strong, 0, 0, false), "strict subset shortcut supersedes old route");
        old_route.verdict = Some(VerdictSummary {
            tier: Tier::Discount,
            grade: Some(-2),
            rank: Some(-0.8),
            evidence: 8.0,
            reasons: vec!["superseded".into()],
            recipe: RECIPE_V2.into(),
        });

        assert!(ghost_utility_v3(&strong) > ghost_utility_v3(&held));
        assert!(ghost_utility_v3(&old_route) < -300.0);
    }

    #[test]
    fn cn263_pinned_slru_protects_practiced_and_pinned_courses_from_one_shot_exploration() {
        let mut c = Collection::default();
        let make_run = |id: u8, day: i64| {
            let s0 = step(StepKind::Screen, &hash_n(0x60));
            let mid = step(StepKind::Region, &hash_n(usize::from(id)));
            let end = step(StepKind::Screen, &hash_n(0x6f));
            Run::from_checkpoints(APP, &[pass(day * DAY_MS, &s0), pass(day * DAY_MS + 2_000, &mid), pass(day * DAY_MS + 5_000, &end)], Some(MarkType::Premium)).unwrap()
        };
        // 5 practiced core courses (10 runs each).
        let mut core_ids = Vec::new();
        for k in 1..=5u8 {
            let r = make_run(k, 10);
            core_ids.push(r.course.clone());
            for _ in 0..10 {
                c.add(&r);
            }
        }
        // Pin one 1-run course as well.
        let pinned_run = make_run(99, 10);
        c.add(&pinned_run);
        assert!(c.pin_course(APP, &pinned_run.course, true));

        // Now add 10 brand-new one-shot exploratory courses (1 run each).
        for k in 20..30u8 {
            c.add(&make_run(k, 11));
        }
        assert_eq!(c.courses(APP).len(), MAX_COURSES);
        for cid in &core_ids {
            assert!(c.courses(APP).iter().any(|e| &e.course == cid), "practiced core course {cid} must never be evicted");
        }
        assert!(c.courses(APP).iter().any(|e| e.course == pinned_run.course && e.pinned), "pinned course must never be evicted");
    }

    #[test]
    fn ac03_personal_pace_normalized_shortcut_win_and_accessible_trait_routing() {
        let s0 = step(StepKind::Screen, &hash_n(0xa0));
        let g1 = StepRef { kind: StepKind::Element, hash: hash_n(0xa1), gesture: Some(Gesture::SwipeLeft) };
        let alt = step(StepKind::Element, &hash_n(0xa2));
        let goal = step(StepKind::Screen, &hash_n(0xaf));

        let ghost = public(
            vec![s0.clone(), g1.clone(), goal.clone()],
            30,
            20,
            2,
            vec![0, 7_000, 14_000],
            vec![0, 8_000, 15_000],
        );
        // Switch user has personal pace ratio ρ_u = 3.0.
        let mut pace = PersonalPaceProfile::default();
        pace.observe_segment(24_000, 8_000);
        pace.observe_segment(21_000, 7_000);
        pace.observe_segment(30_000, 10_000);
        assert!((pace.ratio() - 3.0).abs() < 1e-6);

        // User completes clean accessible bypass in 33s (> 15s raw, but 33/3 = 11s < 15s normalized -> 26.7% speedup!).
        let run = Run::from_checkpoints(APP, &[pass(0, &s0), pass(15_000, &alt), pass(33_000, &goal)], None).unwrap();
        let wins = shortcut_wins_v2(&run, &ghost, 0, pace.ratio());
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].taken, alt);
        assert_eq!(wins[0].saved_ms, 4_000);
        assert!((wins[0].margin - (4.0 / 15.0)).abs() < 0.01);

        // Accessible routing boosts NO_FINE_GESTURE + TREE_ACCESSIBLE course over swipe-based ghost.
        let accessible_course = public(vec![s0, alt, goal], 15, 12, 1, vec![0, 8_000, 16_000], vec![0, 9_000, 17_000]);
        assert_eq!(ghost.interaction_traits & TRAIT_NO_FINE_GESTURE, 0);
        assert_ne!(accessible_course.interaction_traits & TRAIT_NO_FINE_GESTURE, 0);
        let prefs = AccessibilityRoutePrefs {
            require_tree_accessible: true,
            prefer_no_fine_gesture: true,
            prefer_calm: true,
        };
        assert!(contextual_ghost_utility(&accessible_course, &prefs) > contextual_ghost_utility(&ghost, &prefs));
    }
}
