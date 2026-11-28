//! 웹 분석 질의 해석 ("이번 달 결제 이탈 원인이 뭐야?", "지난주 장바구니 페이지에서 클릭한 것")
//!
//! logis-center 의 `parse_analytic_search_query` 에서 **사전 단계**(STEP 1: 정확 · 교착어 접두 일치)와 **기간 재확정**
//! (STEP 11)을 옮겼다. 사전은 logis-center `full_bias.json` 의 분석 항목을 그대로 실었다(`data/analytic_dictionary.json`:
//! 사건 종류 5 · 행동 4 · 페이지 종류 10 · 기간 7(+ PD 의 주 2) · 계절 4 · 인사이트 3). 일치는 premium-discount-nms 의
//! 도메인 은행([`DomainBank::exact`])이 맡는다: 띄어쓰기를 무시하고, 한국어 조사를 떼고, 교착어는 접두로 맞춘다.
//!
//! - **날짜는 사전 · 모델이 내지 않는다.** 해석은 기간 키(`last_week`)와 계절 키(`summer`)만 고르고, 범위는
//!   [`crate::time::resolve`] 가 사용자의 시계로 다시 계산한다(logis-center 의 "LLM 날짜 불신 원칙"). 계절은 이미 시작한 가장
//!   최근 회차다(행동 기록은 미래에 없다).
//! - 같은 갈래에서 둘 이상이 걸리면 더 긴 글이, 같으면 앞에 나온 것이 이긴다. 사건 종류 · 페이지 종류 · 행동은 여럿을 함께 낸다.
//! - **인사이트**(전환 · 유입 · 원인 분석): logis-center 의 `insight` 는 영어 문장만 있고 정확 일치 목록이 없어서, 한국어 ·
//!   일본어 · 중국어 질문은 신경망 임베더 없이는 갈래를 정하지 못한다. PD 가 정확 일치 글(`pd_exact_match`)을 더했다.
//!   logis-center 에는 `sales_insight` 가 없고 `conversion_insight` 다(기획서의 이름과 다름).
//! - logis-center 의 벡터 단계(STEP 3 ~ 7: 1 ~ 6 낱말 조각, 놀람 점수, NMS)는 옮기지 않았다. 신경망 임베더를 넘기면
//!   인사이트만 벡터로 한 번 더 본다([`QueryParser::with_embedder`]).
//! - **기간 비교** (시나리오 5): 기간이 둘이면("이번 주 vs 지난주") 둘째를 비교 기준(`compare_key`)으로, 기간이 하나라도 비교하는
//!   말("vs" · "대비" · "비교" · "떨어졌" · "변화" …)이 있으면 바로 앞 기간을 기준으로 비교 창(`comparison`)을 낸다
//!   ([`crate::time::resolve_comparison_window`]). 예전 칸(`time_key` · `range`)은 그대로다.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use premium_discount_nms::analyze::{read_span, Analyzer, RuleAnalyzer};
use premium_discount_nms::embed::{HashedNgramEmbedder, SharedEmbedder, TextEmbedder};
use premium_discount_nms::normalize::{normalize, Normalized, Word};
use premium_discount_nms::{DomainBank, DomainSpec};
use serde::{Deserialize, Serialize};

use crate::time::{self, ComparisonWindow, SeasonAnchor, TimeRange};

/// logis-center 에서 옮긴 분석 사전 (+ PD 추가)
pub const DICTIONARY_JSON: &str = include_str!("../data/analytic_dictionary.json");
/// 한 번에 견주는 낱말 수 (구간 길이)
const MAX_SPAN_WORDS: usize = 4;
/// 기간 비교를 묻는 말. 영문은 낱말로, 그 밖은 글 안에서 찾는다
const COMPARE_MARKERS: [&str; 19] =
    ["vs", "vs.", "versus", "compare", "compared", "comparison", "대비", "비교", "떨어졌", "떨어진", "줄었", "늘었", "올랐", "변화", "달라졌", "比べ", "比較", "对比", "相比"];

