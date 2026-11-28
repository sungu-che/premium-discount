//! PD-Verdict: 평가(또는 화면 · 영역) 하나를 같은 스코프 안에서 premium(추천) / discount(비추천)으로 판정한다.
//!
//! 규칙의 기준은 premium-discount 의 계약이다. `contracts/gen_verdict_v1.py` · `gen_verdict_v2.py`(참조 구현)가
//! `contracts/verdict_v1.json` · `verdict_v2.json`(매개변수 + 사례 + 기대값)을 만들고, 구현들은 그 사례를 그대로 통과한다.
//! 서버 cron(TypeScript)은 투표로 전역 판정을, 이 crate 는 앱과 raon-zena 의 로컬 판정을 낸다.
//!
//! 1. 관측마다 `u = weight × credit × 2^(−age / H)` (`age = now − at_ms`, 음수는 0). 철회(`sign` 0)는 세지 않는다.
//! 2. `E = Σu`, `v = Σ(u × sign) / (E + k)`.
//! 3. 기준선: secondary → primary → global 중 `n ≥ min_obs` 인 첫 단계. 있으면 `r = tanh((v − mean) / (sd + eps))`.
//! 4. 등급: 대체됨 → discount, `E < min_evidence` → unrated, premium · discount 들어가기, 직전 등급 머무르기
//!    (히스테리시스), 나머지 neutral.
//! 5. grade: premium +2(`r ≥ strong`) 또는 +1, neutral 0, discount −2(대체됨 또는 `r ≤ −strong`) 또는 −1, unrated 없음.
//!
//! v2(`pd-verdict/2`)는 규칙이 같고 매개변수만 넓다: 관측 종류 [`IMPLICIT`](접근성 행동 로그, 가중치 0.3)와
//! 트랙 [`NAV`](화면 · 영역, web 과 같은 값)가 더해졌다. 판정 행은 recipe 를 남기고, recipe 가 다른 판정끼리는 비교하지 않는다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const RECIPE_V1: &str = "pd-verdict/1";
pub const RECIPE_V2: &str = "pd-verdict/2";

/// 접근성 내비게이션의 행동 로그가 에피소드마다 남기는 관측 종류 (v2)
pub const IMPLICIT: &str = "implicit";
/// 화면 · 영역 판정 트랙 (v2)
pub const NAV: &str = "nav";

const DAY_MS: f64 = 86_400_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Secondary,
    Primary,
    Global,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Secondary, Level::Primary, Level::Global];

    pub fn name(self) -> &'static str {
        match self {
            Level::Secondary => "secondary",
            Level::Primary => "primary",
            Level::Global => "global",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Premium,
    Neutral,
    Discount,
    Unrated,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Premium => "premium",
            Tier::Neutral => "neutral",
            Tier::Discount => "discount",
            Tier::Unrated => "unrated",
        }
    }
}

/// 관측 하나 (표, 결과 점수, 대화 속 피드백, 행동 로그의 implicit …)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Obs {
    pub kind: String,
    /// +1 premium 쪽, −1 discount 쪽, 0 철회
    pub sign: i32,
    pub weight: f64,
    /// 0..1 (결과 점수의 몫. 표는 1)
    pub credit: f64,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub n: u64,
    pub mean: f64,
    pub sd: f64,
}

/// 스코프 단계마다 판정된 값의 분포 (판정하는 항목 자신도 포함)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Baselines {
    #[serde(default)]
    pub secondary: Option<Baseline>,
    #[serde(default)]
    pub primary: Option<Baseline>,
    #[serde(default)]
    pub global: Option<Baseline>,
}

