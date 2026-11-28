//! 공개 코스 고르기의 공정성: 접근성 코스를 지키는 대체 판정, 기기 안 개인 속도 정규화, 창 제목의 작업 공간 이름 지우기,
//! 4-쿼터 다양성 포트폴리오(서버 `GET /nav/trials` v2 와 앱이 같은 규칙을 쓴다).
//!
//! - **특성 보존 대체**([`is_course_superseded_preserving_traits`]): 옛 코스가 가진 보호 특성(세밀한 제스처 없음 · 트리 접근 가능)을
//!   새 코스가 다 갖추지 못하면 대체가 아니다. 보는 사람의 빠른 마우스 코스가 전맹 사용자의 Quota D 코스를 영구히 밀어내지 못한다.
//! - **개인 속도**([`PaceScale`]): `ρ_u` 를 1 ~ 5 로 잘라 구간 시간 · 다리 유지 시간(TTL)을 같은 배율로 맞춘다. 기기 안에서만 쓰고
//!   서버로 보내지 않는다.
//! - **작업 공간 이름 지우기**([`scrub_workspace_identity`]): 창 제목 · 프롬프트의 파일 경로 · Git 브랜치 · 버전 · 시각을 자리표로
//!   바꿔, 같은 편집기 · 터미널 화면이 파일이나 브랜치가 바뀌어도 같은 체크포인트가 되게 한다.
//! - **4-쿼터 포트폴리오**([`select_stratified_portfolio`]): A 진입 맥락 · B 빠른 정예 · D 접근성 표준 · C 대중 기준선, 그리고 내
//!   코스. Quota C 는 대체된 코스와 discount 코스를 받지 않는다(좀비 코스 유입 막기).

use std::collections::{HashMap, HashSet};

use premium_discount_verdict::Tier;

use crate::trial::{is_course_superseded, PublicCourse, TRAIT_NO_FINE_GESTURE, TRAIT_TREE_ACCESSIBLE};

/// 보호하는 접근성 특성 (Quota D 의 조건)
pub const PROTECTED_ACCESSIBLE_TRAITS: u16 = TRAIT_NO_FINE_GESTURE | TRAIT_TREE_ACCESSIBLE;

/// [`is_course_superseded`] 에 특성 보존을 더한 것: 옛 코스의 보호 특성을 새 코스가 다 갖춰야 대체할 수 있다
pub fn is_course_superseded_preserving_traits(
    old: &PublicCourse,
    new: &PublicCourse,
    duel_wins: u32,
    duel_losses: u32,
    old_recent_friction_worse: bool,
) -> bool {
    let required = old.interaction_traits & PROTECTED_ACCESSIBLE_TRAITS;
    if (new.interaction_traits & required) != required {
        return false;
    }
    is_course_superseded(old, new, duel_wins, duel_losses, old_recent_friction_worse)
}

/// 기기 안 개인 속도 배율 `max(1, ρ_u)` (5 에서 자른다). 서버 전송 0 바이트
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaceScale(f64);

impl PaceScale {
    pub const NEUTRAL: PaceScale = PaceScale(1.0);
    pub const MAX: f64 = 5.0;

    #[inline]
    pub fn from_rho_u(rho_u: f64) -> PaceScale {
        PaceScale(if rho_u.is_finite() { rho_u.clamp(1.0, Self::MAX) } else { 1.0 })
    }

    #[inline]
    pub fn get(self) -> f64 {
        self.0
    }

    /// 관측한 구간 시간을 보통 속도로 (나눈다)
    #[inline]
    pub fn normalize_split_ms(self, observed_ms: u32) -> f64 {
        observed_ms as f64 / self.0
    }

    /// 기본 유지 시간을 이 사용자의 속도로 (곱한다)
    #[inline]
    pub fn scale_ttl_ms(self, base_ttl_ms: i64) -> i64 {
        (base_ttl_ms as f64 * self.0).round() as i64
    }
}

/// 자리표
pub const PATH_TOKEN: &str = "<PATH>";
pub const BRANCH_TOKEN: &str = "<BRANCH>";
pub const VERSION_TOKEN: &str = "<VER>";
pub const TIME_TOKEN: &str = "<TIME>";

