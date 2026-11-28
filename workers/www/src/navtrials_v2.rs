//! Phase 4 · Phase 5 · Phase 7 서버 타임트라이얼 2.0 순수 로직 및 헬퍼 (`workers/www/src/navtrials.rs` 연동).
//!
//! - Phase 4 Part 1: 품질·속도·모달리티 인지형 `route_marks_v2`, 1인 1목적지 투표권(`ballot_id`), 목적지 조건부 `route_fit_hash`
//! - Phase 4 Part 2: 서버 갈림길 대결 추출(`extract_server_shortcut_duels`), `duel_reg_id`, 3인 선행 승격(`should_pioneer_promote`)
//! - Phase 4 Part 3: 생존자 편향 해소용 단일 체크포인트 `dead_end_marks` 파싱·중복키
//! - Phase 5 Part 1: 14일 반감기 에폭 하트비트(`epoch_pulse_id`), EMA 스플릿(`update_ema_splits`)
//! - Phase 5 Part 2 + Phase 7 Part 4: 4-쿼터 다양성 포트폴리오 조회(`select_stratified_portfolio`), 대표 기록 차분 치환(`representative_best_swap`), `batch_id` 검증, 코스 물리 특성 비트(`compute_course_traits`)
//! - Phase 5 Part 3: 합의 대표 벡터(`evaluate_vector_consensus`), Int8 Base64 압축, 지갑→세션 무중복 승격(`CLAIM_WALLET_PATH`, `plan_wallet_claim_registrations`)

use std::collections::{HashMap, HashSet};

use premium_discount_worker_common::jsv::{num_to_string, strict_eq};
use premium_discount_worker_common::util::hash_str;
use premium_discount_worker_common::{Throw, R, V};

use crate::navtrials::{
    checkpoint_id_v, is_hash, ParsedRun, Step, NAV_FIRST_DAY, NAV_RECIPE,
};

pub const CLAIM_WALLET_PATH: &str = "/nav/trials/claim-wallet";
pub const HASH_NGRAM_SPACE: &str = "hash-ngram-v1";

/// Phase 4 Part 1: 속도 우위 허용 대역(+5%, 최소 500ms 데드밴드).
pub const PACE_EPS: f64 = 0.05;
pub const PACE_DEADBAND_MS: f64 = 500.0;

/// Phase 4 Part 2: 3인 독립 합의 선행 승격 기준.
pub const PIONEER_MIN_WINS: u32 = 3;
pub const PIONEER_MIN_SAVED_MS: f64 = 1_500.0;
pub const PIONEER_MIN_MARGIN: f64 = 0.15;
pub const SERVER_DUEL_MIN_SAVED_MS: f64 = 500.0;
pub const SERVER_DUEL_MIN_MARGIN: f64 = 0.08;

/// Phase 5 Part 1: 14일 반감기 에폭 단위.
pub const EPOCH_DAYS: i64 = 14;

/// Phase 5 Part 2 + Phase 7 Part 4: 4-쿼터 다양성 포트폴리오 슬롯 상한 (premium-discount-nav 의 값)
pub use premium_discount_nav::{PER_GOAL_CAP, QUOTA_ACCESSIBLE, QUOTA_ELITE, QUOTA_ENTRY, QUOTA_POPULAR};

/// Phase 7 Part 4: 코스 물리 조작 요구 특성 비트.
pub const TRAIT_NO_FINE_GESTURE: u32 = 1 << 0;
pub const TRAIT_TREE_ACCESSIBLE: u32 = 1 << 1;
pub const TRAIT_LOW_DENSITY_CALM: u32 = 1 << 2;

/// Phase 5 Part 3: 합의 대표 벡터 임계값.
pub const CONSENSUS_MATCH_COS: f32 = 0.88;
pub const CONSENSUS_CONFLICT_COS: f32 = 0.65;
pub const CONSENSUS_LOCK_CONFIRMATIONS: u32 = 3;
pub const CONSENSUS_REPLACE_HITS: u32 = 2;

// ───────────── Phase 4 Part 1: route_marks 2.0 & Ballot Deduplication ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerMarkOutcome {
    Premium,
    Discount,
    EquivalentPass,
    Abstain,
}

impl ServerMarkOutcome {
    pub fn sign(self) -> Option<i32> {
        match self {
            ServerMarkOutcome::Premium => Some(1),
            ServerMarkOutcome::Discount => Some(-1),
            ServerMarkOutcome::EquivalentPass | ServerMarkOutcome::Abstain => None,
        }
    }
}

fn step_key_v(s: &V) -> R<String> {
    let gesture = s.g("gesture")?;
    let (kind, hash) = (s.get("kind").to_str(), s.get("hash").to_str());
    Ok(if gesture.truthy() {
        format!("{kind}:{hash}@{}", gesture.to_str())
    } else {
        format!("{kind}:{hash}")
    })
}

fn step_base_key_v(s: &V) -> R<String> {
    let (kind, hash) = (s.g("kind")?.to_str(), s.g("hash")?.to_str());
    Ok(format!("{kind}:{hash}"))
}