/// 질의가 기간 비교를 묻는가 ([`COMPARE_MARKERS`])
fn asks_comparison(text: &str) -> bool {
    let lower = text.to_lowercase();
    COMPARE_MARKERS.iter().any(|m| if m.is_ascii() { lower.split(|c: char| !(c.is_alphanumeric() || c == '.')).any(|w| w == *m) } else { lower.contains(m) })
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterEntry {
    #[serde(default)]
    pub semantic: String,
    #[serde(default)]
    pub bias: String,
    #[serde(default)]
    pub prejudice: String,
    #[serde(default)]
    pub exact_match: Vec<String>,
    /// PD 가 더한 정확 일치 글
    #[serde(default)]
    pub pd_exact_match: Vec<String>,
    /// 페이지 종류: URL 경로 머리
    #[serde(default)]
    pub paths: Vec<String>,
    /// 행동: 낱말 줄기
    #[serde(default)]
    pub stems: Vec<String>,
    /// 행동을 남기는 사건 종류
    #[serde(default)]
    pub types: Vec<String>,
}

impl FilterEntry {
    fn phrases(list: &str) -> Vec<String> {
        list.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect()
    }

    fn exact(&self) -> Vec<String> {
        self.exact_match.iter().chain(&self.pd_exact_match).chain(&self.stems).cloned().collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AnalyticDictionary {
    pub analytic_event_filters: BTreeMap<String, FilterEntry>,
    pub analytic_action_filters: BTreeMap<String, FilterEntry>,
    pub analytic_page_filters: BTreeMap<String, FilterEntry>,
    pub time_filters: BTreeMap<String, FilterEntry>,
    pub season_filters: BTreeMap<String, FilterEntry>,
    pub insight: BTreeMap<String, FilterEntry>,
}

impl AnalyticDictionary {
    /// 함께 실은 사전
    pub fn builtin() -> &'static AnalyticDictionary {
        static DICT: OnceLock<AnalyticDictionary> = OnceLock::new();
        DICT.get_or_init(|| serde_json::from_str(DICTIONARY_JSON).expect("data/analytic_dictionary.json 은 사전 모양이다"))
    }

    /// URL 의 페이지 종류: `paths` 머리 중 가장 긴 것 (logis-center `page_kind_of`). 머리는 경로 조각 경계에서만 맞고,
    /// `/` 는 첫 화면(경로가 `/` 그대로)에만 맞는다. SPA 모달 · 드로어의 가상 주소(`#/…/__modal/<slug>`)는 그 slug 의 종류다:
    /// 종류 이름과 같거나(`cart`) 경로 머리와 같을 때(`basket` → `/basket`). 맞는 종류가 없으면 None 이다(밑의 페이지 종류를
    /// 물려받지 않는다)
    pub fn page_kind_of(&self, url: &str) -> Option<String> {
        if let Some(slug) = crate::event::modal_slug_of(url) {
            if self.analytic_page_filters.contains_key(&slug) {
                return Some(slug);
            }
            let path = format!("/{slug}");
            return self
                .analytic_page_filters
                .iter()
                .find(|(_, e)| e.paths.iter().any(|p| p.trim().trim_end_matches('/').eq_ignore_ascii_case(&path)))
                .map(|(k, _)| k.clone());
        }
        let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
        let path_start = rest.find('/').unwrap_or(rest.len());
        let path = &rest[path_start..];
        let path = path[..path.find(['?', '#']).unwrap_or(path.len())].to_lowercase();
        let path = if path.is_empty() { "/".to_string() } else { path };
        let mut best: Option<(&str, usize)> = None;
        for (kind, e) in &self.analytic_page_filters {
            for p in &e.paths {
                let p = p.trim().to_lowercase();
                let hit = if p == "/" {
                    path == "/"
                } else {
                    let base = p.trim_end_matches('/');
                    path == base || path.starts_with(&format!("{base}/"))
                };
                if hit && best.is_none_or(|(_, len)| p.len() > len) {
                    best = Some((kind.as_str(), p.len()));
                }
            }
        }
        best.map(|(k, _)| k.to_string())
    }

    /// 행동 종류가 남기는 사건 종류 (`add_to_cart` → click · touch)
    pub fn action_types(&self, action: &str) -> Vec<String> {
        self.analytic_action_filters.get(action).map(|e| e.types.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Time,
    Season,
    Event,
    Page,
    Action,
    Insight,
}

/// 질의에서 찾은 근거 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub category: Category,
    pub key: String,
    /// 걸린 낱말들
    pub phrase: String,
    pub hit: String,
}

/// 해석한 분석 질의
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticQuery {
    pub text: String,
    pub lang: String,
    pub time_key: Option<String>,
    pub season_key: Option<String>,
    /// 기간 (사용자 시계의 하루 경계). 기간 말이 없으면 None = 전 구간
    pub range: Option<TimeRange>,
    pub event_types: Vec<String>,
    pub page_kinds: Vec<String>,
    pub action_kinds: Vec<String>,
    pub insight: Option<String>,
    pub evidence: Vec<Evidence>,
    /// 비교 기준 기간 키: 기간이 둘 이상이면 `time_key` 가 아닌 첫 기간 ("이번 주 vs 지난주" → `last_week`). 없으면 None
    #[serde(default)]
    pub compare_key: Option<String>,
    /// 기간 비교 창: 기간이 둘이거나 비교하는 말이 있을 때 (진행 중인 기간은 같은 경과 시간으로 자른다). 비교가 아니면 None
    #[serde(default)]
    pub comparison: Option<ComparisonWindow>,
    /// 부분 부정으로 뺀 사건 종류 ("…는데 결제 페이지는 안 간", "but not the checkout", [`crate::negation`]). 포함 목록에는 없다
    #[serde(default)]
    pub excluded_event_types: Vec<String>,
    /// 부분 부정으로 뺀 페이지 종류
    #[serde(default)]
    pub excluded_page_kinds: Vec<String>,
    /// 부분 부정으로 뺀 행동
    #[serde(default)]
    pub excluded_action_kinds: Vec<String>,
}

/// 걸린 근거 하나: (갈래, 키, 시작 낱말, 걸린 글, 걸린 글자 수, 어떻게)
type RawHit = (Category, String, usize, String, usize, String);

/// 한자 · 가나 (띄어 쓰지 않는 글)
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F)
}

pub struct QueryParser {
    banks: Vec<(Category, DomainBank<String, SharedEmbedder>)>,
    /// 띄어 쓰지 않는 글(일본어 · 중국어)에서 찾을 정확 일치 글: (갈래, 키, 글). logis-center 의 `event_type_glued_keys`
    glued: Vec<(Category, String, String)>,
    /// 인사이트를 벡터로도 볼까 (신경망 임베더를 넘겼을 때만)
    semantic: bool,
    /// 벡터로 볼 때의 최소 점수와 2위와의 차이
    pub insight_min_score: f32,
    pub insight_margin: f32,
}

fn specs(entries: &BTreeMap<String, FilterEntry>) -> Vec<DomainSpec<String>> {
    entries
        .iter()
        .map(|(key, e)| {
            let mut bias: Vec<(String, f32)> = FilterEntry::phrases(&e.bias).into_iter().map(|p| (p, 1.0)).collect();
            if !e.semantic.trim().is_empty() {
                bias.push((e.semantic.clone(), 1.0));
            }
            DomainSpec { key: key.clone(), payload: key.clone(), bias, prejudice: FilterEntry::phrases(&e.prejudice), exact: e.exact() }
        })
        .collect()
}

impl QueryParser {
    /// 사전 일치만 하는 해석기 (모델 없음)
    pub fn lexical(dict: &AnalyticDictionary) -> QueryParser {
        Self::build(dict, SharedEmbedder::new(HashedNgramEmbedder::default()), false)
    }

    /// 신경망 임베더(예: granite)로 인사이트를 벡터로도 보는 해석기
    pub fn with_embedder(dict: &AnalyticDictionary, embedder: impl TextEmbedder + 'static) -> QueryParser {
        Self::build(dict, SharedEmbedder::new(embedder), true)
    }

    fn build(dict: &AnalyticDictionary, e: SharedEmbedder, semantic: bool) -> QueryParser {
        let cats = [
            (Category::Time, &dict.time_filters),
            (Category::Season, &dict.season_filters),
            (Category::Event, &dict.analytic_event_filters),
            (Category::Page, &dict.analytic_page_filters),
            (Category::Action, &dict.analytic_action_filters),
            (Category::Insight, &dict.insight),
        ];
        let mut glued: Vec<(Category, String, String)> = Vec::new();
        for (c, entries) in &cats {
            for (key, e) in entries.iter() {
                for p in e.exact() {
                    let p = p.trim().to_lowercase();
                    if p.chars().count() >= 2 && p.chars().all(is_cjk) {
                        glued.push((*c, key.clone(), p));
                    }
                }
            }
        }
        let banks = cats
            .into_iter()
            .map(|(c, entries)| {
                // 벡터 점수는 인사이트에서만 쓰므로, 나머지 갈래는 bias · prejudice 를 임베딩하지 않는다
                let mut s = specs(entries);
                if c != Category::Insight || !semantic {
                    for spec in &mut s {
                        spec.bias.clear();
                        spec.prejudice.clear();
                    }
                }
                (c, DomainBank::from_specs(s, &[], e.clone()))
            })
            .collect();
        QueryParser { banks, glued, semantic, insight_min_score: 0.5, insight_margin: 0.05 }
    }

    /// 질의를 해석한다. `lang` 은 사용자 언어(시간대 · 반구), `now_ms` 는 현재 시각
    pub fn parse(&self, text: &str, lang: &str, now_ms: i64) -> AnalyticQuery {
        let words = normalize(text).words;
        let mut hits: Vec<RawHit> = Vec::new();
        // 걸린 구간의 (끝 낱말 번호, 낱말 안에서 찾은 글인가): 부분 부정 판정용, hits 와 같은 순서
        let mut extents: Vec<(usize, bool)> = Vec::new();
        for (cat, bank) in &self.banks {
            for start in 0..words.len() {
                for len in (1..=MAX_SPAN_WORDS.min(words.len() - start)).rev() {
                    let span = &words[start..start + len];
                    if let Some((i, hit)) = bank.exact(span) {
                        let phrase = span.iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ");
                        let chars = phrase.chars().filter(|c| !c.is_whitespace()).count();
                        hits.push((*cat, bank.entry(i).key.clone(), start, phrase, chars, format!("{hit:?}").to_lowercase()));
                        extents.push((start + len, false));
                        break;
                    }
                }
            }
        }
        // 띄어 쓰지 않는 글: 낱말 안에서 한자 · 가나 글을 찾는다 (이미 정확히 걸린 갈래 · 키는 다시 넣지 않는다)
        for (i, w) in words.iter().enumerate() {
            let surface = w.surface.to_lowercase();
            if !surface.chars().any(is_cjk) {
                continue;
            }
            for (cat, key, p) in &self.glued {
                if surface.contains(p.as_str()) && !hits.iter().any(|h| h.0 == *cat && h.1 == *key && h.2 == i) {
                    hits.push((*cat, key.clone(), i, p.clone(), p.chars().count(), "glued".into()));
                    extents.push((i + 1, true));
                }
            }
        }
        let excluded = hit_polarities(text, lang, &words, &hits, &extents);
        let mut evidence: Vec<Evidence> = hits
            .iter()
            .map(|(c, k, _, p, _, h)| Evidence { category: *c, key: k.clone(), phrase: p.clone(), hit: h.clone() })
            .collect();
        // 한 갈래의 승자: 가장 긴 글, 같으면 앞의 것
        let single = |cat: Category| -> Option<String> {
            hits.iter()
                .filter(|h| h.0 == cat)
                .max_by(|a, b| a.4.cmp(&b.4).then(b.2.cmp(&a.2)))
                .map(|h| h.1.clone())
        };
        let many = |cat: Category| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for h in hits.iter().filter(|h| h.0 == cat) {
                if !out.contains(&h.1) {
                    out.push(h.1.clone());
                }
            }
            out
        };
        // 사건 · 페이지 · 행동은 부분 부정으로 포함과 제외를 가른다 (키마다 하나의 판정, [`hit_polarities`])
        let split = |cat: Category| -> (Vec<String>, Vec<String>) {
            let (mut inc, mut exc) = (Vec::new(), Vec::new());
            for key in many(cat) {
                if excluded.get(&(cat, key.clone())).copied().unwrap_or(false) {
                    exc.push(key);
                } else {
                    inc.push(key);
                }
            }
            (inc, exc)
        };
        let (event_types, excluded_event_types) = split(Category::Event);
        let (page_kinds, excluded_page_kinds) = split(Category::Page);
        let (action_kinds, excluded_action_kinds) = split(Category::Action);
        let time_key = single(Category::Time);
        let season_key = single(Category::Season);
        let mut insight = single(Category::Insight);
        if insight.is_none() && self.semantic {
            if let Some((_, bank)) = self.banks.iter().find(|(c, _)| *c == Category::Insight) {
                let scores = bank.score(text);
                if let Some(best) = scores.first() {
                    let second = scores.get(1).map_or(0.0, |s| s.score);
                    if best.score >= self.insight_min_score && best.score - second >= self.insight_margin {
                        let key = bank.entry(best.index).key.clone();
                        evidence.push(Evidence { category: Category::Insight, key: key.clone(), phrase: text.trim().to_string(), hit: "vector".into() });
                        insight = Some(key);
                    }
                }
            }
        }
        let range = time::resolve(time_key.as_deref().unwrap_or(""), season_key.as_deref().unwrap_or(""), lang, SeasonAnchor::Latest, now_ms);
        // 기간 비교: 기간이 둘 이상이면 처음 고른 것이 아닌 첫 기간과, 비교하는 말만 있으면 바로 앞 기간과 견준다 (계절만 물은 질의는
        // 견주지 않는다)
        let compare_key = many(Category::Time).into_iter().find(|k| Some(k) != time_key.as_ref());
        let comparison = if (compare_key.is_some() || asks_comparison(text)) && (time_key.is_some() || season_key.is_none()) {
            time::resolve_comparison_window(time_key.as_deref(), compare_key.as_deref(), lang, now_ms)
        } else {
            None
        };
        AnalyticQuery {
            text: text.to_string(),
            lang: time::lang_code_of(lang),
            time_key,
            season_key,
            range,
            event_types,
            page_kinds,
            action_kinds,
            insight,
            evidence,
            compare_key,
            comparison,
            excluded_event_types,
            excluded_page_kinds,
            excluded_action_kinds,
        }
    }
}