fn is_version(s: &str) -> bool {
    let body = s.strip_prefix(['v', 'V', '@']).unwrap_or(s);
    let mut parts = body.split('.');
    let first = parts.next().unwrap_or("");
    let numeric = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
    let rest: Vec<&str> = parts.collect();
    // v12 · 1.2 · 1.2.3 · 2026.11.1, 꼬리 -rc1 · -beta.2 는 허용
    let tail_ok = |p: &str| {
        let core = p.split(['-', '+']).next().unwrap_or("");
        numeric(core)
    };
    let prefixed = s.len() != body.len();
    numeric(first.split(['-', '+']).next().unwrap_or("")) && (prefixed || !rest.is_empty()) && rest.iter().all(|p| tail_ok(p))
}

fn is_time(s: &str) -> bool {
    let hm = |t: &str| {
        let parts: Vec<&str> = t.split(':').collect();
        (2..=3).contains(&parts.len()) && parts.iter().all(|p| (1..=2).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()))
    };
    let date = |t: &str| {
        let parts: Vec<&str> = t.split(['-', '/']).collect();
        parts.len() == 3 && parts[0].len() == 4 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    hm(s) || date(s)
}

fn is_path(s: &str) -> bool {
    let drive = s.len() > 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() && matches!(s.as_bytes()[2], b'\\' | b'/');
    let sep = s.contains('/') || s.contains('\\');
    // 파일 이름: 이름.확장자 (확장자는 1 ~ 5 글자 영숫자, 이름에 글자가 있다). 사이트 주소(`github.com`)는 화면을 가리키므로 남긴다
    const SITE_SUFFIXES: [&str; 15] = ["com", "net", "org", "io", "dev", "app", "ai", "co", "kr", "jp", "de", "uk", "us", "edu", "gov"];
    let file = s.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && stem.chars().any(|c| c.is_alphabetic())
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
            && !SITE_SUFFIXES.contains(&ext.to_ascii_lowercase().as_str())
    });
    drive || s.starts_with("~/") || s.starts_with('~') && s.len() == 1 || (sep && s.chars().any(|c| c.is_alphanumeric())) || file
}

/// 창 제목 · 프롬프트에서 작업 공간을 가리키는 낱말을 자리표로 바꾼다: 파일 경로 · 파일 이름 → `<PATH>`, 괄호 안 브랜치
/// (`(main)` · `[feat/x]`) · `branch X` → `<BRANCH>`, 버전(`v1.4.2` · `1.2.3`) → `<VER>`, 시각 · 날짜(`12:30` · `2026-11-28`) →
/// `<TIME>`. 나머지 낱말과 띄어쓰기 한 칸은 그대로다
pub fn scrub_workspace_identity(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let lower = w.to_ascii_lowercase();
        // "branch X" · "on branch X"
        if (lower == "branch" || lower == "branch:") && i + 1 < words.len() {
            out.push(w.to_string());
            out.push(BRANCH_TOKEN.to_string());
            i += 2;
            continue;
        }
        let trimmed = w.trim_end_matches([',', ';', ':', '$', '#', '>']);
        let tail = &w[trimmed.len()..];
        let bracketed = (trimmed.starts_with('(') && trimmed.ends_with(')')) || (trimmed.starts_with('[') && trimmed.ends_with(']'));
        let inner = if bracketed && trimmed.len() >= 2 { &trimmed[1..trimmed.len() - 1] } else { trimmed };
        let token = if bracketed && !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.')) && !is_version(inner) {
            Some(BRANCH_TOKEN)
        } else if is_time(inner) {
            Some(TIME_TOKEN)
        } else if is_version(inner) {
            Some(VERSION_TOKEN)
        } else if is_path(inner) {
            Some(PATH_TOKEN)
        } else {
            None
        };
        match token {
            Some(t) if bracketed => out.push(format!("{}{t}{}{tail}", &trimmed[..1], &trimmed[trimmed.len() - 1..])),
            Some(t) => out.push(format!("{t}{tail}")),
            None => out.push(w.to_string()),
        }
        i += 1;
    }
    out.join(" ")
}