impl Baselines {
    pub fn get(&self, level: Level) -> Option<Baseline> {
        match level {
            Level::Secondary => self.secondary,
            Level::Primary => self.primary,
            Level::Global => self.global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinObs {
    pub secondary: u64,
    pub primary: u64,
    pub global: u64,
}

impl MinObs {
    pub fn get(&self, level: Level) -> u64 {
        match level {
            Level::Secondary => self.secondary,
            Level::Primary => self.primary,
            Level::Global => self.global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackParams {
    pub half_life_ms: f64,
    pub min_obs: MinObs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub k: f64,
    pub eps: f64,
    pub enter: f64,
    pub exit: f64,
    pub floor: f64,
    pub strong: f64,
    pub min_evidence: f64,
    pub duel_rate: f64,
    pub duel_min: u32,
    pub tracks: BTreeMap<String, TrackParams>,
    /// 관측 종류별 기본 가중치 (관측을 만드는 쪽이 `Obs::weight` 에 넣는다. 판정 함수는 weight 만 본다)
    pub weights: BTreeMap<String, f64>,
}

impl Params {
    /// `pd-verdict/1` (계약의 `params` 와 같아야 한다. 시험이 비교한다)
    pub fn v1() -> Params {
        let track = |days: f64, secondary, primary, global| TrackParams {
            half_life_ms: days * DAY_MS,
            min_obs: MinObs {
                secondary,
                primary,
                global,
            },
        };
        Params {
            k: 2.0,
            eps: 0.05,
            enter: 0.5,
            exit: 0.3,
            floor: 0.1,
            strong: 0.8,
            min_evidence: 2.0,
            duel_rate: 0.65,
            duel_min: 3,
            tracks: BTreeMap::from([
                ("pd".to_string(), track(90.0, 5, 10, 30)),
                ("drop".to_string(), track(14.0, 6, 3, 10)),
                ("web".to_string(), track(14.0, 8, 4, 12)),
                ("chat".to_string(), track(14.0, 6, 3, 10)),
                ("coach".to_string(), track(14.0, 6, 3, 10)),
            ]),
            weights: BTreeMap::from(
                [
                    ("vote", 1.0),
                    ("vote_outcome", 0.5),
                    ("manual", 2.0),
                    ("feedback", 1.0),
                    ("outcome", 1.0),
                    ("reaction", 0.6),
                    ("engage", 0.2),
                    ("model", 0.3),
                ]
                .map(|(k, w)| (k.to_string(), w)),
            ),
        }
    }

    /// `pd-verdict/2`: v1 + 트랙 `nav`(web 과 같은 값) + 관측 종류 `implicit`(0.3)
    pub fn v2() -> Params {
        let mut p = Params::v1();
        let web = p.tracks["web"];
        p.tracks.insert(NAV.to_string(), web);
        p.weights.insert(IMPLICIT.to_string(), 0.3);
        p
    }

    /// recipe 이름의 매개변수. 모르는 이름은 None
    pub fn for_recipe(recipe: &str) -> Option<Params> {
        match recipe {
            RECIPE_V1 => Some(Params::v1()),
            RECIPE_V2 => Some(Params::v2()),
            _ => None,
        }
    }

    pub fn track(&self, name: &str) -> Option<&TrackParams> {
        self.tracks.get(name)
    }

    /// 관측 종류의 기본 가중치 (모르는 종류는 0)
    pub fn weight(&self, kind: &str) -> f64 {
        self.weights.get(kind).copied().unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prev {
    pub tier: Tier,
    pub since_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Duel {
    pub wins: u32,
    pub losses: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub evidence: f64,
    pub value: f64,
    pub level: Option<Level>,
    pub rank: Option<f64>,
    pub tier: Tier,
    pub grade: Option<i32>,
    pub since_ms: i64,
    pub reasons: Vec<String>,
}

/// 판정이 쓰는 초월 함수 두 개: 감쇠의 `2^x` 와 순위의 `tanh`.
///
/// 기본([`Math::STD`])은 Rust 표준(플랫폼의 libm)이다. 서버 cron(workers/cron, wasm32)은 JS 엔진의 `Math.pow` · `Math.tanh` 를
/// 넘긴다: wasm32 의 libm 은 V8(fdlibm)과 마지막 자리가 다를 때가 있어(2^x 는 약 4.5 %, tanh 는 약 10 % 의 입력), 그대로면
/// 옛 TypeScript 판과 D1 에 쓰는 E · v · r 이 비트까지 같지 않다. 계약(`tolerance`)으로는 둘 다 맞다.
#[derive(Debug, Clone, Copy)]
pub struct Math {
    /// `pow(2, x)` (x ≤ 0)
    pub pow: fn(f64, f64) -> f64,
    pub tanh: fn(f64) -> f64,
}

impl Math {
    pub const STD: Math = Math {
        pow: f64::powf,
        tanh: f64::tanh,
    };
}

/// `now_ms` 시점 관측들의 근거량 `E` 와 값 `v`
pub fn evidence(obs: &[Obs], now_ms: i64, track: &TrackParams, params: &Params) -> (f64, f64) {
    evidence_in(&Math::STD, obs, now_ms, track, params)
}

/// [`evidence`] 를 주어진 초월 함수로
pub fn evidence_in(math: &Math, obs: &[Obs], now_ms: i64, track: &TrackParams, params: &Params) -> (f64, f64) {
    let (mut e, mut s) = (0.0, 0.0);
    for o in obs {
        if o.sign == 0 {
            continue;
        }
        let age = (now_ms - o.at_ms).max(0) as f64;
        let u = o.weight * o.credit * (math.pow)(2.0, -age / track.half_life_ms);
        e += u;
        s += u * o.sign as f64;
    }
    (e, s / (e + params.k))
}

/// [`judge`] 가 `E` · `v` 말고 더 보는 것
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Context<'a> {
    pub track: &'a TrackParams,
    pub baselines: &'a Baselines,
    pub prev: Prev,
    pub now_ms: i64,
    pub duel: Option<Duel>,
    pub superseded: bool,
}

/// `E` 와 `v` 를 아는 항목의 등급 (피어를 다시 판정할 때 관측을 다시 읽지 않는다)
pub fn judge(evidence: f64, value: f64, cx: &Context<'_>, params: &Params) -> Verdict {
    judge_in(&Math::STD, evidence, value, cx, params)
}

/// [`judge`] 를 주어진 초월 함수로
pub fn judge_in(math: &Math, evidence: f64, value: f64, cx: &Context<'_>, params: &Params) -> Verdict {
    let (e, v) = (evidence, value);
    let base = Level::ALL
        .into_iter()
        .find_map(|lv| cx.baselines.get(lv).filter(|b| b.n >= cx.track.min_obs.get(lv)).map(|b| (lv, b)));
    let level = base.map(|(lv, _)| lv);
    let r = base.map(|(_, b)| (math.tanh)((v - b.mean) / (b.sd + params.eps)));
    let (win, loss) = match cx.duel {
        Some(d) if d.wins + d.losses >= params.duel_min => {
            let meetings = (d.wins + d.losses) as f64;
            (Some(d.wins as f64 / meetings), Some(d.losses as f64 / meetings))
        }
        _ => (None, None),
    };
    let prev = cx.prev.tier;

    let mut reasons = Vec::new();
    let tier = if cx.superseded {
        reasons.push("superseded".to_string());
        Tier::Discount
    } else if e < params.min_evidence {
        reasons.push("low_evidence".to_string());
        Tier::Unrated
    } else {
        let rank_high = r.is_some_and(|r| r >= params.enter);
        let rank_low = r.is_some_and(|r| r <= -params.enter);
        let duel_win = win.is_some_and(|w| w >= params.duel_rate);
        let duel_loss = loss.is_some_and(|l| l >= params.duel_rate);
        if v >= params.floor && (rank_high || duel_win) {
            reasons.push(if rank_high { "rank_high" } else { "duel_win" }.to_string());
            Tier::Premium
        } else if v <= -params.floor && (rank_low || duel_loss) {
            reasons.push(if rank_low { "rank_low" } else { "duel_loss" }.to_string());
            Tier::Discount
        } else if prev == Tier::Premium && v > 0.0 && (r.is_some_and(|r| r >= params.exit) || (r.is_none() && duel_win)) {
            reasons.push("held".to_string());
            Tier::Premium
        } else if prev == Tier::Discount && v < 0.0 && (r.is_some_and(|r| r <= -params.exit) || (r.is_none() && duel_loss)) {
            reasons.push("held".to_string());
            Tier::Discount
        } else {
            if rank_high || rank_low || duel_win || duel_loss {
                reasons.push("floor".to_string());
            }
            Tier::Neutral
        }
    };
    reasons.push(match level {
        Some(lv) => format!("level:{}", lv.name()),
        None => "no_baseline".to_string(),
    });

    let grade = match tier {
        Tier::Premium => Some(if r.is_some_and(|r| r >= params.strong) { 2 } else { 1 }),
        Tier::Discount => Some(if cx.superseded || r.is_some_and(|r| r <= -params.strong) {
            -2
        } else {
            -1
        }),
        Tier::Neutral => Some(0),
        Tier::Unrated => None,
    };
    let since_ms = if tier == prev { cx.prev.since_ms } else { cx.now_ms };
    Verdict {
        evidence: e,
        value: v,
        level,
        rank: r,
        tier,
        grade,
        since_ms,
        reasons,
    }
}

/// 계약 사례 하나의 입력
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub now_ms: i64,
    pub track: String,
    pub prev: Prev,
    pub obs: Vec<Obs>,
    #[serde(default)]
    pub baseline: Baselines,
    #[serde(default)]
    pub duel: Option<Duel>,
    #[serde(default)]
    pub superseded: bool,
}

/// 관측에서 판정까지. 모르는 트랙이면 None
pub fn verdict(case: &Case, params: &Params) -> Option<Verdict> {
    let track = params.track(&case.track)?;
    let (e, v) = evidence(&case.obs, case.now_ms, track, params);
    let cx = Context {
        track,
        baselines: &case.baseline,
        prev: case.prev,
        now_ms: case.now_ms,
        duel: case.duel,
        superseded: case.superseded,
    };
    Some(judge(e, v, &cx, params))
}

/// 한 스코프의 판정된 값들로 만든 기준선 (모집단 표준편차)
pub fn baseline_of(values: &[f64]) -> Baseline {
    let n = values.len();
    if n == 0 {
        return Baseline { n: 0, mean: 0.0, sd: 0.0 };
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let variance = values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    Baseline {
        n: n as u64,
        mean,
        sd: variance.sqrt(),
    }
}

/// 관측을 만드는 쪽(-nav · -analytic · 로컬 코칭)이 [`Obs::credit`] 에 넣는 표준 몫.
///
/// 판정 수식과 [`Params::v1`] · [`Params::v2`] 는 그대로다. 가감점은 이 몫과 철회(`sign` 0)로만 조절하므로 recipe ·
/// 계약이 바뀌지 않는다. 1 보다 큰 값은 검증된 근거를 관측 한 번보다 무겁게 세는 배율이다.
pub mod credits {
    /// 마찰 폭발 · 재시도 · 접힌 취소 루프 없이 완주했지만 지름길 기준(z ≤ −band)에는 못 미친 무마찰 완주의 바닥값
    pub const CLEAN_PASS_FLOOR: f64 = 0.65;
    /// 고스트 안내 없이 스스로 찾아 완주한 지름길
    pub const SELF_DISCOVERED: f64 = 1.00;
    /// 고스트(러닝메이트) 배지를 그대로 따라 완주한 런 (확증 편향을 줄인다)
    pub const GHOST_FOLLOWED: f64 = 0.35;
    /// 표준 명령(AST · 명령줄)과 1:1 로 묶여 음성 이름 · 단일 키 호출이 검증된 접근성 체크포인트
    pub const ACCESSIBLE_VERIFIED: f64 = 1.20;
    /// 조건부로 되살린(FadedRecall) 옛 코스가 처음 마찰 없이 통과한 런: 빨리 회복시킨다
    pub const PROVISIONAL_RECALL_PASS: f64 = 1.50;

    /// 무마찰 완주의 몫: 스스로 찾았으면 [`CLEAN_PASS_FLOOR`], 고스트를 따랐으면 [`GHOST_FOLLOWED`]
    pub fn clean_pass(ghost_followed: bool) -> f64 {
        if ghost_followed {
            GHOST_FOLLOWED
        } else {
            CLEAN_PASS_FLOOR
        }
    }

    /// 지름길 완주의 몫: 스스로 찾았으면 [`SELF_DISCOVERED`], 고스트를 따랐으면 [`GHOST_FOLLOWED`]
    pub fn shortcut(ghost_followed: bool) -> f64 {
        if ghost_followed {
            GHOST_FOLLOWED
        } else {
            SELF_DISCOVERED
        }
    }
}

/// [`effective_half_life_ms`] 의 조절 폭
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdaptiveHalfLifeCfg {
    /// 패치 드리프트로 보는 `|z|` (기본 2.0)
    pub drift_z_trigger: f64,
    /// 드리프트 기울기 γ: `H / (1 + γ(|z| − trigger))` (기본 1.0)
    #[serde(default = "unit_gamma")]
    pub drift_gamma: f64,
    /// 드리프트 때 줄일 수 있는 가장 짧은 반감기 (기본 2일)
    pub min_drift_half_life_ms: f64,
    /// 스코프 전체가 쉬는 동안(월간 · 분기 업무) 늘일 수 있는 가장 긴 반감기 (기본 45일)
    pub max_dormant_half_life_ms: f64,
}

fn unit_gamma() -> f64 {
    1.0
}

impl Default for AdaptiveHalfLifeCfg {
    fn default() -> Self {
        AdaptiveHalfLifeCfg {
            drift_z_trigger: 2.0,
            drift_gamma: 1.0,
            min_drift_half_life_ms: 2.0 * DAY_MS,
            max_dormant_half_life_ms: 45.0 * DAY_MS,
        }
    }
}

/// 트래픽 · 드리프트에 맞춘 유효 반감기 `H_eff`.
///
/// 계약 경로(`verdict_v1` · `v2`)는 트랙의 `half_life_ms` 를 그대로 쓰고, 서버 nav_verdicts 스윕과 로컬 코칭이 골라 부른다.
/// - 드리프트 `|z| ≥ drift_z_trigger`(어느 쪽으로 꺾여도): `H / (1 + γ(|z| − trigger))`, 단 `min_drift_half_life_ms` 아래로는
///   줄이지 않는다(기본 반감기가 그보다 짧으면 기본값). 표가 아래로 꺾인 옛 premium 을 빨리 내린다.
/// - 그 밖: 스코프 활성 비율 `a`(0 = 완전 휴지 ~ 1 = 상시 활성)만큼 `H + (H_max − H)(1 − a)` 로 늘인다. 월간 업무의
///   휴지기에 코스가 잊히지 않게 한다. 기본 반감기가 이미 `H_max` 이상이면(예: 90일 `pd` 트랙) 늘이지도 줄이지도 않는다.
/// - 유한하지 않은 `z` 는 드리프트 없음, 유한하지 않은 `a` 는 상시 활성(조절 없음)으로 본다.
pub fn effective_half_life_ms(
    base_half_life_ms: f64,
    drift_z: Option<f64>,
    scope_activity_ratio: f64,
    cfg: &AdaptiveHalfLifeCfg,
) -> f64 {
    if !base_half_life_ms.is_finite() || base_half_life_ms <= 0.0 {
        return base_half_life_ms;
    }
    if let Some(dz) = drift_z.map(f64::abs).filter(|z| z.is_finite() && *z >= cfg.drift_z_trigger) {
        // 나눗셈 그대로 (서버 스윕의 예전 식과 비트까지 같다)
        let divisor = 1.0 + cfg.drift_gamma.max(0.0) * (dz - cfg.drift_z_trigger);
        return (base_half_life_ms / divisor).max(cfg.min_drift_half_life_ms.min(base_half_life_ms));
    }
    let act = if scope_activity_ratio.is_finite() {
        scope_activity_ratio.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let ceiling = cfg.max_dormant_half_life_ms.max(base_half_life_ms);
    base_half_life_ms + (ceiling - base_half_life_ms) * (1.0 - act)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // 계약은 이 저장소의 contracts/ 에 있다 (crates/premium-discount-verdict/src 에서 세 단계 위)
    const CONTRACT_V1: &str = include_str!("../../../contracts/verdict_v1.json");
    const CONTRACT_V2: &str = include_str!("../../../contracts/verdict_v2.json");

    #[derive(Deserialize)]
    struct Contract {
        recipe: String,
        tolerance: f64,
        params: Params,
        cases: Vec<Named>,
    }

    #[derive(Deserialize)]
    struct Named {
        name: String,
        #[serde(flatten)]
        case: Case,
        expect: Verdict,
    }

    fn check(text: &str, recipe: &str, params: Params, min_cases: usize) -> Contract {
        let c: Contract = serde_json::from_str(text).unwrap();
        assert_eq!(c.recipe, recipe);
        assert_eq!(c.params, params, "{recipe} 매개변수");
        assert_eq!(Params::for_recipe(recipe).as_ref(), Some(&params));
        assert!(c.cases.len() >= min_cases, "{recipe} 사례 수 {}", c.cases.len());
        let close = |a: Option<f64>, b: Option<f64>, what: &str| match (a, b) {
            (Some(a), Some(b)) => assert!((a - b).abs() <= c.tolerance, "{what}: {a} vs {b}"),
            _ => assert_eq!(a, b, "{what}"),
        };
        for n in &c.cases {
            let got = verdict(&n.case, &params).unwrap();
            let want = &n.expect;
            close(Some(got.evidence), Some(want.evidence), &format!("{recipe} {} evidence", n.name));
            close(Some(got.value), Some(want.value), &format!("{recipe} {} value", n.name));
            close(got.rank, want.rank, &format!("{recipe} {} rank", n.name));
            assert_eq!(got.level, want.level, "{recipe} {} level", n.name);
            assert_eq!(got.tier, want.tier, "{recipe} {} tier", n.name);
            assert_eq!(got.grade, want.grade, "{recipe} {} grade", n.name);
            assert_eq!(got.since_ms, want.since_ms, "{recipe} {} since", n.name);
            assert_eq!(got.reasons, want.reasons, "{recipe} {} reasons", n.name);
        }
        c
    }

    #[test]
    fn v1_params_and_every_case_match_the_contract() {
        check(CONTRACT_V1, RECIPE_V1, Params::v1(), 19);
    }

    #[test]
    fn v2_params_and_every_case_match_the_contract() {
        let v2 = check(CONTRACT_V2, RECIPE_V2, Params::v2(), 25);
        // v2 는 v1 사례를 모두 담고, 그 결과는 v1 과 같다
        let v1 = check(CONTRACT_V1, RECIPE_V1, Params::v1(), 19);
        for old in &v1.cases {
            let same = v2.cases.iter().find(|n| n.name == old.name).unwrap_or_else(|| panic!("v2 에 {} 가 없다", old.name));
            assert_eq!(same.case, old.case, "{} 입력", old.name);
            assert_eq!(same.expect, old.expect, "{} 기대값", old.name);
        }
        assert!(v2.cases.iter().any(|n| n.case.track == NAV && n.case.obs.iter().any(|o| o.kind == IMPLICIT)));
    }

    #[test]
    fn v2_only_adds_the_nav_track_and_the_implicit_weight() {
        let (v1, v2) = (Params::v1(), Params::v2());
        assert_eq!(v2.track(NAV), v1.track("web"));
        assert_eq!(v2.weight(IMPLICIT), 0.3);
        assert_eq!(v1.weight(IMPLICIT), 0.0);
        let mut back = v2.clone();
        back.tracks.remove(NAV);
        back.weights.remove(IMPLICIT);
        assert_eq!(back, v1);
        assert_eq!(Params::for_recipe("pd-verdict/9"), None);
    }

    #[test]
    fn verdicts_serialize_with_the_contract_field_names() {
        let c: Value = serde_json::from_str(CONTRACT_V2).unwrap();
        let first: Case = serde_json::from_value(c["cases"][0].clone()).unwrap();
        let v = serde_json::to_value(verdict(&first, &Params::v2()).unwrap()).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let want: Vec<&str> = c["cases"][0]["expect"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), want.len());
        for k in want {
            assert!(keys.contains(&k), "{k}");
        }
        assert_eq!(v["tier"], "premium");
        assert_eq!(v["level"], "secondary");
    }

    #[test]
    fn the_math_hook_is_what_evidence_and_judge_use() {
        let params = Params::v1();
        let track = params.track("pd").unwrap();
        let day = 86_400_000;
        let obs: Vec<Obs> = (0..5)
            .map(|i| Obs {
                kind: "vote".into(),
                sign: if i % 2 == 0 { 1 } else { -1 },
                weight: 1.0,
                credit: 1.0,
                at_ms: -i * 7 * day,
            })
            .collect();
        assert_eq!(evidence_in(&Math::STD, &obs, 0, track, &params), evidence(&obs, 0, track, &params));
        // 2^x 를 1 로 두면 감쇠가 없다: E = 5, v = 1 / 7
        let flat = Math { pow: |_, _| 1.0, tanh: f64::tanh };
        assert_eq!(evidence_in(&flat, &obs, 0, track, &params), (5.0, 1.0 / 7.0));
        let base = Baselines {
            secondary: Some(Baseline { n: 9, mean: 0.0, sd: 1.0 }),
            ..Baselines::default()
        };
        let cx = Context {
            track,
            baselines: &base,
            prev: Prev { tier: Tier::Unrated, since_ms: 0 },
            now_ms: 0,
            duel: None,
            superseded: false,
        };
        assert_eq!(judge_in(&Math::STD, 3.0, 0.2, &cx, &params), judge(3.0, 0.2, &cx, &params));
        let steep = Math { pow: f64::powf, tanh: |_| 0.9 };
        let v = judge_in(&steep, 3.0, 0.2, &cx, &params);
        assert_eq!((v.tier, v.grade, v.rank), (Tier::Premium, Some(2), Some(0.9)));
    }

    #[test]
    fn unknown_tracks_and_weights() {
        let params = Params::v2();
        let case = Case {
            now_ms: 0,
            track: "nope".into(),
            prev: Prev {
                tier: Tier::Unrated,
                since_ms: 0,
            },
            obs: vec![],
            baseline: Baselines::default(),
            duel: None,
            superseded: false,
        };
        assert!(verdict(&case, &params).is_none());
        assert_eq!(params.weight("manual"), 2.0);
        assert_eq!(params.weight("unknown"), 0.0);
        let b = baseline_of(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!((b.n, b.mean, b.sd), (4, 0.0, 1.0));
        assert_eq!(baseline_of(&[]).n, 0);
    }

    #[test]
    fn credits_split_self_found_and_ghost_followed_runs() {
        use credits::*;
        assert_eq!(clean_pass(false), CLEAN_PASS_FLOOR);
        assert_eq!(clean_pass(true), GHOST_FOLLOWED);
        assert_eq!(shortcut(false), SELF_DISCOVERED);
        assert_eq!(shortcut(true), GHOST_FOLLOWED);
        // 몫은 판정 수식을 바꾸지 않고 근거량만 바꾼다: 같은 관측 둘의 E 는 몫의 합
        let params = Params::v2();
        let track = params.track(NAV).unwrap();
        let obs = |credit| Obs {
            kind: IMPLICIT.into(),
            sign: 1,
            weight: params.weight(IMPLICIT),
            credit,
            at_ms: 0,
        };
        let (e, _) = evidence(&[obs(CLEAN_PASS_FLOOR), obs(GHOST_FOLLOWED)], 0, track, &params);
        assert!((e - 0.3 * (0.65 + 0.35)).abs() < 1e-12);
    }

    #[test]
    fn effective_half_life_shrinks_on_drift_and_stretches_while_dormant() {
        let cfg = AdaptiveHalfLifeCfg::default();
        let day = DAY_MS;
        let h = 14.0 * day;
        // 상시 활성 · 드리프트 없음: 그대로
        assert_eq!(effective_half_life_ms(h, None, 1.0, &cfg), h);
        // 완전 휴지: 45일, 절반 활성: 그 사이
        assert_eq!(effective_half_life_ms(h, None, 0.0, &cfg), 45.0 * day);
        assert_eq!(effective_half_life_ms(h, None, 0.5, &cfg), 29.5 * day);
        // 드리프트: 기준 z 에서는 그대로, z = 3 이면 절반, 아주 크면 2일에서 멈춤
        assert_eq!(effective_half_life_ms(h, Some(2.0), 0.0, &cfg), h);
        assert_eq!(effective_half_life_ms(h, Some(3.0), 1.0, &cfg), 7.0 * day);
        assert_eq!(effective_half_life_ms(h, Some(50.0), 1.0, &cfg), 2.0 * day);
        // 기준 z 아래 드리프트는 휴지 규칙을 따른다
        assert_eq!(effective_half_life_ms(h, Some(1.9), 0.0, &cfg), 45.0 * day);
        // 아래로 꺾인 드리프트도 줄인다 (낡은 premium 의 강등)
        assert_eq!(effective_half_life_ms(h, Some(-3.0), 1.0, &cfg), 7.0 * day);
        // 기울기: 서버 nav 스윕의 매개변수(기준 1.5, γ 1.2)는 H / (1 + 1.2 · (|z| − 1.5))
        let nav = AdaptiveHalfLifeCfg { drift_z_trigger: 1.5, drift_gamma: 1.2, ..cfg };
        assert!((effective_half_life_ms(h, Some(-2.5), 1.0, &nav) - h / 2.2).abs() < 1e-6);
        let old: AdaptiveHalfLifeCfg = serde_json::from_str(r#"{"drift_z_trigger":2.0,"min_drift_half_life_ms":1.0,"max_dormant_half_life_ms":2.0}"#).unwrap();
        assert_eq!(old.drift_gamma, 1.0, "configs written before the slope read with γ = 1");
        // 90일 트랙은 휴지기에 줄지 않는다 (45일 상한은 늘이는 폭일 뿐)
        let pd = 90.0 * day;
        assert_eq!(effective_half_life_ms(pd, None, 0.0, &cfg), pd);
        assert!((effective_half_life_ms(pd, Some(4.0), 1.0, &cfg) - 30.0 * day).abs() < 1e-3);
        // 2일보다 짧은 기본 반감기는 드리프트로 늘어나지 않는다
        assert_eq!(effective_half_life_ms(day, Some(9.0), 1.0, &cfg), day);
        // 유한하지 않은 입력
        assert_eq!(effective_half_life_ms(h, Some(f64::NAN), f64::NAN, &cfg), h);
        assert_eq!(effective_half_life_ms(h, None, -3.0, &cfg), 45.0 * day);
        // 계약 트랙의 반감기는 그대로다 (계약 경로는 이 함수를 부르지 않는다)
        assert_eq!(Params::v2().track(NAV).unwrap().half_life_ms, h);
    }
}