/// 걸린 사건 · 페이지 · 행동 키마다 제외인지: 주절의 마지막 판정, 주절에 없으면 마지막 판정. 부정 수는
/// [`crate::negation::span_polarities`](질의 언어의 표지)와, 낱말 구간이면 nms [`read_span`] 의 부정 수 중 큰 쪽이다
fn hit_polarities(text: &str, lang: &str, words: &[Word], hits: &[RawHit], extents: &[(usize, bool)]) -> BTreeMap<(Category, String), bool> {
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    // 낱말의 글자 번호 (text.to_lowercase() 기준, 앞에서부터 차례로 찾는다)
    let mut word_at: Vec<Option<(usize, usize)>> = Vec::with_capacity(words.len());
    let mut cursor = 0usize;
    for w in words {
        let s: Vec<char> = w.surface.to_lowercase().chars().collect();
        let found = if s.is_empty() || s.len() > lower.len() {
            None
        } else {
            (cursor..=lower.len() - s.len()).find(|&i| lower[i..i + s.len()] == s[..])
        };
        word_at.push(found.map(|i| (i, i + s.len())));
        if let Some(i) = found {
            cursor = i + s.len();
        }
    }
    let char_span = |h: &RawHit, end: usize, glued: bool| -> Option<(usize, usize)> {
        if glued {
            let (ws, we) = word_at.get(h.2).copied().flatten()?;
            let p: Vec<char> = h.3.to_lowercase().chars().collect();
            let last = we.checked_sub(p.len())?;
            let at = (ws..=last).find(|&i| lower[i..i + p.len()] == p[..])?;
            Some((at, at + p.len()))
        } else {
            let (s, _) = word_at.get(h.2).copied().flatten()?;
            let (_, e) = word_at.get(end.checked_sub(1)?).copied().flatten()?;
            Some((s, e))
        }
    };
    let picked: Vec<usize> = (0..hits.len()).filter(|&i| matches!(hits[i].0, Category::Event | Category::Page | Category::Action)).collect();
    let spans: Vec<Option<(usize, usize)>> = picked.iter().map(|&i| char_span(&hits[i], extents[i].0, extents[i].1)).collect();
    let located: Vec<(usize, usize)> = spans.iter().flatten().copied().collect();
    let mut pols = crate::negation::span_polarities(text, lang, &located).into_iter();
    let mut analyzed = Normalized { text: text.to_string(), words: words.to_vec(), addressed: false };
    RuleAnalyzer.analyze(&mut analyzed);
    // 키 → (제외인가, 주절의 판정인가)
    let mut verdict: BTreeMap<(Category, String), (bool, bool)> = BTreeMap::new();
    for (k, &i) in picked.iter().enumerate() {
        let pol = if spans[k].is_some() { pols.next() } else { None };
        let mut negations = pol.map_or(0, |p| p.negations);
        let main = pol.is_none_or(|p| p.main);
        if !extents[i].1 {
            negations = negations.max(read_span(&analyzed.words, hits[i].2, extents[i].0).negations);
        }
        let out = negations % 2 == 1;
        let entry = verdict.entry((hits[i].0, hits[i].1.clone())).or_insert((out, main));
        // 주절의 판정이 양보절의 것을 이기고, 같은 쪽이면 뒤의 것이 이긴다
        if main || !entry.1 {
            *entry = (out, main || entry.1);
        }
    }
    verdict.into_iter().map(|(key, (out, _))| (key, out)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-18 (수) 12:00 KST
    const NOW: i64 = 1_794_970_800_000;

    fn parser() -> QueryParser {
        QueryParser::lexical(AnalyticDictionary::builtin())
    }

    #[test]
    fn the_builtin_dictionary_has_every_analytic_section() {
        let d = AnalyticDictionary::builtin();
        assert_eq!(d.analytic_event_filters.len(), 5);
        assert_eq!(d.analytic_action_filters.len(), 4);
        assert_eq!(d.analytic_page_filters.len(), 10);
        assert_eq!(d.time_filters.len(), 9);
        assert_eq!(d.season_filters.len(), 4);
        assert_eq!(d.insight.keys().cloned().collect::<Vec<_>>(), ["conversion_insight", "general_insight", "traffic_insight"]);
        for k in d.time_filters.keys() {
            assert!(crate::time::TIME_KEYS.contains(&k.as_str()), "{k}");
        }
    }

    #[test]
    fn korean_checkout_question() {
        let q = parser().parse("이번 달 결제 이탈 원인이 뭐야?", "ko", NOW);
        assert_eq!(q.time_key.as_deref(), Some("this_month"));
        let r = q.range.unwrap();
        assert_eq!((r.start.to_string(), r.end.to_string()), ("2026-11-01".to_string(), "2026-11-30".to_string()));
        assert_eq!(r.offset_minutes, 540);
        // "결제 이탈" 이 "이탈" · "원인" 보다 길다
        assert_eq!(q.insight.as_deref(), Some("conversion_insight"), "{:?}", q.evidence);
    }

    #[test]
    fn pages_events_and_actions_come_out_together() {
        let q = parser().parse("지난주 장바구니 페이지에서 쿠폰 클릭한 사람", "ko", NOW);
        assert_eq!(q.time_key.as_deref(), Some("last_week"));
        assert_eq!(q.range.as_ref().map(|r| (r.start.to_string(), r.end.to_string())), Some(("2026-11-09".to_string(), "2026-11-15".to_string())));
        assert!(q.page_kinds.contains(&"cart".to_string()), "{:?}", q.evidence);
        assert!(q.event_types.contains(&"click".to_string()), "{:?}", q.evidence);
        assert!(q.action_kinds.contains(&"coupon".to_string()), "{:?}", q.evidence);
    }

    #[test]
    fn english_and_japanese() {
        let q = parser().parse("What drove the conversion rate down last month?", "en", NOW);
        assert_eq!(q.time_key.as_deref(), Some("last_month"));
        assert_eq!(q.insight.as_deref(), Some("conversion_insight"));
        let j = parser().parse("昨日のページビュー", "ja", NOW);
        assert_eq!(j.time_key.as_deref(), Some("yesterday"));
        assert_eq!(j.insight.as_deref(), Some("traffic_insight"), "{:?}", j.evidence);
    }

    #[test]
    fn seasons_use_the_latest_one_that_started() {
        // 11월에 "겨울" 은 아직 오지 않았다: 작년 겨울
        let q = parser().parse("겨울 시즌 이탈률", "ko", NOW);
        assert_eq!(q.season_key.as_deref(), Some("winter"));
        assert_eq!(q.range.unwrap().start.to_string(), "2025-12-01");
        // 기간 말이 없으면 전 구간
        let none = parser().parse("장바구니 페이지 클릭", "ko", NOW);
        assert!(none.range.is_none() && none.time_key.is_none());
    }

    #[test]
    fn two_periods_or_a_comparison_word_make_a_comparison_window() {
        // 2026-11-17 (화) 13:00 KST
        let tuesday = 1_794_841_200_000 + 13 * 3_600_000;
        let q = parser().parse("이번 주 vs 지난주 전환율 왜 떨어졌어?", "ko", tuesday);
        assert_eq!((q.time_key.as_deref(), q.compare_key.as_deref()), (Some("this_week"), Some("last_week")));
        let w = q.comparison.as_ref().unwrap();
        assert!(w.pace_matched);
        assert_eq!(w.label, "this_week (+37.0h) vs last_week (+37.0h)");
        // 예전 칸은 그대로: 그 주 전체
        assert_eq!(q.range.as_ref().map(|r| r.end.to_string()).as_deref(), Some("2026-11-22"));
        assert_eq!(q.insight.as_deref(), Some("conversion_insight"));
        // 영어 · "vs." (런던 시계: 수요일 03:00 UTC 는 이번 주 +51시간)
        let en = parser().parse("conversion this week vs. last week", "en", NOW);
        assert_eq!(en.comparison.map(|w| w.label).as_deref(), Some("this_week (+51.0h) vs last_week (+51.0h)"));
        // 기간이 하나여도 비교하는 말이 있으면 바로 앞 기간과
        assert_eq!(parser().parse("어제 대비 전환율", "ko", tuesday).comparison.map(|w| w.label).as_deref(), Some("yesterday vs day_before"));
        // 비교가 아니거나 계절만 물으면 없다
        assert!(parser().parse("지난주 장바구니 페이지에서 쿠폰 클릭한 사람", "ko", tuesday).comparison.is_none());
        assert!(parser().parse("겨울 대비 이탈률", "ko", tuesday).comparison.is_none());
        // 예전 모양(두 칸이 없는 JSON)도 읽는다
        let mut old = serde_json::to_value(&q).unwrap();
        old.as_object_mut().unwrap().retain(|k, _| k != "compare_key" && k != "comparison");
        let back: AnalyticQuery = serde_json::from_value(old).unwrap();
        assert_eq!((back.compare_key, back.comparison), (None, None));
    }

    #[test]
    fn modal_hrefs_take_the_page_kind_of_their_slug() {
        let d = AnalyticDictionary::builtin();
        assert_eq!(d.page_kind_of("https://shop.example.com/product/1#/__modal/cart").as_deref(), Some("cart"));
        assert_eq!(d.page_kind_of("https://shop.example.com/#/product/1/__modal/checkout").as_deref(), Some("checkout"));
        assert_eq!(d.page_kind_of("https://shop.example.com/product/1#/__modal/basket").as_deref(), Some("cart"));
        assert_eq!(d.page_kind_of("https://shop.example.com/product/1#/__modal/size-guide"), None, "an unknown modal is not the product page");
        assert_eq!(d.page_kind_of("https://shop.example.com/product/1").as_deref(), Some("goods_view"));
    }

    #[test]
    fn page_kinds_from_urls() {
        let d = AnalyticDictionary::builtin();
        assert_eq!(d.page_kind_of("https://shop.example.com/order/cart?x=1").as_deref(), Some("cart"));
        assert_eq!(d.page_kind_of("https://shop.example.com/").as_deref(), Some("home"));
        assert_eq!(d.page_kind_of("https://shop.example.com").as_deref(), Some("home"));
        assert_eq!(d.page_kind_of("https://shop.example.com/cartography"), None, "경로 조각 경계에서만 맞는다");
        assert!(d.page_kind_of("https://shop.example.com/goods/view/123").is_some());
        assert_eq!(d.action_types("add_to_cart"), ["click", "touch"]);
    }

    #[test]
    fn partial_negation_splits_included_and_excluded_pages() {
        let p = parser();
        let cases = [
            ("ko", "장바구니 페이지는 봤는데 결제 페이지는 안 간 기록"),
            ("en", "visits that reached the cart page but not the checkout"),
            ("de", "Besuche mit Warenkorbseite, aber ohne Kasse"),
            ("es", "visitas a la página del carrito pero no al pago"),
        ];
        for (lang, text) in cases {
            let q = p.parse(text, lang, NOW);
            assert_eq!(q.page_kinds, vec!["cart".to_string()], "{lang}: {q:?}");
            assert_eq!(q.excluded_page_kinds, vec!["checkout".to_string()], "{lang}: {q:?}");
        }
        // 부정이 없으면 예전과 같다: 제외 목록은 비고 둘 다 포함
        let q = p.parse("cart page and checkout visits", "en", NOW);
        assert_eq!((q.page_kinds.len(), q.excluded_page_kinds.len()), (2, 0), "{q:?}");
        // 이중 부정은 긍정
        let q = p.parse("not without the checkout", "en", NOW);
        assert_eq!((q.page_kinds.clone(), q.excluded_page_kinds.clone()), (vec!["checkout".to_string()], vec![]), "{q:?}");
        // 예전 JSON 은 제외 칸 없이도 읽힌다
        let mut v = serde_json::to_value(&q).unwrap();
        for k in ["excluded_event_types", "excluded_page_kinds", "excluded_action_kinds"] {
            v.as_object_mut().unwrap().remove(k);
        }
        let back: AnalyticQuery = serde_json::from_value(v).unwrap();
        assert!(back.excluded_page_kinds.is_empty());
    }
}