/// 포트폴리오의 쿼터 크기
pub const QUOTA_ENTRY: usize = 6;
pub const QUOTA_ELITE: usize = 6;
pub const QUOTA_POPULAR: usize = 6;
pub const QUOTA_ACCESSIBLE: usize = 4;
/// 같은 (시작, 결승) 쌍은 1차 쿼터(A · B)에서 이만큼까지
pub const PER_GOAL_CAP: usize = 2;

/// 포트폴리오 후보 코스 하나
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioCourse {
    pub course: String,
    pub start_cp: String,
    pub goal_cp: String,
    pub runs: f64,
    pub tier: Tier,
    pub grade: i32,
    pub pace_ms: f64,
    pub interaction_traits: u16,
    pub superseded: bool,
}

/// 4-쿼터 다양성 포트폴리오 (뽑힌 코스 id, 뽑힌 순서):
/// * Quota A (진입 맥락, 최대 6): `start_cp == entry_cp` 이고 대체되지 않음 (premium · 등급 · 빠른 순)
/// * Quota B (빠른 정예, 최대 6): premium 이고 대체되지 않음 (빠른 순)
/// * Quota D (접근성 표준, 최대 4): 보호 특성을 다 갖추고 discount 가 아니며 대체되지 않음
/// * Quota C (대중 기준선, 최대 6): 대체되지 않고 discount 가 아닌 것만 (주행 수 순)
/// * 내 코스는 늘 넣는다
///
/// A · B 에서는 같은 (시작, 결승) 쌍을 [`PER_GOAL_CAP`] 개까지만 받는다
pub fn select_stratified_portfolio(candidates: &[PortfolioCourse], entry_cp: Option<&str>, mine_ids: &[String]) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    let mut picked: HashSet<String> = HashSet::new();
    let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut take = |list: &[&PortfolioCourse], limit: usize, enforce_cap: bool, selected: &mut Vec<String>| {
        let mut taken = 0usize;
        for c in list {
            if taken >= limit {
                break;
            }
            if picked.contains(&c.course) {
                continue;
            }
            let pair = (c.start_cp.clone(), c.goal_cp.clone());
            if enforce_cap && *pair_counts.get(&pair).unwrap_or(&0) >= PER_GOAL_CAP {
                continue;
            }
            picked.insert(c.course.clone());
            *pair_counts.entry(pair).or_insert(0) += 1;
            selected.push(c.course.clone());
            taken += 1;
        }
    };
    let premium = |c: &PortfolioCourse| i32::from(c.tier == Tier::Premium);

    if let Some(entry) = entry_cp.filter(|e| !e.is_empty()) {
        let mut qa: Vec<&PortfolioCourse> = candidates.iter().filter(|c| c.start_cp == entry && !c.superseded).collect();
        qa.sort_by(|a, b| premium(b).cmp(&premium(a)).then(b.grade.cmp(&a.grade)).then(a.pace_ms.total_cmp(&b.pace_ms)));
        take(&qa, QUOTA_ENTRY, true, &mut selected);
    }

    let mut qb: Vec<&PortfolioCourse> = candidates.iter().filter(|c| c.tier == Tier::Premium && !c.superseded).collect();
    qb.sort_by(|a, b| a.pace_ms.total_cmp(&b.pace_ms).then(b.grade.cmp(&a.grade)).then(a.course.cmp(&b.course)));
    take(&qb, QUOTA_ELITE, true, &mut selected);

    let mut qd: Vec<&PortfolioCourse> = candidates
        .iter()
        .filter(|c| (c.interaction_traits & PROTECTED_ACCESSIBLE_TRAITS) == PROTECTED_ACCESSIBLE_TRAITS && c.tier != Tier::Discount && !c.superseded)
        .collect();
    qd.sort_by(|a, b| premium(b).cmp(&premium(a)).then(b.grade.cmp(&a.grade)).then(a.pace_ms.total_cmp(&b.pace_ms)));
    take(&qd, QUOTA_ACCESSIBLE, false, &mut selected);

    let mut qc: Vec<&PortfolioCourse> = candidates.iter().filter(|c| !c.superseded && c.tier != Tier::Discount).collect();
    qc.sort_by(|a, b| b.runs.total_cmp(&a.runs).then(a.course.cmp(&b.course)));
    take(&qc, QUOTA_POPULAR, false, &mut selected);

    for m in mine_ids {
        if !picked.contains(m) && candidates.iter().any(|c| &c.course == m) {
            picked.insert(m.clone());
            selected.push(m.clone());
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course(id: &str, tier: Tier, runs: f64, pace_ms: f64, traits: u16, superseded: bool) -> PortfolioCourse {
        PortfolioCourse {
            course: id.into(),
            start_cp: "start".into(),
            goal_cp: format!("goal-{id}"),
            runs,
            tier,
            grade: match tier {
                Tier::Premium => 2,
                Tier::Discount => -1,
                _ => 0,
            },
            pace_ms,
            interaction_traits: traits,
            superseded,
        }
    }

    #[test]
    fn quota_c_takes_no_zombie_and_quota_d_keeps_the_accessible_course() {
        let mut cands: Vec<PortfolioCourse> = (0..10).map(|i| course(&format!("zombie-{i}"), Tier::Discount, 90.0 + i as f64, 9_000.0, 0, false)).collect();
        cands.push(course("stale", Tier::Neutral, 200.0, 8_000.0, 0, true));
        cands.push(course("popular-ok", Tier::Neutral, 40.0, 7_000.0, 0, false));
        cands.push(course("keyboard-only", Tier::Neutral, 5.0, 12_000.0, PROTECTED_ACCESSIBLE_TRAITS, false));
        cands.push(course("elite", Tier::Premium, 12.0, 2_500.0, TRAIT_TREE_ACCESSIBLE, false));
        let got = select_stratified_portfolio(&cands, None, &["zombie-3".to_string()]);
        assert!(got.contains(&"elite".to_string()) && got.contains(&"keyboard-only".to_string()) && got.contains(&"popular-ok".to_string()), "{got:?}");
        assert!(!got.contains(&"stale".to_string()), "superseded courses never return through Quota C");
        assert_eq!(got.iter().filter(|c| c.starts_with("zombie")).count(), 1, "only my own course comes back: {got:?}");
        assert_eq!(got.last().map(String::as_str), Some("zombie-3"));
    }

    #[test]
    fn pace_scale_and_workspace_scrubbing() {
        assert_eq!(PaceScale::from_rho_u(0.4).get(), 1.0);
        assert_eq!(PaceScale::from_rho_u(3.0).normalize_split_ms(9_000), 3_000.0);
        assert_eq!(PaceScale::from_rho_u(9.0).get(), 5.0, "capped");
        assert_eq!(PaceScale::from_rho_u(f64::NAN), PaceScale::NEUTRAL);
        assert_eq!(PaceScale::from_rho_u(2.5).scale_ttl_ms(15_000), 37_500);

        assert_eq!(scrub_workspace_identity("main.rs - premium-discount - Visual Studio Code"), "<PATH> - premium-discount - Visual Studio Code");
        assert_eq!(scrub_workspace_identity("user@host:~/work/repo (feat/blind-nav)$"), "<PATH> (<BRANCH>)$");
        assert_eq!(scrub_workspace_identity("C:\\Users\\me\\report.docx - Word"), "<PATH> - Word");
        assert_eq!(scrub_workspace_identity("Release v1.4.2 built at 12:30"), "Release <VER> built at <TIME>");
        assert_eq!(scrub_workspace_identity("On branch main, 2026-11-28"), "On branch <BRANCH> <TIME>");
        assert_eq!(scrub_workspace_identity("Settings - Network"), "Settings - Network", "plain titles stay");
        // 파일이 바뀌어도 같은 화면은 같은 글이 된다
        assert_eq!(scrub_workspace_identity("lib.rs - app [main]"), scrub_workspace_identity("trial.rs - app [develop]"));
    }
}