/// 오염 방지 `route_marks_v2`: 품질·속도·모달리티 등가성·스텝별 병목 마스크를 반영한 4-상태 교차 판정.
pub fn route_marks_v2(
    run: &[V],
    ghost: &V,
    run_total_ms: f64,
    run_kind: i32,
    ghost_avg_total_ms: f64,
    fault_steps_mask: u16,
) -> R<Option<Vec<(V, ServerMarkOutcome)>>> {
    let ghost_len = ghost.g("length")?;
    if run.is_empty() || strict_eq(&ghost_len, &V::Num(0.0)) {
        return Ok(None);
    }
    let ghost_first = ghost.get("0");
    let ghost_last = ghost.get(&num_to_string(ghost_len.to_num() - 1.0));
    if step_key_v(&run[0])? != step_key_v(&ghost_first)? || step_key_v(&run[run.len() - 1])? != step_key_v(&ghost_last)? {
        return Ok(None);
    }
    let middle: Vec<V> = match ghost {
        V::Arr(_) => {
            let items = ghost.items();
            if items.len() > 2 {
                items[1..items.len() - 1].to_vec()
            } else {
                Vec::new()
            }
        }
        _ => return Err(Throw::type_error("ghost.slice is not a function")),
    };

    let pace_ceiling = (ghost_avg_total_ms * (1.0 + PACE_EPS)).max(ghost_avg_total_ms + PACE_DEADBAND_MS);
    let fast_enough = ghost_avg_total_ms <= 0.0 || run_total_ms <= pace_ceiling;
    let clean_for_bypass = run_kind != -1 && fast_enough;

    let mut run_keys = Vec::with_capacity(run.len());
    let mut run_bases = Vec::with_capacity(run.len());
    for s in run {
        run_keys.push(step_key_v(s)?);
        run_bases.push(step_base_key_v(s)?);
    }

    let mut out: Vec<(V, ServerMarkOutcome)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for step in middle {
        let gk = step_key_v(&step)?;
        if !seen.insert(gk.clone()) {
            continue;
        }
        let gbase = step_base_key_v(&step)?;
        if let Some(ri) = run_keys.iter().position(|k| k == &gk) {
            let faulted = ri < 16 && (fault_steps_mask & (1u16 << ri)) != 0;
            let run_blocked = run_kind == -1 && fault_steps_mask == 0;
            let outcome = if faulted || run_blocked {
                ServerMarkOutcome::Abstain
            } else {
                ServerMarkOutcome::Premium
            };
            out.push((step, outcome));
        } else if run_bases.iter().any(|b| b == &gbase) {
            out.push((step, ServerMarkOutcome::EquivalentPass));
        } else {
            let outcome = if clean_for_bypass {
                ServerMarkOutcome::Discount
            } else {
                ServerMarkOutcome::Abstain
            };
            out.push((step, outcome));
        }
    }
    Ok(Some(out))
}

/// Phase 4 Part 1: `(account, app, goal_cp, target_cp)` 1인 1표 투표권 기본키.
pub fn ballot_id(account_key: &str, app: &str, goal_cp: &str, target_cp: &str) -> String {
    hash_str(&format!("nav-ballot/2|{account_key}{app}{goal_cp}{target_cp}"))
}

/// Phase 4 Part 1: 목적지 조건부 경로 적합도 키 (`route_fit_hash = hashId(CP(g_k) + CP(goal))`).
pub fn route_fit_hash(target_cp: &str, goal_cp: &str) -> String {
    hash_str(&format!("{target_cp}{goal_cp}"))
}

/// 투표장부 변경 결과: 최초 투표 · 동일 부호 중복 차단 · 의견 번복(이전 부호 상쇄 후 새 부호 가산).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BallotDelta {
    First(i32),
    Duplicate,
    Flipped { old: i32, new: i32 },
}

pub fn apply_ballot_delta(prev_sign: Option<i32>, new_sign: i32) -> BallotDelta {
    match prev_sign {
        None => BallotDelta::First(new_sign),
        Some(old) if old == new_sign => BallotDelta::Duplicate,
        Some(old) => BallotDelta::Flipped { old, new: new_sign },
    }
}

// ───────────── Phase 4 Part 2: Server Shortcut Duels & Pioneer Promotion ─────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedDuel {
    pub fork_course: String,
    pub skipped_cp: String,
    pub taken_cp: String,
    pub saved_ms: f64,
    pub margin: f64,
}

pub fn duel_reg_id(account_key: &str, fork_course: &str, taken_cp: &str, skipped_cp: &str) -> String {
    hash_str(&format!("nav-duel/1|{account_key}{fork_course}{taken_cp}{skipped_cp}"))
}

/// LCS 창 내부 구간 시간 대조로 서버 측 `AltBranch` 지름길 대결 쌍을 추출한다.
pub fn extract_server_shortcut_duels(run: &ParsedRun, ghost_steps: &[V], ghost_avg: &[f64]) -> R<Vec<ExtractedDuel>> {
    if run.kind == -1 || run.steps.len() < 2 || ghost_steps.len() < 2 || ghost_avg.len() != ghost_steps.len() {
        return Ok(Vec::new());
    }
    let mut g_ids = Vec::with_capacity(ghost_steps.len());
    let mut g_courses = Vec::with_capacity(ghost_steps.len());
    for s in ghost_steps {
        let id = s.g("id")?;
        let cid = if id.is_nullish() { checkpoint_id_v(s)? } else { id.to_str() };
        g_ids.push(cid);
        let c = s.g("course")?;
        g_courses.push(if c.is_nullish() { String::new() } else { c.to_str() });
    }
    let r_ids: Vec<&str> = run.steps.iter().map(|s| s.id.as_str()).collect();
    if r_ids.first().copied() != g_ids.first().map(String::as_str) || r_ids.last().copied() != g_ids.last().map(String::as_str) {
        return Ok(Vec::new());
    }
    // LCS on inner indices.
    let (ia, ib) = (&r_ids[1..r_ids.len() - 1], &g_ids[1..g_ids.len() - 1]);
    let mut dp = vec![vec![0usize; ib.len() + 1]; ia.len() + 1];
    for i in (0..ia.len()).rev() {
        for j in (0..ib.len()).rev() {
            dp[i][j] = if ia[i] == ib[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut anchors = vec![(0usize, 0usize)];
    let (mut i, mut j) = (0usize, 0usize);
    while i < ia.len() && j < ib.len() {
        if ia[i] == ib[j] {
            anchors.push((i + 1, j + 1));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    anchors.push((r_ids.len() - 1, g_ids.len() - 1));

    let mut out = Vec::new();
    for w in anchors.windows(2) {
        let ((i0, j0), (i1, j1)) = (w[0], w[1]);
        // AltBranch: 고스트 스텝을 1개 이상 건너뛰고(j1 > j0 + 1), 런에도 독자 대안 스텝이 존재(i1 > i0 + 1).
        if j1 <= j0 + 1 || i1 <= i0 + 1 {
            continue;
        }
        let skipped_cp = &g_ids[j0 + 1];
        if r_ids.contains(&skipped_cp.as_str()) {
            continue;
        }
        let run_dt = (run.splits[i1] - run.splits[i0]).max(0.0);
        let ghost_dt = (ghost_avg[j1] - ghost_avg[j0]).max(0.0);
        if ghost_dt <= 0.0 || run_dt >= ghost_dt {
            continue;
        }
        let saved_ms = ghost_dt - run_dt;
        let margin = saved_ms / ghost_dt;
        if saved_ms < SERVER_DUEL_MIN_SAVED_MS || margin < SERVER_DUEL_MIN_MARGIN {
            continue;
        }
        let fork_course = if !g_courses[j0].is_empty() {
            g_courses[j0].clone()
        } else {
            run.steps[i0].course.clone()
        };
        out.push(ExtractedDuel {
            fork_course,
            skipped_cp: skipped_cp.clone(),
            taken_cp: run.steps[i0 + 1].id.clone(),
            saved_ms,
            margin,
        });
    }
    Ok(out)
}

/// 독립 3인 이상 합의 + 평균 margin >= 0.15 + 평균 saved_ms >= 1500ms + 자체 마찰 discount < 1/3 이면 선행 승격.
pub fn should_pioneer_promote(wins: u32, saved_ms_sum: f64, margin_sum: f64, taken_premium: u32, taken_discount: u32) -> bool {
    if wins < PIONEER_MIN_WINS {
        return false;
    }
    let mean_saved = saved_ms_sum / f64::from(wins);
    let mean_margin = margin_sum / f64::from(wins);
    let decided = taken_premium + taken_discount;
    let discount_ok = decided == 0 || (taken_discount * 3 < decided);
    mean_saved >= PIONEER_MIN_SAVED_MS && mean_margin >= PIONEER_MIN_MARGIN && discount_ok
}

// ───────────── Phase 4 Part 3: Dead-End Trap Pulse ─────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDeadEnd {
    pub app: String,
    pub checkpoint_id: String,
    pub day: f64,
}

pub fn dead_end_ballot_id(account_key: &str, app: &str, checkpoint_id: &str) -> String {
    hash_str(&format!("nav-deadend/1|{account_key}{app}{checkpoint_id}"))
}

pub fn parse_dead_end_marks(val: &V, today: f64) -> Result<Vec<ParsedDeadEnd>, String> {
    if val.is_undef() || val.is_nullish() {
        return Ok(Vec::new());
    }
    if !val.is_arr() || val.len() > 10 {
        return Err("dead_end_marks must hold up to 10 items".to_string());
    }
    let mut out = Vec::new();
    for item in val.items() {
        let app = item.get("app").to_str().trim().to_ascii_lowercase();
        let cp = item.get("checkpoint_id").to_str().trim().to_ascii_lowercase();
        if !is_hash(&app) || !is_hash(&cp) {
            return Err("dead_end_marks app and checkpoint_id must be 0x + 40 hex".to_string());
        }
        let recipe = item.get("recipe");
        if !recipe.is_undef() && recipe.as_str() != Some(NAV_RECIPE) {
            return Err(format!("dead_end_marks recipe must be {NAV_RECIPE}"));
        }
        let day = match item.get("day") {
            V::Num(d) if d.is_finite() && d.trunc() == d && d >= NAV_FIRST_DAY && d <= today + 1.0 => d,
            V::Undef | V::Null => today,
            _ => return Err("dead_end_marks day invalid".to_string()),
        };
        out.push(ParsedDeadEnd { app, checkpoint_id: cp, day });
    }
    Ok(out)
}

// ───────────── Phase 5 Part 1: Half-Life Epoch Heartbeat & EMA Splits ─────────────

pub fn half_life_epoch(day: f64) -> i64 {
    (day.floor() as i64).div_euclid(EPOCH_DAYS)
}

pub fn epoch_pulse_id(account_key: &str, target_hash: &str, day: f64) -> String {
    let ep = half_life_epoch(day);
    hash_str(&format!("nav-pulse/2|{account_key}{target_hash}{ep}"))
}

pub fn update_ema_splits(prev: Option<&[f64]>, new_splits: &[f64], alpha: f64) -> Vec<f64> {
    let a = alpha.clamp(0.01, 1.0);
    match prev {
        Some(old) if old.len() == new_splits.len() => old
            .iter()
            .zip(new_splits)
            .map(|(o, n)| ((1.0 - a) * o + a * n).round())
            .collect(),
        _ => new_splits.to_vec(),
    }
}

// ───────────── Phase 5 Part 2 + Phase 7 Part 4: 4-Quota Portfolio & Rep Best Swap ─────────────

pub fn valid_batch_id(id: &str) -> bool {
    let b = id.as_bytes();
    (16..=64).contains(&b.len()) && b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

pub fn compute_course_traits(steps: &[Step], splits: &[f64], discount: f64, runs: f64) -> u32 {
    let mut traits = 0u32;
    let no_fine = steps.iter().all(|s| {
        !matches!(
            s.gesture,
            Some("swipe-left" | "swipe-right" | "swipe-up" | "swipe-down")
        )
    });
    if no_fine {
        traits |= TRAIT_NO_FINE_GESTURE;
    }
    // 서버 등록 체크포인트(screen/region/page/element) 중 비전 전용 격자가 아니면 트리 접근 가능
    traits |= TRAIT_TREE_ACCESSIBLE;
    let no_rapid = splits.windows(2).all(|w| (w[1] - w[0]) >= 800.0);
    let low_fail = runs <= 0.0 || (discount / runs) <= 0.15;
    if no_rapid && low_fail {
        traits |= TRAIT_LOW_DENSITY_CALM;
    }
    traits
}

#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioCourse {
    pub course: String,
    pub start_cp: String,
    pub goal_cp: String,
    pub runs: f64,
    pub tier: String,
    pub grade: i32,
    pub pace_ms: f64,
    pub interaction_traits: u32,
    pub superseded: bool,
}

/// 4-쿼터 다양성 포트폴리오 선발 (`GET /nav/trials v2`): 규칙은 premium-discount-nav 의
/// [`premium_discount_nav::select_stratified_portfolio`] 하나다(앱과 같은 규칙).
/// * Quota A (진입 맥락, 최대 6) · Quota B (고속 정예, 최대 6) · Quota D (포용 접근성 표준, 최대 4) · Quota C (대중 기준선, 최대 6) + 내 코스
/// * Quota C 는 대체(superseded)된 코스와 discount 코스를 받지 않는다(좀비 코스 유입 막기)
pub fn select_stratified_portfolio(candidates: &[PortfolioCourse], entry_cp: Option<&str>, mine_ids: &[String]) -> Vec<String> {
    use premium_discount_verdict::Tier;
    let tier = |t: &str| match t {
        "premium" => Tier::Premium,
        "discount" => Tier::Discount,
        "neutral" => Tier::Neutral,
        _ => Tier::Unrated,
    };
    let cands: Vec<premium_discount_nav::PortfolioCourse> = candidates
        .iter()
        .map(|c| premium_discount_nav::PortfolioCourse {
            course: c.course.clone(),
            start_cp: c.start_cp.clone(),
            goal_cp: c.goal_cp.clone(),
            runs: c.runs,
            tier: tier(&c.tier),
            grade: c.grade,
            pace_ms: c.pace_ms,
            interaction_traits: (c.interaction_traits & u32::from(u16::MAX)) as u16,
            superseded: c.superseded,
        })
        .collect();
    premium_discount_nav::select_stratified_portfolio(&cands, entry_cp, mine_ids)
}

/// Phase 5 Part 2: `duplicate: true` 재주행자가 무마찰(`kind != -1`)로 종전 대표 기록보다 200ms 이상 단축했을 때
/// `nav_courses.runs` 증가 없이 `sums` 에서 과거 기여분만 차분 치환하고 `record` 를 갱신한다.
pub fn representative_best_swap(
    old_sums: &[f64],
    old_rep: &[f64],
    new_splits: &[f64],
    old_record: &[f64],
    new_kind: i32,
) -> Option<(Vec<f64>, Vec<f64>)> {
    if new_kind == -1
        || old_sums.len() != new_splits.len()
        || old_rep.len() != new_splits.len()
        || old_record.len() != new_splits.len()
        || new_splits.is_empty()
    {
        return None;
    }
    let old_total = *old_rep.last()?;
    let new_total = *new_splits.last()?;
    if new_total >= old_total - 200.0 {
        return None;
    }
    let next_sums: Vec<f64> = old_sums
        .iter()
        .zip(old_rep)
        .zip(new_splits)
        .map(|((s, o), n)| (*s - *o + *n).max(0.0))
        .collect();
    let rec_total = *old_record.last()?;
    let next_record = if new_total < rec_total {
        new_splits.to_vec()
    } else {
        old_record.to_vec()
    };
    Some((next_sums, next_record))
}

// ───────────── Phase 5 Part 3: Consensus Medoid Vectors & Wallet Claim ─────────────

pub fn is_allowed_upload_space(space: &str) -> bool {
    let b = space.as_bytes();
    if b.is_empty() || b.len() > 64 || space == HASH_NGRAM_SPACE || space.contains(':') {
        return false;
    }
    let head = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    head(b[0]) && b[1..].iter().all(|&c| head(c) || matches!(c, b'.' | b'_' | b'-'))
}

fn dot_norm(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt();
    if norm <= 1e-12 {
        return v.to_vec();
    }
    v.iter().map(|&x| (x as f64 / norm) as f32).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum VectorConsensusAction {
    ConfirmMerge { merged: Vec<f32>, confirmations: u32, lock: bool },
    KeepExisting,
    TrackCandidate { candidate: Vec<f32>, hits: u32 },
    ReplaceWithCandidate { replacement: Vec<f32> },
}

/// Phase 5 Part 3: 최초 선점 오염(First-Mover Poisoning)을 차단하는 2-단계 신뢰 합의 대표 벡터 판정.
pub fn evaluate_vector_consensus(
    cur: &[f32],
    confirmations: u32,
    locked: bool,
    cand: Option<&[f32]>,
    cand_hits: u32,
    incoming: &[f32],
) -> VectorConsensusAction {
    if locked || cur.len() != incoming.len() {
        return VectorConsensusAction::KeepExisting;
    }
    let cos = dot_norm(cur, incoming);
    if cos >= CONSENSUS_MATCH_COS {
        let raw: Vec<f32> = cur.iter().zip(incoming).map(|(c, n)| 0.8 * c + 0.2 * n).collect();
        let next_conf = confirmations.saturating_add(1);
        return VectorConsensusAction::ConfirmMerge {
            merged: l2_normalize(&raw),
            confirmations: next_conf,
            lock: next_conf >= CONSENSUS_LOCK_CONFIRMATIONS,
        };
    }
    // 최초 1인 임시 벡터(confirmations == 1)와 충돌(cos < 0.65): 후발 다수파 후보 추적 후 2인 합의 시 자동 교체!
    if confirmations <= 1 && cos < CONSENSUS_CONFLICT_COS {
        if let Some(cvec) = cand.filter(|c| c.len() == incoming.len()) {
            if dot_norm(cvec, incoming) >= CONSENSUS_MATCH_COS {
                let next_hits = cand_hits.saturating_add(1);
                let raw: Vec<f32> = cvec.iter().zip(incoming).map(|(a, b)| 0.5 * a + 0.5 * b).collect();
                let merged = l2_normalize(&raw);
                if next_hits >= CONSENSUS_REPLACE_HITS {
                    return VectorConsensusAction::ReplaceWithCandidate { replacement: merged };
                }
                return VectorConsensusAction::TrackCandidate { candidate: merged, hits: next_hits };
            }
        }
        return VectorConsensusAction::TrackCandidate {
            candidate: l2_normalize(incoming),
            hits: 1,
        };
    }
    VectorConsensusAction::KeepExisting
}

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// L2 정규화 f32 벡터를 Int8 스칼라 양자화 + Base64 로 압축한다 (`GET /nav/trials` 배달용).
pub fn encode_unit_i8_b64(unit: &[f32]) -> String {
    let bytes: Vec<u8> = unit
        .iter()
        .map(|&v| {
            let q = (v.clamp(-1.0, 1.0) * 127.0).round().clamp(-127.0, 127.0) as i8;
            q as u8
        })
        .collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[((n >> 18) & 63) as usize] as char);
        out.push(B64_TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { B64_TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64_TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn decode_unit_i8_b64(b64: &str) -> Option<Vec<f32>> {
    let mut bytes = Vec::with_capacity(b64.len() * 3 / 4);
    let mut vals = [0u8; 4];
    let mut k = 0usize;
    let mut pad = 0usize;
    for ch in b64.bytes() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                0
            }
            _ => return None,
        };
        vals[k] = v;
        k += 1;
        if k == 4 {
            let n = ((vals[0] as u32) << 18) | ((vals[1] as u32) << 12) | ((vals[2] as u32) << 6) | (vals[3] as u32);
            bytes.push(((n >> 16) & 0xff) as u8);
            if pad < 2 {
                bytes.push(((n >> 8) & 0xff) as u8);
            }
            if pad < 1 {
                bytes.push((n & 0xff) as u8);
            }
            k = 0;
            pad = 0;
        }
    }
    if bytes.is_empty() {
        return None;
    }
    let raw: Vec<f32> = bytes.into_iter().map(|b| (b as i8) as f32 / 127.0).collect();
    Some(l2_normalize(&raw))
}

/// Phase 5 Part 3: 지갑 → 세션 이관 시 세션 키 앞으로 미리 심어 둘 `nav_registrations` 키 목록을 산출한다.
/// 로그인 직후 세션 계정으로 동일 코스를 재주행해도 `duplicate: true` 가 작동해 공용 `runs` 부풀림 0건을 보장한다.
pub fn plan_wallet_claim_registrations(session_key: &str, wallet_course_ids: &[String]) -> Vec<String> {
    wallet_course_ids.iter().map(|c| hash_str(&format!("{session_key}{c}"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navtrials::{account_key, checkpoint_id, course_chain, parse_run};
    use premium_discount_worker_common::{obj, parse_str};

    fn fill(c: char) -> String {
        format!("0x{}", c.to_string().repeat(40))
    }

    #[test]
    fn cn246_single_account_multi_variant_detour_ballots_deduplicate_to_one_vote() {
        let app = fill('a');
        let goal = fill('9');
        let target_r2 = fill('2');
        let acc = account_key(&V::from("secret"), "wallet", &fill('1')).unwrap();
        // 5 different detour courses all bypass target_r2 toward the same goal: ballot_id is identical!
        let mut ledger: HashMap<String, i32> = HashMap::new();
        let mut discount_count = 0i32;
        for _variant in 0..5 {
            let bid = ballot_id(&acc, &app, &goal, &target_r2);
            let prev = ledger.get(&bid).copied();
            match apply_ballot_delta(prev, -1) {
                BallotDelta::First(s) => {
                    ledger.insert(bid, s);
                    if s == -1 {
                        discount_count += 1;
                    }
                }
                BallotDelta::Duplicate => {}
                BallotDelta::Flipped { old, new } => {
                    ledger.insert(bid, new);
                    if old == -1 {
                        discount_count -= 1;
                    }
                    if new == -1 {
                        discount_count += 1;
                    }
                }
            }
        }
        assert_eq!(discount_count, 1, "5 detour variants from one wallet must only count 1 vote");
        // Reversible opinion flip: user switches from -1 to +1 => discount returns to 0!
        let bid = ballot_id(&acc, &app, &goal, &target_r2);
        assert_eq!(apply_ballot_delta(ledger.get(&bid).copied(), 1), BallotDelta::Flipped { old: -1, new: 1 });
        // Goal-scoped route_fit_hash separates CP_Cart for checkout vs CP_Cart for cart-management goal!
        assert_ne!(route_fit_hash(&target_r2, &fill('9')), route_fit_hash(&target_r2, &fill('8')));
    }

    #[test]
    fn cn247_server_route_marks_v2_protects_tap_vs_swipe_modality() {
        let s0 = obj! { "kind" => "page", "hash" => fill('a') };
        let c_swipe = obj! { "kind" => "element", "hash" => fill('c'), "gesture" => "swipe-left" };
        let c_tap = obj! { "kind" => "element", "hash" => fill('c') };
        let finish = obj! { "kind" => "page", "hash" => fill('d') };
        let ghost = V::from(vec![s0.clone(), c_swipe.clone(), finish.clone()]);
        let marks = route_marks_v2(&[s0, c_tap, finish], &ghost, 13_800.0, 1, 15_000.0, 0).unwrap().unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].1, ServerMarkOutcome::EquivalentPass);
        assert_eq!(marks[0].1.sign(), None);
    }

    #[test]
    fn cn250_server_shortcut_duels_pioneer_promote_after_3_distinct_accounts_and_dedup_repeats() {
        let app = fill('a');
        let s0 = fill('1');
        let g1 = fill('2');
        let g2 = fill('3');
        let alt = fill('4');
        let goal = fill('9');
        let today = 20_774.0;
        let ghost_ids = [
            checkpoint_id("screen", &s0, None),
            checkpoint_id("region", &g1, None),
            checkpoint_id("region", &g2, None),
            checkpoint_id("screen", &goal, None),
        ];
        let ghost_chain = course_chain(&app, &ghost_ids);
        let ghost_steps = vec![
            obj! { "kind" => "screen", "hash" => s0.as_str(), "id" => ghost_ids[0].as_str(), "course" => ghost_chain[0].as_str() },
            obj! { "kind" => "region", "hash" => g1.as_str(), "id" => ghost_ids[1].as_str(), "course" => ghost_chain[1].as_str() },
            obj! { "kind" => "region", "hash" => g2.as_str(), "id" => ghost_ids[2].as_str(), "course" => ghost_chain[2].as_str() },
            obj! { "kind" => "screen", "hash" => goal.as_str(), "id" => ghost_ids[3].as_str(), "course" => ghost_chain[3].as_str() },
        ];
        let ghost_avg = [0.0, 6_000.0, 12_000.0, 20_000.0];

        // 3 distinct accounts take Alt at fork S0 and save 6s (> 1.5s & 50% > 15%), each with different tail paths (runs = 1 < 11).
        let mut reg_set: HashSet<String> = HashSet::new();
        let (mut wins, mut saved_sum, mut margin_sum) = (0u32, 0.0f64, 0.0f64);
        for (idx, tail_char) in ['5', '6', '7'].iter().enumerate() {
            let tail = fill(*tail_char);
            let run_ids = [
                checkpoint_id("screen", &s0, None),
                checkpoint_id("region", &alt, None),
                checkpoint_id("region", &tail, None),
                checkpoint_id("screen", &goal, None),
            ];
            let run_chain = course_chain(&app, &run_ids);
            let json = format!(
                r#"{{"schema":"trial/1","app":"{app}","course":"{}","steps":[{{"kind":"screen","hash":"{s0}","split_ms":0}},{{"kind":"region","hash":"{alt}","split_ms":4000}},{{"kind":"region","hash":"{tail}","split_ms":9000}},{{"kind":"screen","hash":"{goal}","split_ms":14000}}],"total_ms":14000,"type":null,"day":{today},"recipe":"pd-verdict/2","ghost":"{}"}}"#,
                run_chain[3], ghost_chain[3]
            );
            let parsed = parse_run(&parse_str(&json).unwrap(), today).unwrap();
            let duels = extract_server_shortcut_duels(&parsed, &ghost_steps, &ghost_avg).unwrap();
            assert_eq!(duels.len(), 1);
            assert_eq!(duels[0].taken_cp, run_ids[1]);
            assert_eq!(duels[0].skipped_cp, ghost_ids[1]);
            assert_eq!(duels[0].saved_ms, 6_000.0);

            let acc = format!("acc-{idx}");
            // Repeat submissions by the same account must deduplicate to 1.
            for _repeat in 0..3 {
                let rid = duel_reg_id(&acc, &duels[0].fork_course, &duels[0].taken_cp, &duels[0].skipped_cp);
                if reg_set.insert(rid) {
                    wins += 1;
                    saved_sum += duels[0].saved_ms;
                    margin_sum += duels[0].margin;
                }
            }
        }
        assert_eq!(wins, 3);
        assert!(should_pioneer_promote(wins, saved_sum, margin_sum, 0, 0));
    }

    #[test]
    fn cn254_dead_end_marks_validate_single_checkpoint_and_deduplicate_per_account() {
        let app = fill('a');
        let trap_cp = fill('b');
        let today = 20_774.0;
        let payload = parse_str(&format!(
            r#"[{{"app":"{app}","checkpoint_id":"{trap_cp}","recipe":"pd-verdict/2","day":{today}}}]"#
        ))
        .unwrap();
        let parsed = parse_dead_end_marks(&payload, today).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].checkpoint_id, trap_cp);
        // 3 distinct accounts accumulate 3 discount marks -> Tier::Discount without exposing any path!
        let mut seen = HashSet::new();
        for acc in ["u1", "u1", "u2", "u3"] {
            seen.insert(dead_end_ballot_id(acc, &app, &trap_cp));
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn cn258_epoch_pulse_blocks_same_epoch_burst_and_allows_next_14d_epoch_refresh() {
        let acc = "account-key-1";
        let course = fill('c');
        let day0 = 20_776.0; // epoch 1484
        let same_epoch_day = day0 + 5.0;
        let next_epoch_day = day0 + 15.0;
        assert_eq!(
            epoch_pulse_id(acc, &course, day0),
            epoch_pulse_id(acc, &course, same_epoch_day),
            "burst retries inside same 14-day epoch must collide"
        );
        assert_ne!(
            epoch_pulse_id(acc, &course, day0),
            epoch_pulse_id(acc, &course, next_epoch_day),
            "re-run in next 14-day epoch refreshes decayed evidence"
        );
    }

    #[test]
    fn cn261_stratified_4quota_portfolio_delivers_elite_entry_and_accessible_courses() {
        let mut cands = Vec::new();
        // 12 slow popular trap courses (runs=50, discount)
        for i in 0..12u8 {
            cands.push(PortfolioCourse {
                course: format!("pop-{i:02}"),
                start_cp: "start-early".into(),
                goal_cp: format!("goal-{i:02}"),
                runs: 50.0 + f64::from(i),
                tier: "discount".into(),
                grade: -1,
                pace_ms: 9_000.0,
                interaction_traits: 0,
                superseded: false,
            });
        }
        // Rank 13 elite premium shortcut (runs=12 >= 11)
        cands.push(PortfolioCourse {
            course: "elite-fast".into(),
            start_cp: "start-early".into(),
            goal_cp: "goal-00".into(),
            runs: 12.0,
            tier: "premium".into(),
            grade: 2,
            pace_ms: 2_500.0,
            interaction_traits: TRAIT_TREE_ACCESSIBLE,
            superseded: false,
        });
        // Late-game entry-matched course (runs=11)
        cands.push(PortfolioCourse {
            course: "boss-entry".into(),
            start_cp: "scene-8".into(),
            goal_cp: "boss-8".into(),
            runs: 11.0,
            tier: "neutral".into(),
            grade: 0,
            pace_ms: 6_000.0,
            interaction_traits: TRAIT_TREE_ACCESSIBLE,
            superseded: false,
        });
        // Accessible-viable keyboard-only course (runs=11)
        cands.push(PortfolioCourse {
            course: "accessible-clean".into(),
            start_cp: "start-early".into(),
            goal_cp: "goal-acc".into(),
            runs: 11.0,
            tier: "neutral".into(),
            grade: 0,
            pace_ms: 7_500.0,
            interaction_traits: TRAIT_NO_FINE_GESTURE | TRAIT_TREE_ACCESSIBLE,
            superseded: false,
        });

        let selected = select_stratified_portfolio(&cands, Some("scene-8"), &[]);
        assert!(selected.contains(&"elite-fast".to_string()), "Quota B must rescue elite shortcut");
        assert!(selected.contains(&"boss-entry".to_string()), "Quota A must deliver ?entry= matched course");
        assert!(selected.contains(&"accessible-clean".to_string()), "Quota D must deliver accessible-viable course");
        // Quota C 는 대체 · discount 코스를 받지 않는다: 12개 함정 코스(discount)는 하나도 들어오지 않는다
        assert!(selected.iter().all(|c| !c.starts_with("pop-")), "{selected:?}");
    }

    #[test]
    fn cn262_representative_best_swap_updates_sums_and_record_without_inflating_runs() {
        // First run: 4000ms (single user -> sums = [0, 2000, 4000], record = [0, 2000, 4000], runs = 1).
        let (sums1, rec1) = representative_best_swap(&[0.0, 2000.0, 4000.0], &[0.0, 2000.0, 4000.0], &[0.0, 1000.0, 2500.0], &[0.0, 2000.0, 4000.0], 1).unwrap();
        assert_eq!(sums1, vec![0.0, 1000.0, 2500.0]);
        assert_eq!(rec1, vec![0.0, 1000.0, 2500.0]);
        // After deleting mine and running again in 2000ms, nav_registrations still holds old rep [0, 1000, 2500] -> exact diff!
        let (sums2, rec2) = representative_best_swap(&sums1, &[0.0, 1000.0, 2500.0], &[0.0, 800.0, 2000.0], &rec1, 1).unwrap();
        assert_eq!(sums2, vec![0.0, 800.0, 2000.0]);
        assert_eq!(rec2, vec![0.0, 800.0, 2000.0]);
        // Slower or discount run must not swap.
        assert!(representative_best_swap(&sums2, &[0.0, 800.0, 2000.0], &[0.0, 900.0, 2100.0], &rec2, 1).is_none());
        assert!(representative_best_swap(&sums2, &[0.0, 800.0, 2000.0], &[0.0, 500.0, 1500.0], &rec2, -1).is_none());
    }

    #[test]
    fn cn266_consensus_medoid_evicts_poisoned_provisional_vector_and_locks_at_3() {
        let poison = vec![0.0, 0.0, 1.0];
        let good1 = vec![0.99, 0.141, 0.0];
        let good2 = vec![0.985, 0.172, 0.0];
        let good3 = vec![0.992, 0.126, 0.0];
        // First legitimate user conflicts with poisoned provisional vector -> enters candidate slot.
        let a1 = evaluate_vector_consensus(&poison, 1, false, None, 0, &good1);
        let cand = match a1 {
            VectorConsensusAction::TrackCandidate { candidate, hits } => {
                assert_eq!(hits, 1);
                candidate
            }
            other => panic!("expected TrackCandidate, got {other:?}"),
        };
        // Second independent legitimate user agrees with candidate -> replaces poisoned vector!
        let a2 = evaluate_vector_consensus(&poison, 1, false, Some(&cand), 1, &good2);
        let replaced = match a2 {
            VectorConsensusAction::ReplaceWithCandidate { replacement } => replacement,
            other => panic!("expected ReplaceWithCandidate, got {other:?}"),
        };
        // Third agreeing user reaches confirmations=3 and locks.
        let a3 = evaluate_vector_consensus(&replaced, 2, false, None, 0, &good3);
        match a3 {
            VectorConsensusAction::ConfirmMerge { confirmations, lock, merged } => {
                assert_eq!(confirmations, 3);
                assert!(lock);
                let b64 = encode_unit_i8_b64(&merged);
                let dec = decode_unit_i8_b64(&b64).unwrap();
                assert!(dot_norm(&merged, &dec) > 0.999);
            }
            other => panic!("expected ConfirmMerge lock, got {other:?}"),
        }
        // Locked vector rejects any later tampering.
        assert_eq!(evaluate_vector_consensus(&replaced, 3, true, None, 0, &poison), VectorConsensusAction::KeepExisting);
        assert!(!is_allowed_upload_space(HASH_NGRAM_SPACE));
        assert!(!is_allowed_upload_space("raon-ve:335544320"));
        assert!(is_allowed_upload_space("raon-ve-so400m-v1"));
    }

    #[test]
    fn cn267_wallet_claim_preseeds_session_registrations_preventing_run_inflation() {
        let session_k = account_key(&V::from("secret"), "session", &fill('a')).unwrap();
        let wallet_k = account_key(&V::from("secret"), "wallet", &fill('b')).unwrap();
        let courses = vec![fill('1'), fill('2')];
        let preseeded = plan_wallet_claim_registrations(&session_k, &courses);
        assert_eq!(preseeded.len(), 2);
        // Session re-submission produces the exact same registration key -> ON CONFLICT DO NOTHING -> duplicate: true (+0 runs)!
        assert_eq!(preseeded[0], hash_str(&format!("{session_k}{}", courses[0])));
        assert_ne!(preseeded[0], hash_str(&format!("{wallet_k}{}", courses[0])));
    }

    #[test]
    fn cn277_multi_layer_abuse_defense_blocks_detour_ballots_wallet_inflation_and_batch_replays() {
        let session_k = account_key(&V::from("secret"), "session", &fill('a')).unwrap();
        let wallet_k = account_key(&V::from("secret"), "wallet", &fill('b')).unwrap();
        let app = fill('9');
        let goal = fill('8');
        let target = fill('7');
        // 1) 5 detour variants from one wallet collapse to 1 ballot_id
        let ballots: HashSet<String> = (0..5).map(|_| ballot_id(&wallet_k, &app, &goal, &target)).collect();
        assert_eq!(ballots.len(), 1);
        // 2) Wallet->session claim pre-seeds session registration so run inflation is 0
        let course = fill('3');
        let preseeded = plan_wallet_claim_registrations(&session_k, std::slice::from_ref(&course));
        assert_eq!(preseeded, vec![hash_str(&format!("{session_k}{course}"))]);
        // 3) batch_id validation enforces safe idempotent receipt keys
        assert!(valid_batch_id("batch_1234567890abcdef"));
        assert!(!valid_batch_id("short"));
    }
}
