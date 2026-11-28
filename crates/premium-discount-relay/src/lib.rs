//! # premium-discount-relay
//!
//! 같은 주제의 기록끼리 잇기: **Relay Key**(고유명사 · 식별자 · 기호 · 복합 명사 · 따옴표 이름)를 뽑아 다국어 표현을 한 키로
//! 묶고(말레니아 ↔ Malenia), 새 기록이 옛 기록을 **대체**하는지(같은 주장에서 수치 · 단위 · 형용사만 바뀜, "피해 20% → 15%")
//! 판정한다. raon-zena `docs/plans/relative-value-sds.md` 3.4절(신구 대체) · 4.1절(Relay Key)을 처음 구현한 것이다(2026-11-17).
//!
//! 입력은 모델과 상관없는 [`Token`] 문장이다. Stanza(premium-discount-nlp, 기능 `stanza`)의 분석은 [`Sentence::from_stanza`] 로
//! 바꾼다. 임베딩은 premium-discount-nms 의 [`TextEmbedder`](granite 또는 해시)다. 판정 결과의 [`Relation::Supersedes`] 는
//! PD-Verdict 의 `superseded` 입력(옛 기록은 discount)으로 이어진다.
//!
//! **임계값은 SDS 띠다.** 고정 숫자 대신 이번 비교의 코사인 분포에서 `평균 + z·표준편차`(상한 띠)를 구하고, 표본이 모자라거나
//! 띠가 너무 낮으면 바닥값을 쓴다 ([`RelayConfig`]). 바닥값은 granite 실측으로 정했다(README). 해시 임베더(`hash-ngram-v1`)는
//! 술어 하나만 다른 짧은 문장끼리의 코사인이 낮아 문장 맞추기 바닥을 0.50 으로 낮춘다 ([`RelayConfig::effective_align_floor`]).
//!
//! 주장 견주기의 두 가드 (2026-11, 시나리오 3 Part 3):
//!
//! * **배타 조건 가드**(ConditionDisjointGuard): 서수 구간 · 모드 · 인원 · 난이도 · 플랫폼("1페이즈", "PvP", "하드 모드", "모바일")은
//!   값이 아니라 조건 한정자([`Claim::qualifiers`])다. 두 문장의 조건이 서로 다르면("1페이즈 20%" · "2페이즈 15%") 둘 다 맞는
//!   말이라 대체도 중복도 아닌 의미 연결이다.
//! * **부정 반전**(PolarityFlip): 서술 핵에 붙은 부정("회피 가능" → "회피 불가", "can be dodged" → "can not be dodged")이 바뀌면
//!   값이 없어도 바뀐 주장이다 ([`ChangeKind::PolarityFlip`]). 예전에는 중복으로 합쳐졌다.
//!
//! 서버 보고용 [`supersessions`] 는 서버 `POST /evaluations/supersedes` 처럼 같은 장소 · 대상(`cc` · `to`)끼리만 견준다. RAG
//! 후보를 고르는 [`curate_cross_scope`] 는 같은 `cc` 안에서 `to` 가 달라도 대표 Relay Key 가 겹치면 견주고, 서버에 알릴지는
//! [`supersede_sync`] 가 가린다(같은 `to` · 내 글 · 로그인 · 둘 다 stored). 대체된 남의 옛 글은 [`QuarantineLedger`] 에 로컬로만
//! 격리하고, 대체한 새 글이 무너지면(전역 discount 등) 스스로 복권한다.

use std::collections::{BTreeMap, BTreeSet};

use premium_discount_nms::embed::{cosine, TextEmbedder};
use premium_discount_nms::korean::{hangul_tail_agrees, NOMINAL_TAILS};
use premium_discount_sds::{matrix, Moments};

/// Stanza 낱말 하나 (UD 표기)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Token {
    pub text: String,
    pub lemma: String,
    /// UPOS (NOUN · PROPN · NUM · ADJ · SYM · X · VERB …)
    pub upos: String,
    /// 의존 관계 (nsubj · obj · root · compound · flat …)
    pub deprel: String,
    /// 머리 낱말 번호 (1부터, 0 은 ROOT)
    pub head: usize,
}

/// 문장 하나: 원문과 낱말들
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sentence {
    pub text: String,
    pub tokens: Vec<Token>,
}

impl Sentence {
    /// Stanza 분석 결과에서 (premium-discount-nlp)
    #[cfg(feature = "stanza")]
    pub fn from_stanza(s: &premium_discount_nlp::stanza::SentenceAnalysis) -> Sentence {
        Sentence {
            text: s.tokens.join(" "),
            tokens: s
                .words
                .iter()
                .map(|w| Token {
                    text: w.text.clone(),
                    lemma: w.lemma.clone(),
                    upos: w.upos.clone(),
                    deprel: w.deprel.clone(),
                    head: w.head,
                })
                .collect(),
        }
    }
}

/// 기록 하나: 문장들과 쓴 시각
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    pub sentences: Vec<Sentence>,
    pub at_ms: i64,
}

/// Relay Key 의 출처
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyKind {
    /// 따옴표 안 이름 (그대로)
    Quoted,
    /// 고유명사 (PROPN)
    Proper,
    /// 글자와 숫자가 섞인 식별자 (`E0425`, `RTX3050`, `v1.4`)
    Identifier,
    /// 기호 · 외래 토큰 (X · SYM, 예: `wgpu`, `SurfaceError::Outdated`)
    Symbol,
    /// 복합 명사 (`compound` · `flat` 으로 이어진 명사 덩어리)
    Compound,
    /// 기술 기호 ([`TechnicalToken`]): 문장 키는 그 계열(`-O2` → `-O`), 체크포인트 키는 정규 꼴 그대로
    TechnicalToken,
}

/// 키 조합의 수식 키 (대소문자 무시)
const CHORD_MODIFIERS: &[&str] = &["ctrl", "control", "ctl", "alt", "option", "opt", "shift", "cmd", "command", "meta", "win", "super", "fn"];

/// 통째로 지키는 기술 기호 하나: 키 조합(`Ctrl+Shift+B`), 명령줄 옵션(`-O2`, `-j`, `--release`, `--jobs=4`), 거듭제곱(`10^4`),
/// 표의 열 · 행 참조(`C 열`, `row 3`). 2글자 미만 조각을 버리기 전에 먼저 박제한다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TechnicalToken {
    /// 정규 꼴: 키 조합은 소문자(`ctrl+shift+b`), 옵션은 쓴 그대로(`-O2` 와 `-o2` 는 뜻이 다르다), 열 · 행은 `col:C` · `row:3`
    pub canonical: String,
    /// 무엇을 정하는가 (단계 · 인자를 뺀 꼴): `-O2` → `-O`, `--jobs=4` → `--jobs`, 키 조합 → `chord`, `10^4` → `10^`,
    /// `C 열` → `col`. 같은 것을 정하는 두 글은 이 키를 나누고, 정규 꼴은 값으로 견준다
    pub family: String,
}

/// 낱말 앞뒤 문장 부호를 떼고, 한글 조사 등 ASCII 가 아닌 꼬리 앞까지 (`-O2로` → `-O2`)
fn ascii_core(word: &str) -> &str {
    let w = word.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`'));
    let end = w.char_indices().find(|(_, c)| !c.is_ascii()).map_or(w.len(), |(i, _)| i);
    w[..end].trim_end_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''))
}

/// 낱말 하나가 기술 기호이면 그 꼴 (열 · 행 참조는 두 낱말이라 [`technical_tokens`] 가 본다)
pub fn technical_token(word: &str) -> Option<TechnicalToken> {
    let w = ascii_core(word);
    if w.is_empty() || w.len() > 48 {
        return None;
    }
    let name_ok = |n: &str| n.starts_with(|c: char| c.is_ascii_alphabetic()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if let Some(body) = w.strip_prefix("--") {
        let name = body.split_once('=').map_or(body, |(n, _)| n);
        return (name_ok(name) && name.len() <= 32).then(|| TechnicalToken { canonical: w.to_string(), family: format!("--{name}") });
    }
    if let Some(body) = w.strip_prefix('-') {
        let name = body.split_once('=').map_or(body, |(n, _)| n);
        if !name_ok(name) || name.len() > 16 || name.contains('-') {
            return None;
        }
        let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
        return Some(TechnicalToken { canonical: w.to_string(), family: format!("-{stem}") });
    }
    if w.contains('+') {
        let parts: Vec<&str> = w.split('+').collect();
        let (key, mods) = parts.split_last()?;
        let chord = !mods.is_empty()
            && mods.iter().all(|m| CHORD_MODIFIERS.contains(&m.to_ascii_lowercase().as_str()))
            && (1..=12).contains(&key.len())
            && key.chars().all(|c| c.is_ascii_alphanumeric());
        return chord.then(|| TechnicalToken { canonical: w.to_ascii_lowercase(), family: "chord".into() });
    }
    if let Some((base, exp)) = w.split_once('^') {
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        let exp_ok = digits(exp.strip_prefix('-').unwrap_or(exp));
        return (digits(base) && exp_ok).then(|| TechnicalToken { canonical: w.to_string(), family: format!("{base}^") });
    }
    None
}

/// 글 속의 기술 기호 (나온 순서, 중복 없이): 낱말마다 [`technical_token`], 그리고 두 낱말짜리 열 · 행 참조
/// (`C 열` · `C열` · `3 행` · `col C` · `column B` · `row 3`)
pub fn technical_tokens(text: &str) -> Vec<TechnicalToken> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let label = |s: &str| -> Option<String> {
        let s = ascii_core(s);
        let letters = (1..=3).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphabetic());
        let digits = (1..=4).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
        (letters || digits).then(|| s.to_ascii_uppercase())
    };
    // 한국어 "열" · "행" (조사가 붙어도: "열의", "행에")
    let ko_axis = |s: &str| -> Option<&'static str> {
        match premium_discount_nms::korean::noun_lemma(s.trim_matches(|c: char| c.is_ascii_punctuation())).as_str() {
            "열" => Some("col"),
            "행" => Some("row"),
            _ => None,
        }
    };
    let mut out: Vec<TechnicalToken> = Vec::new();
    let mut push = |t: TechnicalToken| {
        if !out.contains(&t) {
            out.push(t);
        }
    };
    for (i, w) in words.iter().enumerate() {
        if let Some(t) = technical_token(w) {
            push(t);
            continue;
        }
        let axis_ref = |axis: &str, x: String| TechnicalToken { canonical: format!("{axis}:{x}"), family: axis.to_string() };
        // "C열" (붙여 씀)
        let head = ascii_core(w);
        if !head.is_empty() && w.starts_with(head) && head.len() < w.len() {
            if let (Some(x), Some(axis)) = (label(head), ko_axis(&w[head.len()..])) {
                push(axis_ref(axis, x));
                continue;
            }
        }
        if let Some(next) = words.get(i + 1) {
            if let (Some(x), Some(axis)) = (label(w), ko_axis(next)) {
                push(axis_ref(axis, x));
                continue;
            }
            let en = match ascii_core(w).to_ascii_lowercase().as_str() {
                "col" | "column" => Some("col"),
                "row" => Some("row"),
                _ => None,
            };
            // 영어 꼴은 대문자 · 숫자 이름만 ("row 3", "column B"; "a row of" 는 아님)
            let named = ascii_core(next);
            let strict = !named.is_empty() && (named.chars().all(|c| c.is_ascii_uppercase()) || named.chars().all(|c| c.is_ascii_digit()));
            if let (Some(axis), Some(x), true) = (en, label(next), strict) {
                push(axis_ref(axis, x));
            }
        }
    }
    out
}

/// 한 작업에서 자주 짝을 이루는 창 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkSurface {
    Terminal,
    Ide,
    Browser,
    Docs,
    Other,
}

/// 창을 바꾼 뒤 대명사가 앞 창의 대화를 가리킬 수 있는 시간 (밀리초)
pub const ANAPHORA_BRIDGE_MS: u64 = 15_000;

/// 창을 바꾼 직후의 대명사("그 에러", "이거")가 앞 창 대화의 주제를 가리켜도 되는가. 짝 전환(터미널 ⇄ IDE, 브라우저 ⇄
/// 문서)이고, 바꾼 지 [`ANAPHORA_BRIDGE_MS`] 안이고, 두 창이 정밀한 키(식별자 · 기호 · 따옴표 이름 · 기술 기호 — 오류 코드나
/// 파일 이름)를 하나 이상 나눌 때만 참이다. 그 밖에는 무관한 창 전환의 단절(토픽 가중치 0.0)을 그대로 지킨다.
pub fn anaphora_bridge_allowed(from: WorkSurface, to: WorkSurface, since_switch_ms: u64, shared: &[RelayKey]) -> bool {
    use WorkSurface::*;
    let paired = matches!((from, to), (Terminal, Ide) | (Ide, Terminal) | (Browser, Docs) | (Docs, Browser));
    paired
        && since_switch_ms <= ANAPHORA_BRIDGE_MS
        && shared
            .iter()
            .any(|k| matches!(k.kind, KeyKind::Identifier | KeyKind::Symbol | KeyKind::Quoted | KeyKind::TechnicalToken))
}

/// 문서 사이 사건 간선의 신뢰 등급
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeTrust {
    /// 원본과 일치하거나, 광학 열화(스캔 · 촬영) 판독이 그림자 판독과 교차 확인됨: 사건이 성립하고 정합성도 인정한다
    Certified,
    /// 디지털 원본에서 검사 숫자 · 승인 번호가 어긋남: 사건을 따라가는 데만 쓰고, 자동 승인의 근거로는 쓰지 않는다
    FaultTracingOnly,
}

impl EdgeTrust {
    /// 이 간선을 근거로 자동 승인해도 되는가
    pub fn may_auto_approve(self) -> bool {
        self == EdgeTrust::Certified
    }
}

/// 사건 간선의 역할
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRole {
    /// 한 사건의 서류들을 한 봉투로 합치고(union), 반려되면 다시 나눈다(de-union)
    CaseBinding,
    /// 여러 사건이 함께 쓰는 운반자(용기 번호 · 설비 코드 · 공용 계좌 …): 거꾸로 찾아가는 데만 지나고, 다른 사건을 합치지 않는다
    CarrierTraversal,
}

impl EdgeRole {
    /// 이 간선이 두 끝을 한 사건으로 합치는가
    pub fn merges_cases(self) -> bool {
        self == EdgeRole::CaseBinding
    }
}

/// 사건 간선 하나: 두 끝의 64비트 개체 키 ([`entity_key_u64`]), 역할, 신뢰. 방향을 뒤집어도 같은 간선이다
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CaseEdge {
    pub from: u64,
    pub to: u64,
    pub role: EdgeRole,
    pub trust: EdgeTrust,
}

impl CaseEdge {
    pub fn reversed(self) -> CaseEdge {
        CaseEdge { from: self.to, to: self.from, ..self }
    }

    /// 방향 없는 비교용 꼴 (작은 키가 앞)
    pub fn undirected(self) -> CaseEdge {
        if self.from <= self.to {
            self
        } else {
            self.reversed()
        }
    }
}

/// 개체 키의 64비트 해시: 앞뒤 공백을 떼고 소문자로 바꾼 키의 FNV-1a 64. 여러 문서 묶음이 한 결속 엔진을 써도 충돌이 드물다
pub fn entity_key_u64(key: &str) -> u64 {
    premium_discount_nms::embed::fnv1a_64(&lower(key))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelayKey {
    /// 정규화한 키 (원형 · 소문자, 따옴표 이름은 그대로)
    pub key: String,
    pub kind: KeyKind,
}

fn lower(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 원형 (없으면 글자). 한국어 Stanza 원형은 형태소를 `+` 로 잇는다(`말레니아+의`, `감소+하+었+다`): 첫 형태소(어간 · 명사)만 쓴다.
fn lemma_or_text(t: &Token) -> String {
    let l = if t.lemma.trim().is_empty() || t.lemma == "_" { &t.text } else { &t.lemma };
    let l = if l.contains('+') && l.len() > 1 { l.split('+').next().unwrap_or(l) } else { l.as_str() };
    lower(l)
}

/// 의존 관계 이름의 앞부분 ("nsubj:pass" → "nsubj")
fn rel_of(t: &Token) -> &str {
    t.deprel.split(':').next().unwrap_or("")
}

/// 따옴표(" " · “ ” · ' ' · ‘ ’ · 「 」 · 『 』) 안의 글
pub fn quoted_spans(text: &str) -> Vec<String> {
    const PAIRS: [(char, char); 6] = [('"', '"'), ('\u{201C}', '\u{201D}'), ('\'', '\''), ('\u{2018}', '\u{2019}'), ('\u{300C}', '\u{300D}'), ('\u{300E}', '\u{300F}')];
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(&(_, close)) = PAIRS.iter().find(|(o, _)| *o == chars[i]) {
            if let Some(j) = chars[i + 1..].iter().position(|c| *c == close) {
                let inner: String = chars[i + 1..i + 1 + j].iter().collect();
                if !inner.trim().is_empty() && inner.chars().count() <= 64 {
                    out.push(inner.trim().to_string());
                }
                i += j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 글자와 숫자가 섞인 식별자 (`E0425`, `RTX3050`, `v1.4`). 숫자로 시작해 단위가 붙은 값(`3,000원`, `20kg`, 한국어 서술어
/// `3,000원이다`)은 식별자가 아니라 값이다 ([`numeric_value`]).
fn is_identifier(text: &str) -> bool {
    let letters = text.chars().any(|c| c.is_alphabetic());
    let digits = text.chars().any(|c| c.is_ascii_digit());
    letters && digits && text.chars().count() >= 2 && !text.starts_with(|c: char| c.is_ascii_digit())
}

/// 숫자로 시작하는 낱말의 값: 수(`0-9 , .`)와 바로 붙은 단위. 한국어 Stanza 원형은 형태소를 `+` 로 잇으므로(`3,000+원+이+다`)
/// 원형이 있으면 수 형태소와 다음 형태소(단위)를, 없으면 글자에서 수 뒤의 글자들을 단위로 붙인다. 숫자로 시작하지 않으면 None.
fn numeric_value(t: &Token) -> Option<String> {
    let surface = t.text.trim();
    if !surface.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let is_number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.');
    if t.lemma.contains('+') {
        let parts: Vec<&str> = t.lemma.split('+').collect();
        if is_number(parts[0]) {
            let unit = parts.get(1).filter(|u| u.chars().all(char::is_alphabetic) || **u == "%").copied().unwrap_or("");
            return Some(format!("{}{}", parts[0], unit));
        }
    }
    Some(surface.to_string())
}

/// 한국어 계사 서술어(`무료+이+다`, `3,000+원+이+다`)의 명사 부분. 주어(주제어)에 대한 값이다.
fn copula_stem(t: &Token) -> Option<String> {
    let parts: Vec<&str> = t.lemma.split('+').collect();
    match parts.as_slice() {
        [stem, "이", "다"] | [stem, "이", "었", "다"] if !stem.is_empty() => Some(lower(stem)),
        _ => None,
    }
}

/// 문장의 Relay Key (4.1절): 따옴표 이름, 고유명사, 글자 · 숫자 식별자, X · SYM 토큰, 복합 명사 덩어리. 중복은 하나로.
pub fn relay_keys(s: &Sentence) -> Vec<RelayKey> {
    let mut out: BTreeSet<RelayKey> = BTreeSet::new();
    for q in quoted_spans(&s.text) {
        out.insert(RelayKey { key: q, kind: KeyKind::Quoted });
    }
    // 두 낱말짜리 열 · 행 참조 ("C 열"): 낱말 하나씩으로는 보이지 않는다
    for tt in technical_tokens(&s.text).into_iter().filter(|t| t.family == "col" || t.family == "row") {
        out.insert(RelayKey { key: tt.family, kind: KeyKind::TechnicalToken });
    }
    for t in &s.tokens {
        let surface = t.text.trim();
        if surface.is_empty() {
            continue;
        }
        // 기술 기호는 계열을 주제 키로 (정규 꼴은 주장의 값: `-O2` → `-O3` 가 대체로 잡힌다)
        if let Some(tt) = technical_token(surface) {
            out.insert(RelayKey { key: tt.family, kind: KeyKind::TechnicalToken });
            continue;
        }
        if t.upos == "PROPN" {
            out.insert(RelayKey { key: lemma_or_text(t), kind: KeyKind::Proper });
        } else if is_identifier(surface) {
            out.insert(RelayKey { key: lower(surface), kind: KeyKind::Identifier });
        } else if matches!(t.upos.as_str(), "X" | "SYM") && surface.chars().any(|c| c.is_alphabetic()) {
            out.insert(RelayKey { key: lower(surface), kind: KeyKind::Symbol });
        }
    }
    // 복합 명사: compound · flat 으로 머리 명사에 붙은 낱말들 + 머리 (문장 순서대로)
    for (h, head) in s.tokens.iter().enumerate() {
        if !matches!(head.upos.as_str(), "NOUN" | "PROPN") {
            continue;
        }
        let mut parts: Vec<(usize, String)> = s
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.head == h + 1 && (t.deprel.starts_with("compound") || t.deprel.starts_with("flat")))
            .map(|(i, t)| (i, lemma_or_text(t)))
            .collect();
        if parts.is_empty() {
            continue;
        }
        parts.push((h, lemma_or_text(head)));
        parts.sort_by_key(|(i, _)| *i);
        let key = parts.into_iter().map(|(_, p)| p).collect::<Vec<_>>().join(" ");
        out.insert(RelayKey { key, kind: KeyKind::Compound });
    }
    out.into_iter().collect()
}

/// 해시 임베더의 공간 이름 (premium-discount-nms `TextEmbedder::space` 기본값)
pub const HASH_SPACE: &str = "hash-ngram-v1";

/// 임계값 · 바닥값. 띠는 이번 비교의 코사인 분포에서 `평균 + z·표준편차`, 바닥값보다 낮으면 바닥값.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelayConfig {
    /// 키 묶기의 코사인 바닥 (`relay.cos`)
    pub key_cos_floor: f32,
    /// 키 묶기: 서로 가장 가까운 짝이 다음 후보보다 앞서야 하는 코사인
    pub key_margin: f32,
    /// 문장 맞추기의 코사인 바닥 (`align.cos`)
    pub align_cos_floor: f32,
    /// 해시 임베더(`hash-ngram-v1`)일 때 문장 맞추기 바닥의 상한: 해시 코사인은 술어 하나만 다른 짧은 문장끼리 granite 보다
    /// 낮다(배송비 두 문장: 해시 0.633, granite 0.955). granite 등 다른 공간은 `align_cos_floor` 그대로.
    pub hash_align_cos_floor: f32,
    /// 상한 띠의 z
    pub band_z: f32,
    /// 띠를 쓰는 최소 표본 수 (그보다 적으면 바닥값)
    pub band_min_samples: u64,
    /// 문장 맞추기에서 같은 줄의 2등보다 앞서야 하는 이중 센터링 잔차
    pub align_margin: f32,
    /// 대체 · 중복으로 보는 Relay Key 포함률 하한 `|K_O ∩ K_N| / |K_O|`
    pub containment: f32,
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            key_cos_floor: 0.80,
            key_margin: 0.03,
            align_cos_floor: 0.75,
            hash_align_cos_floor: 0.50,
            band_z: 2.0,
            band_min_samples: 6,
            align_margin: 0.0,
            containment: 0.5,
        }
    }
}

impl RelayConfig {
    /// 임베더 공간에 맞춘 문장 맞추기 바닥: 해시 공간이면 `min(align_cos_floor, hash_align_cos_floor)`, 아니면 `align_cos_floor`
    pub fn effective_align_floor(&self, space: &str) -> f32 {
        if space == HASH_SPACE {
            self.align_cos_floor.min(self.hash_align_cos_floor)
        } else {
            self.align_cos_floor
        }
    }
}

/// 값들의 SDS 상한 띠 (표본이 모자라면 `floor`)
fn upper_band(values: &[f32], floor: f32, cfg: &RelayConfig) -> f32 {
    let m = Moments::of(&values.iter().map(|v| *v as f64).collect::<Vec<_>>());
    if m.n < cfg.band_min_samples {
        return floor;
    }
    ((m.mean + cfg.band_z as f64 * m.sample_sd()) as f32).max(floor)
}

/// 다국어 키 묶기 결과: 키 → 대표 키
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyClusters {
    pub canonical: BTreeMap<String, String>,
    /// 이번에 쓴 코사인 임계값
    pub threshold: f32,
}

impl KeyClusters {
    /// 대표 키 (모르는 키는 그대로)
    pub fn canon(&self, key: &str) -> String {
        self.canonical.get(key).cloned().unwrap_or_else(|| key.to_string())
    }
}

/// 키들을 임베딩 코사인으로 묶는다: 두 키가 **서로 가장 가까운 짝**이고, 코사인이 바닥(`key_cos_floor`) 이상이며 각자의 다음
/// 후보보다 `key_margin` 이상 앞서면 같은 키. 띠(평균 + z·표준편차)는 쓰지 않는다: 키 몇 개의 짧은 이름끼리는 다른 이름도
/// 코사인이 높게 몰려(granite 실측: 말레니아 ↔ malenia 0.833, 라다곤 ↔ radagon 0.848, 다른 이름끼리 0.739 ~ 0.771) 작은 표본의
/// 띠가 같은 이름 짝까지 막는다(0.881). 대표는 ASCII 키가 있으면 그중
/// 사전순 첫째(언어가 달라도 같은 대표가 되도록), 없으면 사전순 첫째. 따옴표 이름 · 식별자는 글자가 같을 때만 묶는다
/// (다른 오류 코드가 닮았다고 하나가 되면 안 된다).
pub fn cluster_keys(keys: &[RelayKey], embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> KeyClusters {
    let uniq: Vec<&RelayKey> = {
        let mut seen = BTreeSet::new();
        keys.iter().filter(|k| seen.insert(k.key.clone())).collect()
    };
    let soft: Vec<usize> = (0..uniq.len()).filter(|&i| matches!(uniq[i].kind, KeyKind::Proper | KeyKind::Compound | KeyKind::Symbol)).collect();
    let texts: Vec<&str> = soft.iter().map(|&i| uniq[i].key.as_str()).collect();
    let vecs = embedder.embed_batch(&texts);
    let mut pairs = Vec::new();
    for a in 0..soft.len() {
        for b in a + 1..soft.len() {
            pairs.push((a, b, cosine(&vecs[a], &vecs[b])));
        }
    }
    let threshold = cfg.key_cos_floor;
    let n = soft.len();
    let mut sim = vec![vec![f32::NEG_INFINITY; n]; n];
    for (a, b, c) in &pairs {
        sim[*a][*b] = *c;
        sim[*b][*a] = *c;
    }
    // 각 키의 가장 가까운 짝과 다음 후보와의 차이
    let best: Vec<Option<(usize, f32)>> = (0..n)
        .map(|a| {
            let mut order: Vec<(usize, f32)> = (0..n).filter(|&b| b != a).map(|b| (b, sim[a][b])).collect();
            order.sort_by(|x, y| y.1.total_cmp(&x.1));
            let first = *order.first()?;
            let second = order.get(1).map(|x| x.1).unwrap_or(f32::NEG_INFINITY);
            (first.1 - second >= cfg.key_margin).then_some(first)
        })
        .collect();
    // 합집합 찾기
    let mut parent: Vec<usize> = (0..uniq.len()).collect();
    fn root(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    for a in 0..n {
        let Some((b, c)) = best[a] else { continue };
        let mutual = matches!(best[b], Some((x, _)) if x == a);
        if mutual && c >= threshold {
            let (ra, rb) = (root(&mut parent, soft[a]), root(&mut parent, soft[b]));
            if ra != rb {
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for i in 0..uniq.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(uniq[i].key.as_str());
    }
    let mut canonical = BTreeMap::new();
    for members in groups.values() {
        let rep = members
            .iter()
            .filter(|m| m.is_ascii())
            .min()
            .or_else(|| members.iter().min())
            .map(|s| s.to_string())
            .unwrap_or_default();
        for m in members {
            canonical.insert(m.to_string(), rep.clone());
        }
    }
    KeyClusters { canonical, threshold }
}

/// 서수 구간 낱말 → 차원 ("2페이즈" · "2페" · "phase 2" → `phase:2`, "1막" → `act:1`)
const SECTIONS: &[(&str, &str)] = &[
    ("페이즈", "phase"),
    ("페", "phase"),
    ("phase", "phase"),
    ("막", "act"),
    ("act", "act"),
    ("단계", "stage"),
    ("스테이지", "stage"),
    ("관문", "stage"),
    ("stage", "stage"),
    ("라운드", "round"),
    ("round", "round"),
    ("웨이브", "wave"),
    ("wave", "wave"),
    ("챕터", "chapter"),
    ("chapter", "chapter"),
];
/// 서수 낱말 ("첫 페이즈", "second phase", "막페")
const ORDINALS: &[(&str, &str)] = &[
    ("첫", "1"),
    ("첫번째", "1"),
    ("첫째", "1"),
    ("first", "1"),
    ("1st", "1"),
    ("두번째", "2"),
    ("둘째", "2"),
    ("second", "2"),
    ("2nd", "2"),
    ("세번째", "3"),
    ("셋째", "3"),
    ("third", "3"),
    ("3rd", "3"),
    ("마지막", "last"),
    ("막", "last"),
    ("final", "last"),
    ("last", "last"),
];
/// 혼자서도 조건인 낱말: 모드 · 인원 · 난이도(헷갈리지 않는 것) · 플랫폼
const CONDITIONS: &[(&str, &str)] = &[
    ("pve", "mode:pve"),
    ("pvp", "mode:pvp"),
    ("솔로", "party:solo"),
    ("solo", "party:solo"),
    ("멀티", "party:multi"),
    ("multi", "party:multi"),
    ("multiplayer", "party:multi"),
    ("코옵", "party:multi"),
    ("coop", "party:multi"),
    ("협동", "party:multi"),
    ("하드", "difficulty:hard"),
    ("노말", "difficulty:normal"),
    ("헤로익", "difficulty:heroic"),
    ("나이트메어", "difficulty:nightmare"),
    ("하드코어", "difficulty:hardcore"),
    ("hardcore", "difficulty:hardcore"),
    ("pc", "platform:pc"),
    ("모바일", "platform:mobile"),
    ("mobile", "platform:mobile"),
    ("콘솔", "platform:console"),
    ("console", "platform:console"),
    ("ps4", "platform:ps4"),
    ("ps5", "platform:ps5"),
    ("플스", "platform:playstation"),
    ("playstation", "platform:playstation"),
    ("xbox", "platform:xbox"),
    ("엑박", "platform:xbox"),
];
/// 뒤에 "난이도 · 모드" 가 붙을 때만 난이도인 낱말 ("일반 공격", "hard to dodge" 는 난이도가 아니다)
const MARKED_DIFFICULTY: &[(&str, &str)] = &[
    ("일반", "normal"),
    ("보통", "normal"),
    ("normal", "normal"),
    ("쉬움", "easy"),
    ("이지", "easy"),
    ("easy", "easy"),
    ("어려움", "hard"),
    ("hard", "hard"),
    ("영웅", "heroic"),
    ("heroic", "heroic"),
    ("신화", "mythic"),
    ("mythic", "mythic"),
    ("지옥", "hell"),
    ("헬", "hell"),
    ("hell", "hell"),
    ("악몽", "nightmare"),
    ("nightmare", "nightmare"),
];
const DIFFICULTY_MARKERS: &[&str] = &["난이도", "모드", "mode", "difficulty"];

/// 낱말과, 조사를 한 겹 · 두 겹 뗀 꼴 ("솔로로" → 솔로로 · 솔로 · 솔). 맨 앞이 쓴 그대로(소문자)다.
fn peel_forms(t: &Token) -> Vec<String> {
    let mut cur = lower(&t.text).trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let mut out = vec![cur.clone()];
    let mut tails: Vec<&str> = NOMINAL_TAILS.to_vec();
    tails.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    for _ in 0..2 {
        let next = tails.iter().find_map(|tl| cur.strip_suffix(tl).filter(|s| !s.is_empty() && hangul_tail_agrees(s, tl)).map(str::to_string));
        let Some(stem) = next else { break };
        cur = stem;
        out.push(cur.clone());
    }
    out
}

fn table<'t>(rows: &'t [(&'t str, &'t str)], word: &str) -> Option<&'t str> {
    rows.iter().find(|(k, _)| *k == word).map(|(_, v)| *v)
}

fn number_text(n: &str) -> String {
    let t = n.trim_start_matches('0');
    if t.is_empty() {
        "0".into()
    } else {
        t.into()
    }
}

fn all_digits(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())
}

/// 한 낱말 안의 서수 구간: "2페이즈", "1막", "phase2", "막페", "첫페이즈"
fn fused_section(word: &str) -> Option<String> {
    let lead: String = word.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !lead.is_empty() {
        let dim = table(SECTIONS, &word[lead.len()..])?;
        return Some(format!("{dim}:{}", number_text(&lead)));
    }
    let trail: String = word.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    if !trail.is_empty() {
        let dim = table(SECTIONS, &word[..word.len() - trail.len()])?;
        return Some(format!("{dim}:{}", number_text(&trail)));
    }
    match word {
        "막페" | "막페이즈" => Some("phase:last".into()),
        "첫페" | "첫페이즈" => Some("phase:1".into()),
        _ => None,
    }
}

/// 한 낱말로 된 난이도 ("하드", "하드모드", "영웅난이도", "hardmode")
fn fused_difficulty(word: &str) -> Option<String> {
    for m in DIFFICULTY_MARKERS {
        if let Some(stem) = word.strip_suffix(m).filter(|s| !s.is_empty()) {
            if let Some(d) = table(MARKED_DIFFICULTY, stem) {
                return Some(format!("difficulty:{d}"));
            }
            if let Some(c) = table(CONDITIONS, stem).filter(|c| c.starts_with("difficulty:")) {
                return Some(c.to_string());
            }
        }
    }
    None
}

/// 배타 조건 한정자와 그 낱말들 (ConditionDisjointGuard): 서수 구간("1페이즈", "phase 2", "두번째 페이즈"), 모드(PvE · PvP),
/// 인원(솔로 · 멀티), 난이도("하드", "노말 모드"; "일반 · hard" 는 "난이도 · 모드" 가 뒤에 붙을 때만), 플랫폼(PC · 모바일 · 콘솔).
/// 조사는 떼고 본다("2페이즈에서", "솔로로"). 이 낱말들은 값도 핵심도 아니다.
pub fn condition_qualifiers(s: &Sentence) -> (BTreeSet<String>, BTreeSet<usize>) {
    let forms: Vec<Vec<String>> = s.tokens.iter().map(peel_forms).collect();
    let mut quals = BTreeSet::new();
    let mut used = BTreeSet::new();
    let section = |i: usize| forms.get(i).and_then(|fs| fs.iter().find_map(|f| table(SECTIONS, f)));
    let number = |i: usize| forms.get(i).and_then(|fs| fs.first()).filter(|f| all_digits(f)).map(|f| number_text(f));
    let marker = |i: usize| forms.get(i).is_some_and(|fs| fs.iter().any(|f| DIFFICULTY_MARKERS.contains(&f.as_str())));
    for (i, fs) in forms.iter().enumerate() {
        if used.contains(&i) {
            continue;
        }
        if let Some(q) = fs.iter().find_map(|f| fused_section(f)) {
            quals.insert(q);
            used.insert(i);
        } else if let (Some(n), Some(dim)) = (number(i), section(i + 1)) {
            quals.insert(format!("{dim}:{n}"));
            used.extend([i, i + 1]);
        } else if let (Some(dim), Some(n)) = (section(i), number(i + 1)) {
            quals.insert(format!("{dim}:{n}"));
            used.extend([i, i + 1]);
        } else if let (Some(o), Some(dim)) = (fs.iter().find_map(|f| table(ORDINALS, f)), section(i + 1)) {
            quals.insert(format!("{dim}:{o}"));
            used.extend([i, i + 1]);
        } else if let (Some(d), true) = (fs.iter().find_map(|f| table(MARKED_DIFFICULTY, f)), marker(i + 1)) {
            quals.insert(format!("difficulty:{d}"));
            used.extend([i, i + 1]);
        } else if let Some(c) = fs.iter().find_map(|f| table(CONDITIONS, f).map(str::to_string).or_else(|| fused_difficulty(f))) {
            quals.insert(c);
            used.insert(i);
        }
    }
    (quals, used)
}

/// 부정소 낱말 (주장에서는 뺀다): 짧은 부정 "안 · 못", 긴 부정 "않다 · 못하다", 서술격 "아니다", 영어 not · never
const NEG_PARTICLES: &[&str] = &["안", "못", "not", "n't", "never", "cannot"];
const NEG_PARTICLE_LEMMAS: &[&str] = &["않", "못하", "아니", "not", "never"];
/// 뜻이 뒤집힌 서술어와 그 긍정 꼴 ("불가" ↔ "가능", "없다" ↔ "있다")
const NEG_PREDICATES: &[(&str, &str)] = &[
    ("불가능", "가능"),
    ("불가", "가능"),
    ("불능", "가능"),
    ("없", "있"),
    ("impossible", "possible"),
    ("unable", "able"),
];

enum Negator {
    /// 뜻 없이 부정만 하는 낱말
    Particle,
    /// 부정 서술어 (긍정 꼴)
    Predicate(&'static str),
}

fn negator(t: &Token) -> Option<Negator> {
    let text = lower(&t.text);
    let text = text.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
    let lemma = lemma_or_text(t);
    if NEG_PARTICLES.contains(&text) || NEG_PARTICLE_LEMMAS.contains(&lemma.as_str()) || text.starts_with('않') {
        return Some(Negator::Particle);
    }
    table(NEG_PREDICATES, &lemma).map(Negator::Predicate)
}

/// 서술 핵의 부정 (PolarityFlip)
struct PredicatePolarity {
    /// 핵에 붙은 부정이 홀수 번
    negated: bool,
    /// 부정소 낱말 (핵에 붙었든 아니든: 주장에서 뺀다)
    particles: BTreeSet<usize>,
    /// 핵에 붙은 부정 서술어 → 긍정 꼴 (주장의 핵심 · 값은 긍정 꼴로 견준다)
    positive: BTreeMap<usize, &'static str>,
    /// 실제 핵: 뿌리, 뿌리가 부정소면("가능하지 않다" 의 않다) 그 뿌리에 기댄 앞 서술어
    root: Option<usize>,
    /// 서술어 글: 핵에 붙은 부정소 + 핵 ("not dodged", "불가")
    text: String,
}

fn predicate_polarity(s: &Sentence) -> PredicatePolarity {
    let root = s.tokens.iter().position(|t| rel_of(t) == "root");
    // 뿌리가 없으면(구문 분석 없음) 모든 부정이 핵의 것이다
    let bound = |i: usize| match root {
        Some(r) => i == r || s.tokens[i].head == r + 1,
        None => true,
    };
    let mut count = 0usize;
    let mut particles = BTreeSet::new();
    let mut bound_particles = Vec::new();
    let mut positive = BTreeMap::new();
    for (i, t) in s.tokens.iter().enumerate() {
        match negator(t) {
            Some(Negator::Particle) => {
                particles.insert(i);
                if bound(i) {
                    count += 1;
                    bound_particles.push(i);
                }
            }
            Some(Negator::Predicate(pos)) if bound(i) => {
                count += 1;
                positive.insert(i, pos);
            }
            _ => {}
        }
    }
    let core = match root {
        Some(r) if particles.contains(&r) => {
            (0..r).rev().find(|&j| s.tokens[j].head == r + 1 && matches!(s.tokens[j].upos.as_str(), "VERB" | "ADJ" | "NOUN")).or(Some(r))
        }
        other => other,
    };
    let mut parts: Vec<usize> = bound_particles;
    parts.extend(core);
    parts.sort_unstable();
    parts.dedup();
    let text = parts.iter().map(|&i| s.tokens[i].text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
    PredicatePolarity {
        negated: count % 2 == 1,
        particles,
        positive,
        root: core,
        text,
    }
}

/// 키의 부정 서술어를 긍정 꼴로 ("회피 불가" → "회피 가능"): 부정이 바뀌어도 같은 주제로 묶는다
fn topic_key(key: &str) -> String {
    key.split(' ').map(|p| table(NEG_PREDICATES, p).unwrap_or(p)).collect::<Vec<_>>().join(" ")
}

/// 문장의 주장: 핵심 원형(주어 · 목적어 · 뿌리)과 값(수치 · 단위 · 형용사), 배타 조건, 서술 핵의 부정
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Claim {
    pub core: BTreeSet<String>,
    pub values: Vec<String>,
    /// 배타 조건 한정자 ("phase:1", "mode:pvp", "difficulty:hard", "platform:mobile" …, [`condition_qualifiers`])
    pub qualifiers: BTreeSet<String>,
    /// 서술 핵에 붙은 부정이 홀수 번 ("회피 불가", "can not be dodged")
    pub negated: bool,
}

/// 3.4절 2단계의 주장 뽑기. 핵심은 주어(한국어 주제어 `dislocated` 포함) · 목적어 · 뿌리의 원형, 값은 NUM(뒤따르는 단위 낱말 ·
/// `%` 를 붙여서), 숫자로 시작하는 수+단위 낱말(`3,000원`), 한국어 계사 서술어의 명사(`배송비는 무료이다` 의 `무료`), ADJ 원형이다.
/// 값이 된 낱말은 핵심에 넣지 않는다. 조건 한정자 낱말("1페이즈")과 부정소("안 · 않다 · not")는 값도 핵심도 아니고, 서술 핵의
/// 부정 서술어("불가 · 없다")는 긍정 꼴("가능 · 있")로 넣고 [`Claim::negated`] 를 켠다.
pub fn claim(s: &Sentence) -> Claim {
    let (qualifiers, conditions) = condition_qualifiers(s);
    let pol = predicate_polarity(s);
    let mut c = Claim {
        qualifiers,
        negated: pol.negated,
        ..Claim::default()
    };
    for (i, t) in s.tokens.iter().enumerate() {
        if conditions.contains(&i) || pol.particles.contains(&i) {
            continue;
        }
        // 기술 기호는 값이다 (`-O2`, `ctrl+shift+b`, `10^4`)
        if let Some(tt) = technical_token(t.text.trim()) {
            c.values.push(tt.canonical);
            continue;
        }
        let rel = rel_of(t);
        let is_root = pol.root == Some(i) || rel == "root";
        let digits = t.text.chars().any(|ch| ch.is_ascii_digit());
        if t.upos == "NUM" || (digits && !is_identifier(t.text.trim())) {
            let mut v = numeric_value(t).unwrap_or_else(|| t.text.trim().to_string());
            if let Some(next) = s.tokens.get(i + 1) {
                let unit = next.text.trim();
                if unit == "%" || (next.upos == "NOUN" && next.head == i + 1) || (next.upos == "SYM" && unit.chars().count() <= 3) {
                    v.push_str(unit);
                }
            }
            c.values.push(v);
            continue;
        }
        let positive = pol.positive.get(&i).map(|p| p.to_string());
        if t.upos == "ADJ" {
            c.values.push(positive.unwrap_or_else(|| lemma_or_text(t)));
            continue;
        }
        if is_root {
            if let Some(stem) = copula_stem(t) {
                c.values.push(positive.unwrap_or(stem));
                continue;
            }
        }
        let content = matches!(t.upos.as_str(), "NOUN" | "PROPN" | "VERB");
        if content && (is_root || matches!(rel, "nsubj" | "obj" | "dislocated")) {
            c.core.insert(positive.unwrap_or_else(|| lemma_or_text(t)));
        }
    }
    for tt in technical_tokens(&s.text).into_iter().filter(|t| t.family == "col" || t.family == "row") {
        c.values.push(tt.canonical);
    }
    c.values.sort();
    c
}

/// 배타 조건 가드: 두 주장 모두 조건이 있고 서로 다르면 둘 다 맞는 말이다 ("1페이즈 20%" · "2페이즈 15%")
pub fn condition_disjoint(old: &Claim, new: &Claim) -> bool {
    !old.qualifiers.is_empty() && !new.qualifiers.is_empty() && old.qualifiers != new.qualifiers
}

/// 무엇이 바뀌었나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// 수치 · 단위 · 형용사 · 계사 서술 명사 ("20%" → "15%", "3,000원" → "무료")
    ValueChanged,
    /// 핵심이 같고 서술 핵의 부정만 뒤집힘 ("회피 가능" → "회피 불가")
    PolarityFlip,
}

/// 바뀐 주장 하나
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// 옛 기록 · 새 기록의 문장 번호
    pub old_sentence: usize,
    pub new_sentence: usize,
    pub core: Vec<String>,
    /// 값이 바뀌었으면 양쪽 값, 부정만 뒤집혔으면 양쪽 서술어 글 ("가능" · "불가", "dodged" · "not dodged")
    pub old_values: Vec<String>,
    pub new_values: Vec<String>,
    pub kind: ChangeKind,
}

/// 두 기록의 관계 (3.4절 4단계)
#[derive(Debug, Clone, PartialEq)]
pub enum Relation {
    /// 새 기록이 옛 기록을 대체한다: 옛 기록은 discount (PD-Verdict `superseded`)
    Supersedes { changes: Vec<Change>, containment: f32 },
    /// 새 기록이 옛 기록을 바뀜 없이 담는다: 중복으로 합친다
    Duplicate { containment: f32 },
    /// 일부만 겹친다, 또는 배타 조건이 달라 둘 다 맞다: 의미 연결만
    SemanticLink { aligned: usize, containment: f32 },
    Unrelated { containment: f32 },
}

/// 비교의 중간 결과 (진단)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Comparison {
    /// (옛 문장, 새 문장, 코사인)
    pub aligned: Vec<(usize, usize, f32)>,
    pub align_threshold: f32,
    pub containment: f32,
    /// 배타 조건 가드가 갈라 놓은 맞춘 쌍 (옛 문장, 새 문장): 하나라도 있으면 대체 · 중복이 아니다
    pub coexisting: Vec<(usize, usize)>,
}

fn core_overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 부정 반전을 바뀐 주장으로 보는 핵심 겹침 하한 (값 변경은 0.5)
const POLARITY_CORE_OVERLAP: f32 = 0.6;

/// 기록의 주제 키: Relay Key 를 대표 키로, 부정 서술어는 긍정 꼴로
fn topic_set(r: &Record, keys: &KeyClusters) -> BTreeSet<String> {
    r.sentences.iter().flat_map(relay_keys).map(|k| topic_key(&keys.canon(&k.key))).collect()
}

/// 옛 기록 `old` 와 새 기록 `new` 를 견준다 (3.4절):
/// 1. 문장 맞추기: 새 × 옛 문장 코사인 행렬 → 이중 센터링 → 1:1 배정, 원래 코사인이 띠(바닥: [`RelayConfig::effective_align_floor`])
///    이상인 쌍만.
/// 2. 주장 견주기: 맞춘 쌍의 배타 조건이 다르면 갈라 둔다(배타 조건 가드). 아니면 핵심 원형이 절반 이상 같고 값이 다르면 바뀐
///    주장(값 변경), 핵심이 0.6 이상 같고 서술 핵의 부정이 다르면 바뀐 주장(부정 반전).
/// 3. 포함률: 옛 기록 Relay Key(대표 키로) 중 새 기록에도 있는 비율.
/// 4. 판정: 갈라 둔 쌍이 있으면 의미 연결. 아니면 바뀐 주장이 있고 포함률이 하한 이상이고 새 기록이 더 늦으면 대체, 옛 문장이
///    모두 바뀜 없이 맞춰지고 포함률이 하한 이상이면 중복, 일부만 맞으면 의미 연결.
pub fn compare(old: &Record, new: &Record, keys: &KeyClusters, embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> (Relation, Comparison) {
    let old_texts: Vec<&str> = old.sentences.iter().map(|s| s.text.as_str()).collect();
    let new_texts: Vec<&str> = new.sentences.iter().map(|s| s.text.as_str()).collect();
    let (ov, nv) = (embedder.embed_batch(&old_texts), embedder.embed_batch(&new_texts));
    compare_vectors(old, new, &ov, &nv, keys, None, embedder.space(), cfg)
}

/// [`compare`] 의 몸통: 문장 벡터를 받아서. `cores` 가 있으면 주장의 핵심 원형도 대표 꼴로 견준다(다국어: 피해 ↔ damage).
#[allow(clippy::too_many_arguments)]
fn compare_vectors(
    old: &Record,
    new: &Record,
    ov: &[Vec<f32>],
    nv: &[Vec<f32>],
    keys: &KeyClusters,
    cores: Option<&KeyClusters>,
    space: &str,
    cfg: &RelayConfig,
) -> (Relation, Comparison) {
    let raw: Vec<Vec<f32>> = nv.iter().map(|n| ov.iter().map(|o| cosine(n, o).max(0.0)).collect()).collect();
    let cells: Vec<f32> = raw.iter().flatten().copied().collect();
    let align_threshold = upper_band(&cells, cfg.effective_align_floor(space), cfg).min(0.999);
    let centered = matrix::double_center(&raw);
    let assigned = matrix::exclusive_assign_by_score(&centered, -0.99, cfg.align_margin);
    let mut aligned = Vec::new();
    for (n, a) in assigned.iter().enumerate() {
        if let Some((o, _, _)) = a {
            let c = raw[n][*o];
            if c >= align_threshold {
                aligned.push((*o, n, c));
            }
        }
    }
    aligned.sort_by_key(|(o, n, _)| (*o, *n));
    let (ko, kn) = (topic_set(old, keys), topic_set(new, keys));
    let containment = if ko.is_empty() { 0.0 } else { ko.intersection(&kn).count() as f32 / ko.len() as f32 };
    let canon_core = |c: &BTreeSet<String>| -> BTreeSet<String> {
        match cores {
            Some(m) => c.iter().map(|x| m.canon(x)).collect(),
            None => c.clone(),
        }
    };
    let mut changes = Vec::new();
    let mut coexisting = Vec::new();
    for (o, n, _) in &aligned {
        let (co, cn) = (claim(&old.sentences[*o]), claim(&new.sentences[*n]));
        if condition_disjoint(&co, &cn) {
            coexisting.push((*o, *n));
            continue;
        }
        let (core_o, core_n) = (canon_core(&co.core), canon_core(&cn.core));
        let overlap = core_overlap(&core_o, &core_n);
        let values_changed = co.values != cn.values && !(co.values.is_empty() && cn.values.is_empty());
        let core: Vec<String> = core_o.intersection(&core_n).cloned().collect();
        if co.negated != cn.negated && overlap >= POLARITY_CORE_OVERLAP {
            let (old_values, new_values) = if values_changed {
                (co.values, cn.values)
            } else {
                let text = |s: &Sentence, negated: bool| {
                    let t = predicate_polarity(s).text;
                    if t.is_empty() {
                        (if negated { "negated" } else { "affirmative" }).to_string()
                    } else {
                        t
                    }
                };
                (vec![text(&old.sentences[*o], co.negated)], vec![text(&new.sentences[*n], cn.negated)])
            };
            changes.push(Change {
                old_sentence: *o,
                new_sentence: *n,
                core,
                old_values,
                new_values,
                kind: ChangeKind::PolarityFlip,
            });
        } else if values_changed && overlap >= 0.5 {
            changes.push(Change {
                old_sentence: *o,
                new_sentence: *n,
                core,
                old_values: co.values,
                new_values: cn.values,
                kind: ChangeKind::ValueChanged,
            });
        }
    }
    let comparison = Comparison {
        aligned: aligned.clone(),
        align_threshold,
        containment,
        coexisting: coexisting.clone(),
    };
    let covered = containment >= cfg.containment;
    let relation = if !coexisting.is_empty() {
        Relation::SemanticLink { aligned: aligned.len(), containment }
    } else if !changes.is_empty() && covered && new.at_ms > old.at_ms {
        Relation::Supersedes { changes, containment }
    } else if changes.is_empty() && covered && !old.sentences.is_empty() && aligned.len() == old.sentences.len() {
        Relation::Duplicate { containment }
    } else if !aligned.is_empty() {
        Relation::SemanticLink { aligned: aligned.len(), containment }
    } else {
        Relation::Unrelated { containment }
    };
    (relation, comparison)
}

/// 판정 묶음의 평가 하나: 서버 id, 장소(`cc`), 대상(`to`, 없을 수 있음), 분석한 기록
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub id: &'a str,
    pub cc: &'a str,
    pub to: Option<&'a str>,
    pub record: &'a Record,
}

/// 대체 관계 하나: `new_id` 가 `old_id` 를 대체한다. 옛 평가는 PD-Verdict 의 `superseded` 입력이 켜져 discount(−2, 이유 `superseded`)가 된다.
#[derive(Debug, Clone, PartialEq)]
pub struct Supersession {
    pub old_id: String,
    pub new_id: String,
    pub changes: Vec<Change>,
    pub containment: f32,
}

/// 3.4절을 평가 묶음에: 같은 장소 · 대상(`cc` · `to`)끼리 시간순(같으면 id 순)으로 놓고, 평가마다 그보다 늦은 평가들과 견줘 대체되는지 본다.
/// 한 옛 평가에는 그것을 대체하는 가장 이른 새 평가 하나만 낸다(그 새 평가가 다시 대체되면 사슬로 이어짐). Relay Key 묶기는 묶음마다 한 번.
/// 결과는 (cc, to, 옛 평가 시각) 순서다. 서버 `POST /evaluations/supersedes` 의 같은 (cc, to) 규칙 그대로다 — `to` 가 다른 쌍은
/// [`curate_cross_scope`] 가 본다.
pub fn supersessions(entries: &[Entry<'_>], embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> Vec<Supersession> {
    let mut groups: BTreeMap<(&str, Option<&str>), Vec<&Entry<'_>>> = BTreeMap::new();
    for e in entries {
        groups.entry((e.cc, e.to)).or_default().push(e);
    }
    let mut out = Vec::new();
    for (_, mut group) in groups {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| a.record.at_ms.cmp(&b.record.at_ms).then_with(|| a.id.cmp(b.id)));
        let all: Vec<RelayKey> = group.iter().flat_map(|e| e.record.sentences.iter().flat_map(relay_keys)).collect();
        let keys = cluster_keys(&all, embedder, cfg);
        for (i, old) in group.iter().enumerate() {
            for new in &group[i + 1..] {
                if new.record.at_ms <= old.record.at_ms {
                    continue;
                }
                if let (Relation::Supersedes { changes, containment }, _) = compare(old.record, new.record, &keys, embedder, cfg) {
                    out.push(Supersession {
                        old_id: old.id.to_string(),
                        new_id: new.id.to_string(),
                        changes,
                        containment,
                    });
                    break;
                }
            }
        }
    }
    out
}

/// 바뀐 값 요약 (옛 값, 새 값): 첫 바뀐 주장에서 한쪽에만 있는 값. 하나씩이면 그 짝, 여럿이면 단위가 같은 첫 짝("20%" · "15%"),
/// 그것도 없으면 쉼표로 이은 것. 바뀐 주장이 없거나 다른 값이 없으면 None.
pub fn value_diff(changes: &[Change]) -> Option<(String, String)> {
    let c = changes.first()?;
    let old_only: Vec<&String> = c.old_values.iter().filter(|v| !c.new_values.contains(v)).collect();
    let new_only: Vec<&String> = c.new_values.iter().filter(|v| !c.old_values.contains(v)).collect();
    if old_only.is_empty() && new_only.is_empty() {
        return None;
    }
    if old_only.len() == 1 && new_only.len() == 1 {
        return Some((old_only[0].clone(), new_only[0].clone()));
    }
    let unit = |v: &str| v.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == ',' || ch == '.').to_string();
    for o in &old_only {
        let u = unit(o);
        if u.is_empty() || u.len() == o.len() {
            continue;
        }
        if let Some(n) = new_only.iter().find(|n| unit(n) == u && n.len() != u.len()) {
            return Some(((*o).clone(), (*n).clone()));
        }
    }
    let join = |vs: &[&String]| vs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
    Some((join(&old_only), join(&new_only)))
}

/// 교차 스코프 대체 하나: 같은 `cc` 안에서 `new_id` 가 `old_id` 를 대체한다 (`to` 는 달라도 된다)
#[derive(Debug, Clone, PartialEq)]
pub struct CrossScopeSupersession {
    pub cc: String,
    pub old_id: String,
    pub new_id: String,
    pub old_to: Option<String>,
    pub new_to: Option<String>,
    pub old_at_ms: i64,
    pub new_at_ms: i64,
    pub changes: Vec<Change>,
    pub containment: f32,
    /// 바뀐 값 요약 ([`value_diff`]): ("20%", "15%"), 부정 반전이면 ("가능", "불가")
    pub value_diff: Option<(String, String)>,
}

impl CrossScopeSupersession {
    /// 서버 `POST /evaluations/supersedes` 의 (cc, to) 규칙에 맞는가: `cc` 는 이미 같으니 `old.to == new.to`. 다르면 서버가 400 으로
    /// 영구 거부하므로 로컬 격리만 한다 ([`supersede_sync`]).
    pub fn same_scope(&self) -> bool {
        self.old_to == self.new_to
    }
}

/// 의미 연결 하나: 같은 이야기지만 대체도 중복도 아니다
#[derive(Debug, Clone, PartialEq)]
pub struct CrossScopeLink {
    pub cc: String,
    pub old_id: String,
    pub new_id: String,
    pub aligned: usize,
    pub containment: f32,
    /// 배타 조건이 달라 둘 다 유효하다 (배타 조건 가드: "1페이즈 20%" · "2페이즈 15%"): 둘 다 추천 후보로 둔다
    pub coexisting: bool,
}

/// 교차 스코프 큐레이션에서 본 평가 하나의 상태
#[derive(Debug, Clone, PartialEq)]
pub enum SupersedeState {
    /// 대체 · 중복 없음: 추천 후보 그대로
    Active,
    /// 로컬에서 확정한 대체: `new_id` 가 이 평가를 대체했다 (추천 슬롯에서 빼고, 로컬 판정 입력 `superseded`)
    Confirmed { new_id: String, value_diff: Option<(String, String)> },
    /// 바뀐 것 없이 늦은 평가 `of` 와 같은 내용: 하나로 합친다
    Duplicate { of: String },
}

/// [`curate_cross_scope`] 의 결과
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CrossScopeReport {
    pub supersessions: Vec<CrossScopeSupersession>,
    /// (옛 평가, 같은 내용의 늦은 평가)
    pub duplicates: Vec<(String, String)>,
    pub links: Vec<CrossScopeLink>,
    /// 입력한 평가마다의 상태
    pub states: BTreeMap<String, SupersedeState>,
}

impl CrossScopeReport {
    /// 평가의 상태 (모르는 id 는 Active)
    pub fn state(&self, id: &str) -> SupersedeState {
        self.states.get(id).cloned().unwrap_or(SupersedeState::Active)
    }

    /// 대체도 중복도 아닌가 (추천 후보로 올릴 수 있나)
    pub fn recommendable(&self, id: &str) -> bool {
        matches!(self.state(id), SupersedeState::Active)
    }

    /// `id` 를 대체한 판정
    pub fn superseder(&self, id: &str) -> Option<&CrossScopeSupersession> {
        self.supersessions.iter().find(|s| s.old_id == id)
    }

    /// 사슬 끝: A ≺ B ≺ C 이면 A · B 모두 C. 대체되지 않았으면 자기 자신.
    pub fn final_version(&self, id: &str) -> String {
        let mut cur = id.to_string();
        for _ in 0..=self.supersessions.len() {
            match self.superseder(&cur) {
                Some(s) => cur = s.new_id.clone(),
                None => break,
            }
        }
        cur
    }
}

/// RAG 후보 큐레이션용 교차 스코프 비교 (서버 보고용 [`supersessions`] 는 그대로 같은 (cc, to) 규칙):
///
/// 1. 같은 `cc` 끼리 시간순(같으면 id 순)으로 놓는다. `to` 는 보지 않는다 (전역 옛 공략 `to: None` ↔ 체크포인트 새 공략 `to: Some`).
/// 2. 묶음마다 Relay Key 를 다국어로 묶고(말레니아 ↔ malenia), 주장의 핵심 원형도 묶는다(피해 ↔ damage: 다국어 임베더가 있을 때).
/// 3. 대표 주제 키가 하나라도 겹치는 쌍만 [`compare`] 와 같은 규칙으로 견준다(문장 벡터는 묶음마다 한 번에).
/// 4. 옛 평가마다 그것을 대체하는 가장 이른 새 평가 하나 → [`SupersedeState::Confirmed`] (값 요약 포함), 같은 내용 →
///    [`SupersedeState::Duplicate`], 의미 연결(배타 조건으로 공존 포함) → [`CrossScopeLink`].
///
/// 해시 임베더는 글자만 보므로 언어가 다른 기록끼리는 묶지 못한다: 다국어 대체는 granite 같은 다국어 임베더가 있어야 한다.
pub fn curate_cross_scope(entries: &[Entry<'_>], embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> CrossScopeReport {
    let mut report = CrossScopeReport::default();
    let mut groups: BTreeMap<&str, Vec<&Entry<'_>>> = BTreeMap::new();
    for e in entries {
        groups.entry(e.cc).or_default().push(e);
        report.states.insert(e.id.to_string(), SupersedeState::Active);
    }
    let space = embedder.space().to_string();
    for (cc, mut group) in groups {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| a.record.at_ms.cmp(&b.record.at_ms).then_with(|| a.id.cmp(b.id)));
        let all: Vec<RelayKey> = group.iter().flat_map(|e| e.record.sentences.iter().flat_map(relay_keys)).collect();
        let keys = cluster_keys(&all, embedder, cfg);
        let core_terms: Vec<RelayKey> = group
            .iter()
            .flat_map(|e| e.record.sentences.iter().flat_map(|s| claim(s).core))
            .map(|key| RelayKey { key, kind: KeyKind::Compound })
            .collect();
        let cores = cluster_keys(&core_terms, embedder, cfg);
        let topics: Vec<BTreeSet<String>> = group.iter().map(|e| topic_set(e.record, &keys)).collect();
        let texts: Vec<&str> = group.iter().flat_map(|e| e.record.sentences.iter().map(|s| s.text.as_str())).collect();
        let mut vecs = embedder.embed_batch(&texts).into_iter();
        let per: Vec<Vec<Vec<f32>>> = group.iter().map(|e| e.record.sentences.iter().map(|_| vecs.next().unwrap_or_default()).collect()).collect();
        for i in 0..group.len() {
            for j in i + 1..group.len() {
                if topics[i].is_disjoint(&topics[j]) {
                    continue;
                }
                let (old, new) = (group[i], group[j]);
                let (relation, cmp) = compare_vectors(old.record, new.record, &per[i], &per[j], &keys, Some(&cores), &space, cfg);
                match relation {
                    Relation::Supersedes { changes, containment } => {
                        let diff = value_diff(&changes);
                        report.states.insert(
                            old.id.to_string(),
                            SupersedeState::Confirmed {
                                new_id: new.id.to_string(),
                                value_diff: diff.clone(),
                            },
                        );
                        report.supersessions.push(CrossScopeSupersession {
                            cc: cc.to_string(),
                            old_id: old.id.to_string(),
                            new_id: new.id.to_string(),
                            old_to: old.to.map(str::to_string),
                            new_to: new.to.map(str::to_string),
                            old_at_ms: old.record.at_ms,
                            new_at_ms: new.record.at_ms,
                            changes,
                            containment,
                            value_diff: diff,
                        });
                        break;
                    }
                    Relation::Duplicate { .. } => {
                        report.duplicates.push((old.id.to_string(), new.id.to_string()));
                        if matches!(report.states.get(old.id), Some(SupersedeState::Active)) {
                            report.states.insert(old.id.to_string(), SupersedeState::Duplicate { of: new.id.to_string() });
                        }
                    }
                    Relation::SemanticLink { aligned, containment } => report.links.push(CrossScopeLink {
                        cc: cc.to_string(),
                        old_id: old.id.to_string(),
                        new_id: new.id.to_string(),
                        aligned,
                        containment,
                        coexisting: !cmp.coexisting.is_empty(),
                    }),
                    Relation::Unrelated { .. } => {}
                }
            }
        }
    }
    report
}

/// Phase 6 Part 1: 비문장 물리 체크포인트(화면 구조·비전 도메인·배지 라벨·탭 제목·활성 토픽)의 메타데이터.
#[derive(Debug, Clone, Default)]
pub struct CheckpointMeta<'a> {
    pub vision_tag: Option<&'a str>,
    pub screen_label: Option<&'a str>,
    pub badge_labels: &'a [&'a str],
    pub tab_title: Option<&'a str>,
    pub active_topics: &'a [&'a str],
    /// 민감 화면이거나 시크릿 탭이면 키 추출을 0건으로 차단한다.
    pub sensitive_or_incognito: bool,
}

/// Phase 6 Part 1: 문장이 없는 UIA·비전 체크포인트에서 비민감 RelayKey 를 승격 추출한다 (`CheckpointRelayExtractor`).
pub fn lift_checkpoint_relay_keys(meta: &CheckpointMeta<'_>) -> Vec<RelayKey> {
    if meta.sensitive_or_incognito {
        return Vec::new();
    }
    let mut out: BTreeSet<RelayKey> = BTreeSet::new();
    let mut ingest = |raw: &str, default_kind: KeyKind| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        for q in quoted_spans(trimmed) {
            out.insert(RelayKey { key: q, kind: KeyKind::Quoted });
        }
        // 기술 기호는 쪼개기 전에 정규 꼴 그대로 (`-j` · `C 열` 은 쪼개면 2글자 미만으로 사라진다)
        for tt in technical_tokens(trimmed) {
            out.insert(RelayKey { key: tt.canonical, kind: KeyKind::TechnicalToken });
        }
        for tok in trimmed.split(|c: char| c.is_whitespace() || matches!(c, '|' | '/' | '-' | '·' | ',' | '(' | ')' | '[' | ']')) {
            let clean = tok.trim_matches(|c: char| matches!(c, '.' | '!' | '?' | '"' | '\'' | ':' | ';'));
            if clean.chars().count() < 2 {
                continue;
            }
            if clean.starts_with("0x") && clean.len() >= 10 {
                continue; // 원시 해시는 RelayKey 로 올리지 않는다
            }
            let kind = if is_identifier(clean) {
                KeyKind::Identifier
            } else if clean.contains("::") || clean.contains('_') {
                KeyKind::Symbol
            } else {
                default_kind
            };
            out.insert(RelayKey { key: lower(clean), kind });
        }
    };
    if let Some(tag) = meta.vision_tag {
        ingest(tag, KeyKind::Symbol);
    }
    if let Some(lbl) = meta.screen_label {
        ingest(lbl, KeyKind::Proper);
    }
    for &b in meta.badge_labels {
        ingest(b, KeyKind::Proper);
    }
    if let Some(t) = meta.tab_title {
        ingest(t, KeyKind::Proper);
    }
    for &topic in meta.active_topics {
        ingest(topic, KeyKind::Proper);
    }
    out.into_iter().collect()
}

/// Phase 6 Part 1: 영점 붕괴 없는 가산-보정 메타 전이 점수 `S_meta(a -> b)` 와 공유 대표 키 목록.
///
/// * 정규화 대표 키 교집합 `K*(a) ∩ K*(b)` 이 비어 있으면(무관한 앱 전환, CR-02) 즉시 `(0.0, [])` 를 반환해 PLINKO 절벽을 유지한다.
/// * 공유 키가 있으면 `π_trans * (0.65 * J_canon + 0.35 * max(0, cos(v_a, v_b)))` 로 궤적 연속성을 산출한다.
pub fn meta_transition_score(
    keys_a: &[RelayKey],
    keys_b: &[RelayKey],
    vec_a: &[f32],
    vec_b: &[f32],
    clusters: &KeyClusters,
    transition_prior: f32,
) -> (f32, Vec<String>) {
    let set_a: BTreeSet<String> = keys_a.iter().map(|k| clusters.canon(&k.key)).collect();
    let set_b: BTreeSet<String> = keys_b.iter().map(|k| clusters.canon(&k.key)).collect();
    let shared: Vec<String> = set_a.intersection(&set_b).cloned().collect();
    if shared.is_empty() {
        return (0.0, Vec::new());
    }
    let union_len = set_a.union(&set_b).count().max(1);
    let jaccard = shared.len() as f32 / union_len as f32;
    let cos = if !vec_a.is_empty() && vec_a.len() == vec_b.len() {
        cosine(vec_a, vec_b).max(0.0)
    } else {
        0.0
    };
    let pi = if transition_prior.is_finite() && transition_prior > 0.0 {
        transition_prior.clamp(0.85, 1.25)
    } else {
        1.0
    };
    (pi * (0.65 * jaccard + 0.35 * cos), shared)
}

/// 서로 다른 `cc`(앱 · 웹) 엔트리 사이의 로컬 Relay Key 브리지 간선 (Phase 6 Part 1).
#[derive(Debug, Clone, PartialEq)]
pub struct RelayFederationLink {
    pub a_id: String,
    pub a_cc: String,
    pub b_id: String,
    pub b_cc: String,
    pub shared_keys: Vec<String>,
    pub relation: Relation,
}

/// Phase 6 Part 1: 서로 다른 `cc`(예: `eldenring.exe` ↔ `fextralife.com`) 기록끼리도 다국어 `cluster_keys` 로
/// 공유 대표 키를 찾아 로컬 브리지 간선 및 로컬 격리 후보를 산출한다.
///
/// 서버 `POST /evaluations/supersedes` 는 반드시 같은 `(cc, to)` 여야 하므로 이 함수가 낸 교차 `cc` 대체 후보는
/// 호출 측에서 [`supersede_sync`] (`same_scope = false` -> `LocalOnly(CrossScope)`)를 거쳐 서버 호출 0건을 보장한다.
pub fn curate_relay_federation(
    entries: &[Entry<'_>],
    embedder: &dyn TextEmbedder,
    cfg: &RelayConfig,
) -> (Vec<RelayFederationLink>, Vec<CrossScopeSupersession>) {
    if entries.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    let mut ordered: Vec<&Entry<'_>> = entries.iter().collect();
    ordered.sort_by(|a, b| a.record.at_ms.cmp(&b.record.at_ms).then_with(|| a.id.cmp(b.id)));
    let all_keys: Vec<RelayKey> = ordered.iter().flat_map(|e| e.record.sentences.iter().flat_map(relay_keys)).collect();
    let keys = cluster_keys(&all_keys, embedder, cfg);
    let core_terms: Vec<RelayKey> = ordered
        .iter()
        .flat_map(|e| e.record.sentences.iter().flat_map(|s| claim(s).core))
        .map(|key| RelayKey { key, kind: KeyKind::Compound })
        .collect();
    let cores = cluster_keys(&core_terms, embedder, cfg);
    let topics: Vec<BTreeSet<String>> = ordered.iter().map(|e| topic_set(e.record, &keys)).collect();
    let texts: Vec<&str> = ordered.iter().flat_map(|e| e.record.sentences.iter().map(|s| s.text.as_str())).collect();
    let mut vecs = embedder.embed_batch(&texts).into_iter();
    let per: Vec<Vec<Vec<f32>>> = ordered.iter().map(|e| e.record.sentences.iter().map(|_| vecs.next().unwrap_or_default()).collect()).collect();
    let space = embedder.space().to_string();

    let mut links = Vec::new();
    let mut local_supersessions = Vec::new();
    for i in 0..ordered.len() {
        for j in i + 1..ordered.len() {
            if ordered[i].cc == ordered[j].cc {
                continue;
            }
            let shared: Vec<String> = topics[i].intersection(&topics[j]).cloned().collect();
            if shared.is_empty() {
                continue;
            }
            let (old, new) = (ordered[i], ordered[j]);
            let (relation, _) = compare_vectors(old.record, new.record, &per[i], &per[j], &keys, Some(&cores), &space, cfg);
            if let Relation::Supersedes { changes, containment } = &relation {
                local_supersessions.push(CrossScopeSupersession {
                    cc: old.cc.to_string(),
                    old_id: old.id.to_string(),
                    new_id: new.id.to_string(),
                    old_to: old.to.map(str::to_string),
                    new_to: None, // 교차 cc: same_scope() == false 보장 → 서버 supersede 오호출 0건!
                    old_at_ms: old.record.at_ms,
                    new_at_ms: new.record.at_ms,
                    changes: changes.clone(),
                    containment: *containment,
                    value_diff: value_diff(changes),
                });
            }
            links.push(RelayFederationLink {
                a_id: old.id.to_string(),
                a_cc: old.cc.to_string(),
                b_id: new.id.to_string(),
                b_cc: new.cc.to_string(),
                shared_keys: shared,
                relation,
            });
        }
    }
    (links, local_supersessions)
}

/// 서버 보고(pd_supersede) 자격을 가리는 사실들. 부르는 쪽(raon-zena pd_bridge)이 동기화 스냅샷 · 로그인 상태에서 채운다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SupersedeFacts {
    /// 옛 · 새 평가가 내 글인가 (source ∈ {Local, Both})
    pub old_mine: bool,
    pub new_mine: bool,
    /// 로그인했나 · 공유에 동의했나
    pub signed_in: bool,
    pub sharing_consent: bool,
    /// 서버에 저장 끝(stored, 서버 id 확정)인가 — pending(202)이면 서버가 404 를 준다
    pub old_stored: bool,
    pub new_stored: bool,
}

/// 서버에 보내지 않는 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOnlyReason {
    /// 제3자끼리의 대체: 서버는 둘 중 하나를 쓴 사람만 받는다 (403)
    ThirdParty,
    /// 로그인 · 공유 동의가 없다 (401, 분당 쓰기 한도 낭비)
    SignedOut,
    /// `to` 가 다르다: 서버는 같은 (cc, to) 만 받는다 (400, 클라이언트가 영구 거부로 버린다)
    CrossScope,
}

/// 서버 보고 판정 (SupersedeSyncGate)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupersedeSync {
    /// 지금 `pd_supersede(old, new)` 를 보낸다
    Report,
    /// 같은 (cc, to) · 내 글이지만 한쪽이 아직 pending: 둘 다 stored 가 되면 보낸다 ([`QuarantineLedger::drain_ready_server_reports`])
    Defer,
    /// 서버에 보내지 않는다 (로컬 격리는 그대로)
    LocalOnly(LocalOnlyReason),
}

/// 대체 판정을 서버에 알릴지 (교차 스코프 대체도 로컬 격리는 늘 한다). 차례로: ① 소유권(둘 중 하나가 내 글), ② 로그인 · 공유 동의,
/// ③ 스코프(`old_to == new_to`, `cc` 는 같다고 본다), ④ 둘 다 stored. ③ 이 서버 `POST /evaluations/supersedes` 의 400 을 막는
/// 핵심이다 — pd_supersede 는 `old.to == new.to` 일 때만 부른다.
pub fn supersede_sync(old_to: Option<&str>, new_to: Option<&str>, facts: &SupersedeFacts) -> SupersedeSync {
    if !(facts.old_mine || facts.new_mine) {
        SupersedeSync::LocalOnly(LocalOnlyReason::ThirdParty)
    } else if !(facts.signed_in && facts.sharing_consent) {
        SupersedeSync::LocalOnly(LocalOnlyReason::SignedOut)
    } else if old_to != new_to {
        SupersedeSync::LocalOnly(LocalOnlyReason::CrossScope)
    } else if facts.old_stored && facts.new_stored {
        SupersedeSync::Report
    } else {
        SupersedeSync::Defer
    }
}

/// 격리 항목의 서버 전송 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineSyncState {
    /// `to` 가 달라 로컬 격리만 (서버 400 방지)
    LocalOnlyCrossScope,
    /// 보낼 차례를 기다린다: 둘 다 stored 가 되면 [`QuarantineLedger::drain_ready_server_reports`] 가 한 번 내준다
    DeferredPending,
    /// 서버에 보냈다
    ReportedServer,
    /// 제3자끼리의 대체: 로컬 격리만 (서버 403 방지)
    ThirdPartyLocal,
    /// 로그인 · 공유 동의가 없어 로컬 격리만
    LocalOnlySignedOut,
}

impl QuarantineSyncState {
    /// 저장용 이름 (quarantine_v1.json)
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineSyncState::LocalOnlyCrossScope => "local_only_cross_scope",
            QuarantineSyncState::DeferredPending => "deferred_pending",
            QuarantineSyncState::ReportedServer => "reported_server",
            QuarantineSyncState::ThirdPartyLocal => "third_party_local",
            QuarantineSyncState::LocalOnlySignedOut => "local_only_signed_out",
        }
    }

    pub fn from_name(name: &str) -> Option<QuarantineSyncState> {
        [
            QuarantineSyncState::LocalOnlyCrossScope,
            QuarantineSyncState::DeferredPending,
            QuarantineSyncState::ReportedServer,
            QuarantineSyncState::ThirdPartyLocal,
            QuarantineSyncState::LocalOnlySignedOut,
        ]
        .into_iter()
        .find(|s| s.as_str() == name)
    }
}

impl From<SupersedeSync> for QuarantineSyncState {
    /// 장부에 적을 처음 상태: 보낼 것(Report · Defer)은 모두 DeferredPending — 보낸 것은 장부가 한 번만 내준다
    fn from(s: SupersedeSync) -> Self {
        match s {
            SupersedeSync::Report | SupersedeSync::Defer => QuarantineSyncState::DeferredPending,
            SupersedeSync::LocalOnly(LocalOnlyReason::ThirdParty) => QuarantineSyncState::ThirdPartyLocal,
            SupersedeSync::LocalOnly(LocalOnlyReason::SignedOut) => QuarantineSyncState::LocalOnlySignedOut,
            SupersedeSync::LocalOnly(LocalOnlyReason::CrossScope) => QuarantineSyncState::LocalOnlyCrossScope,
        }
    }
}

/// 로컬 격리 한 건: `new_id` 가 `old_id` 를 대체해 옛 평가를 로컬 판정에서 discount(−2)로 내린다
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantineEntry {
    pub cc: String,
    pub old_id: String,
    pub new_id: String,
    pub old_to: Option<String>,
    pub new_to: Option<String>,
    /// 두 평가를 쓴 시각: 간선은 `new_at_ms > old_at_ms` 일 때만 (사슬에 고리가 생기지 않게)
    pub old_at_ms: i64,
    pub new_at_ms: i64,
    pub value_diff: Option<(String, String)>,
    pub recorded_at_ms: i64,
    pub sync_state: QuarantineSyncState,
}

impl QuarantineEntry {
    /// 교차 스코프 판정 하나에서
    pub fn from_supersession(s: &CrossScopeSupersession, sync_state: QuarantineSyncState, recorded_at_ms: i64) -> QuarantineEntry {
        QuarantineEntry {
            cc: s.cc.clone(),
            old_id: s.old_id.clone(),
            new_id: s.new_id.clone(),
            old_to: s.old_to.clone(),
            new_to: s.new_to.clone(),
            old_at_ms: s.old_at_ms,
            new_at_ms: s.new_at_ms,
            value_diff: s.value_diff.clone(),
            recorded_at_ms,
            sync_state,
        }
    }
}

/// 대체한 쪽(새 평가)이 무너졌다고 보는 전역 근거량 하한
pub const FALL_MIN_EVIDENCE: f64 = 3.0;

/// 대체한 쪽(새 평가)의 지금 판정 요약: 자가 복권의 입력. 부르는 쪽이 로컬 판정과 서버 판정 캐시(PD-Verdict 의 tier · grade ·
/// evidence · reasons)에서 채운다. 기본값은 "아무것도 모름"(무너지지 않음 = 격리 유지)이다.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Standing {
    /// 로컬 판정이 discount (수동 ▼ 포함; 이 장부의 격리 때문에 내려간 것은 넣지 않는다)
    pub local_discount: bool,
    /// 로컬 긍정 근거가 있다 (로컬 premium, 내 추천): 있으면 전역 판정만으로는 무너지지 않는다
    pub local_positive: bool,
    /// 서버 전역 판정 grade (−2 ..= 2, 판정 없음 None): −1 이하가 discount
    pub global_grade: Option<i32>,
    /// 서버 전역 판정의 근거량 E
    pub global_evidence: f64,
    /// 서버가 거부했거나(rejected) 없어졌다(missing)
    pub gone: bool,
    /// 서버 판정 이유에 `superseded` 가 있다: 더 새 글이 이 글을 대체했다 (무너진 것이 아니라 사슬이 이어진 것)
    pub superseded: bool,
}

impl Standing {
    /// 무너졌나: 로컬 discount, 또는 로컬 긍정 근거 없이 전역 discount(grade ≤ −1, E ≥ 3.0) · rejected · missing.
    /// 더 새 글에 대체되어 내려간 것(`superseded`)은 무너진 것이 아니다.
    pub fn fallen(&self) -> bool {
        if self.superseded {
            return false;
        }
        let global_discount = self.global_grade.is_some_and(|g| g <= -1) && self.global_evidence >= FALL_MIN_EVIDENCE;
        self.local_discount || (!self.local_positive && (self.gone || global_discount))
    }
}

/// 사슬을 따라가는 깊이 한도
const MAX_CHAIN: usize = 32;

/// 제3자 구버전까지 로컬로 격리하는 장부 (순수: 파일 저장은 부르는 쪽이 `entries()` 로 quarantine_v1.json 에 원자적으로).
///
/// * 간선은 `(cc, old_id, new_id)` 하나씩, `new_at_ms > old_at_ms` 일 때만 받는다.
/// * 조회할 때마다 대체한 쪽의 지금 판정([`Standing`])을 대조한다: 무너졌으면 그 간선은 무효라 옛 평가가 저절로 복권된다(자가 복권).
///   대체한 쪽이 다시 더 새 글에 유효하게 대체되어 있으면(A ≺ B ≺ C 의 B) 무너진 것으로 보지 않는다 — C 가 유효하면 A · B 모두 격리.
/// * 상한: `cc` 마다 256건, 전체 1,024건. 넘치면 가장 오래 기록된 것부터 버린다(LRU, 같은 간선을 다시 적으면 새로 기록).
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantineLedger {
    entries: Vec<QuarantineEntry>,
    per_cc_cap: usize,
    total_cap: usize,
}

impl Default for QuarantineLedger {
    fn default() -> Self {
        QuarantineLedger::with_caps(QuarantineLedger::PER_CC_CAP, QuarantineLedger::TOTAL_CAP)
    }
}

impl QuarantineLedger {
    pub const PER_CC_CAP: usize = 256;
    pub const TOTAL_CAP: usize = 1024;

    pub fn new() -> QuarantineLedger {
        QuarantineLedger::default()
    }

    pub fn with_caps(per_cc_cap: usize, total_cap: usize) -> QuarantineLedger {
        QuarantineLedger {
            entries: Vec::new(),
            per_cc_cap: per_cc_cap.max(1),
            total_cap: total_cap.max(1),
        }
    }

    /// Convenience constructor overriding the total entry cap while keeping `per_cc_cap` proportional.
    pub fn with_capacity(total_cap: usize) -> QuarantineLedger {
        let cap = total_cap.max(1);
        QuarantineLedger::with_caps(cap.min(QuarantineLedger::PER_CC_CAP.max(cap)), cap)
    }

    /// 저장해 둔 항목들에서 (규칙에 안 맞는 항목은 버린다)
    pub fn from_entries(entries: Vec<QuarantineEntry>) -> QuarantineLedger {
        QuarantineLedger::from_entries_with_caps(
            entries,
            QuarantineLedger::PER_CC_CAP,
            QuarantineLedger::TOTAL_CAP,
        )
    }

    /// Reload saved entries using explicit `(per_cc_cap, total_cap)` bounds so callers storing
    /// more than the default 1,024 entries do not suffer silent truncation.
    pub fn from_entries_with_caps(
        entries: Vec<QuarantineEntry>,
        per_cc_cap: usize,
        total_cap: usize,
    ) -> QuarantineLedger {
        let mut l = QuarantineLedger::with_caps(per_cc_cap, total_cap);
        for e in entries {
            l.record(e);
        }
        l
    }

    /// 저장할 항목들
    pub fn entries(&self) -> &[QuarantineEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 대체 한 건을 적는다. 같은 `(cc, old_id, new_id)` 는 고쳐 적되 이미 보낸 상태(ReportedServer)는 되돌리지 않는다.
    /// `cc` 가 비었거나, 자기 자신이거나, 시각이 단조롭지 않으면(`new_at_ms <= old_at_ms`) 받지 않고 false.
    pub fn record(&mut self, entry: QuarantineEntry) -> bool {
        if entry.cc.is_empty() || entry.old_id == entry.new_id || entry.new_at_ms <= entry.old_at_ms {
            return false;
        }
        match self.entries.iter_mut().find(|e| e.cc == entry.cc && e.old_id == entry.old_id && e.new_id == entry.new_id) {
            Some(e) => {
                let sync_state = if e.sync_state == QuarantineSyncState::ReportedServer { e.sync_state } else { entry.sync_state };
                *e = QuarantineEntry { sync_state, ..entry };
            }
            None => self.entries.push(entry),
        }
        self.enforce_caps();
        true
    }

    fn evict_oldest(&mut self, cc: Option<&str>) {
        let victim = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| cc.is_none_or(|c| e.cc == c))
            .min_by_key(|(k, e)| (e.recorded_at_ms, *k))
            .map(|(k, _)| k);
        if let Some(k) = victim {
            self.entries.remove(k);
        }
    }

    fn enforce_caps(&mut self) {
        loop {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for e in &self.entries {
                *counts.entry(e.cc.as_str()).or_default() += 1;
            }
            let Some(cc) = counts.into_iter().find(|(_, n)| *n > self.per_cc_cap).map(|(c, _)| c.to_string()) else { break };
            self.evict_oldest(Some(&cc));
        }
        while self.entries.len() > self.total_cap {
            self.evict_oldest(None);
        }
    }

    /// 간선이 지금 유효한가: 대체한 쪽이 무너지지 않았거나, 무너졌어도 그것이 다시 유효하게 대체되어 있으면(사슬)
    fn edge_valid<F: Fn(&str) -> Standing>(&self, e: &QuarantineEntry, standing: &F, depth: usize) -> bool {
        if depth > MAX_CHAIN || !standing(&e.new_id).fallen() {
            return true;
        }
        self.entries
            .iter()
            .filter(|x| x.cc == e.cc && x.old_id == e.new_id && x.new_at_ms > x.old_at_ms)
            .any(|x| self.edge_valid(x, standing, depth + 1))
    }

    /// `old_id` 를 지금 유효하게 대체한 항목 (자가 복권 내장): 가장 이른 새 평가부터. 없으면 None — 격리가 아니다(복권).
    pub fn active_superseder(&self, cc: &str, old_id: &str, standing: impl Fn(&str) -> Standing) -> Option<&QuarantineEntry> {
        let mut cands: Vec<&QuarantineEntry> = self.entries.iter().filter(|e| e.cc == cc && e.old_id == old_id).collect();
        cands.sort_by(|a, b| a.new_at_ms.cmp(&b.new_at_ms).then_with(|| a.new_id.cmp(&b.new_id)));
        cands.into_iter().find(|e| self.edge_valid(e, &standing, 0))
    }

    /// 로컬 판정에 `superseded` 를 켤까 (격리 중인가)
    pub fn is_quarantined(&self, cc: &str, id: &str, standing: impl Fn(&str) -> Standing) -> bool {
        self.active_superseder(cc, id, standing).is_some()
    }

    /// 사슬 끝: 유효한 간선을 따라간 마지막 평가 (A ≺ B ≺ C 에서 A → C). 격리가 아니면 자기 자신.
    pub fn final_version(&self, cc: &str, id: &str, standing: impl Fn(&str) -> Standing) -> String {
        let mut cur = id.to_string();
        for _ in 0..MAX_CHAIN {
            match self.active_superseder(cc, &cur, &standing) {
                Some(e) => cur = e.new_id.clone(),
                None => break,
            }
        }
        cur
    }

    /// 보낼 차례인 대체(DeferredPending) 가운데 두 평가가 모두 stored 이고(`stored_id` 가 서버 id 를 준다) 간선이 아직 유효한 것:
    /// 서버 id 쌍 `(old, new)` 를 돌려주고 ReportedServer 로 바꾼다 — 같은 쌍은 한 번만 나온다. 보내다 실패하면
    /// [`QuarantineLedger::requeue_server_report`].
    pub fn drain_ready_server_reports(&mut self, stored_id: impl Fn(&str) -> Option<String>, standing: impl Fn(&str) -> Standing) -> Vec<(String, String)> {
        let ready: Vec<usize> = (0..self.entries.len())
            .filter(|&k| self.entries[k].sync_state == QuarantineSyncState::DeferredPending && self.edge_valid(&self.entries[k], &standing, 0))
            .collect();
        let mut out = Vec::new();
        for k in ready {
            if let (Some(o), Some(n)) = (stored_id(&self.entries[k].old_id), stored_id(&self.entries[k].new_id)) {
                self.entries[k].sync_state = QuarantineSyncState::ReportedServer;
                out.push((o, n));
            }
        }
        out
    }

    /// 보내지 못한 보고를 다시 DeferredPending 으로 (오프라인 등). 그 항목이 ReportedServer 였으면 true.
    pub fn requeue_server_report(&mut self, cc: &str, old_id: &str, new_id: &str) -> bool {
        match self.entries.iter_mut().find(|e| e.cc == cc && e.old_id == old_id && e.new_id == new_id && e.sync_state == QuarantineSyncState::ReportedServer) {
            Some(e) => {
                e.sync_state = QuarantineSyncState::DeferredPending;
                true
            }
            None => false,
        }
    }
}

/// Identifies paragraph indices that MUST be pinned open in Pass 1 during long-document
/// perforated embedding so `condition_disjoint` and `PolarityFlip` clauses have 0% omission:
/// 1. Paragraphs containing negations (`-nms::analyze::negation_counts` or `predicate_polarity`),
/// 2. Paragraphs containing condition qualifiers (`condition_qualifiers`), numeric/unit values (`claim`), or identifiers,
/// 3. Header / title / table-header paragraphs.
pub fn pinned_paragraph_indices(paragraphs: &[&str], parsed_sentences: &[Vec<Sentence>]) -> Vec<usize> {
    let mut pinned = BTreeSet::new();
    for (idx, &para) in paragraphs.iter().enumerate() {
        let trimmed = para.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 1) Heading / title / table-header rule
        let is_heading = trimmed.starts_with('#')
            || trimmed.starts_with('|')
            || trimmed.contains("단위:")
            || trimmed.contains("Unit:")
            || (idx == 0 && trimmed.chars().count() <= 120);
        if is_heading {
            pinned.insert(idx);
            continue;
        }
        // 2) -nms negation detection on raw paragraph text
        let mut norm = premium_discount_nms::normalize::normalize(trimmed);
        premium_discount_nms::analyze::Analyzer::analyze(
            &premium_discount_nms::analyze::RuleAnalyzer,
            &mut norm,
        );
        let negs = premium_discount_nms::analyze::negation_counts(&norm.words);
        if negs.iter().any(|&c| c > 0) {
            pinned.insert(idx);
            continue;
        }
        // 3) -relay condition qualifiers, polarity, numeric/unit claims, identifiers
        let mut hit = false;
        if let Some(sents) = parsed_sentences.get(idx) {
            for s in sents {
                let c = claim(s);
                if c.negated || !c.qualifiers.is_empty() || !c.values.is_empty() {
                    hit = true;
                    break;
                }
                if s.tokens.iter().any(|t| is_identifier(t.text.trim())) {
                    hit = true;
                    break;
                }
            }
        }
        if !hit {
            // Raw-token fallback for unparsed paragraphs (lightweight whitespace split -> Sentence)
            let tokens: Vec<Token> = trimmed
                .split_whitespace()
                .map(|w| Token {
                    text: w.to_string(),
                    lemma: w.to_lowercase(),
                    upos: if w.chars().any(|c| c.is_ascii_digit()) { "NUM" } else { "NOUN" }.to_string(),
                    deprel: "dep".to_string(),
                    head: 0,
                })
                .collect();
            let synth = Sentence { text: trimmed.to_string(), tokens };
            let c = claim(&synth);
            if c.negated
                || !c.qualifiers.is_empty()
                || !c.values.is_empty()
                || synth.tokens.iter().any(|t| is_identifier(t.text.trim()))
            {
                hit = true;
            }
        }
        if hit {
            pinned.insert(idx);
        }
    }
    pinned.into_iter().collect()
}

/// Convenience wrapper when caller has only raw paragraph texts without pre-parsed UD sentences.
pub fn pinned_paragraph_indices_text(paragraphs: &[&str]) -> Vec<usize> {
    pinned_paragraph_indices(paragraphs, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_nms::embed::HashedNgramEmbedder;

    fn tok(text: &str, lemma: &str, upos: &str, deprel: &str, head: usize) -> Token {
        Token {
            text: text.into(),
            lemma: lemma.into(),
            upos: upos.into(),
            deprel: deprel.into(),
            head,
        }
    }

    /// "Malenia waterfowl dance damage reduced by 20 %" 을 손으로 분석한 문장 (값만 바꿔 쓴다)
    fn patch_note(value: &str) -> Sentence {
        Sentence {
            text: format!("Malenia waterfowl dance damage reduced by {value} % in patch 1.04"),
            tokens: vec![
                tok("Malenia", "Malenia", "PROPN", "compound", 4),
                tok("waterfowl", "waterfowl", "NOUN", "compound", 3),
                tok("dance", "dance", "NOUN", "compound", 4),
                tok("damage", "damage", "NOUN", "nsubj", 5),
                tok("reduced", "reduce", "VERB", "root", 0),
                tok("by", "by", "ADP", "case", 7),
                tok(value, value, "NUM", "obl", 5),
                tok("%", "%", "SYM", "nmod", 7),
                tok("in", "in", "ADP", "case", 10),
                tok("patch", "patch", "NOUN", "obl", 5),
                tok("1.04", "1.04", "NUM", "nummod", 10),
            ],
        }
    }

    #[test]
    fn keys_cover_names_identifiers_symbols_compounds_and_quotes() {
        let s = Sentence {
            text: "wgpu 가 \"Surface lost\" 오류 E0425 를 낸다 (Malenia 보스)".into(),
            tokens: vec![
                tok("wgpu", "wgpu", "X", "nsubj", 6),
                tok("Surface lost", "surface lost", "NOUN", "obj", 6),
                tok("E0425", "E0425", "NOUN", "obj", 6),
                tok("Malenia", "Malenia", "PROPN", "compound", 5),
                tok("보스", "보스", "NOUN", "obl", 6),
                tok("낸다", "내다", "VERB", "root", 0),
            ],
        };
        let keys = relay_keys(&s);
        let has = |k: &str, kind: KeyKind| keys.iter().any(|x| x.key == k && x.kind == kind);
        assert!(has("wgpu", KeyKind::Symbol), "{keys:?}");
        assert!(has("e0425", KeyKind::Identifier), "{keys:?}");
        assert!(has("malenia", KeyKind::Proper), "{keys:?}");
        assert!(has("Surface lost", KeyKind::Quoted), "{keys:?}");
        assert!(has("malenia 보스", KeyKind::Compound), "{keys:?}");
        assert_eq!(quoted_spans("「보스」와 “Malenia” 그리고 'E0425'"), vec!["보스", "Malenia", "E0425"]);
    }

    #[test]
    fn technical_tokens_stay_whole_and_name_their_family() {
        let got: Vec<(String, String)> = technical_tokens(
            "빌드는 Ctrl+Shift+B, 실행은 Alt+F5. cargo build -O2로 --release -j 1 --jobs=4 그리고 C 열, D열의 값은 10^4; row 3, a row of x",
        )
        .into_iter()
        .map(|t| (t.canonical, t.family))
        .collect();
        let want: Vec<(String, String)> = [
            ("ctrl+shift+b", "chord"),
            ("alt+f5", "chord"),
            ("-O2", "-O"),
            ("--release", "--release"),
            ("-j", "-j"),
            ("--jobs=4", "--jobs"),
            ("col:C", "col"),
            ("col:D", "col"),
            ("10^4", "10^"),
            ("row:3", "row"),
        ]
        .iter()
        .map(|(c, f)| (c.to_string(), f.to_string()))
        .collect();
        assert_eq!(got, want);
        for plain in ["-3", "C++", "E0425", "-", "--", "A+B", "v1.4", "3,000원", "x^2", "-이다", "re-run"] {
            assert!(technical_token(plain).is_none(), "{plain}");
        }
        assert_eq!(technical_token("-O2").unwrap().canonical, "-O2", "옵션은 대소문자를 지킨다");
        assert_ne!(technical_token("-O2"), technical_token("-o2"));
    }

    /// "Build release with -O2" 를 손으로 분석한 문장 (옵션만 바꿔 쓴다)
    fn flag_tip(flag: &str) -> Sentence {
        Sentence {
            text: format!("Build release with {flag}"),
            tokens: vec![
                tok("Build", "build", "VERB", "root", 0),
                tok("release", "release", "NOUN", "obj", 1),
                tok("with", "with", "ADP", "case", 4),
                tok(flag, flag, "X", "obl", 1),
            ],
        }
    }

    #[test]
    fn a_changed_command_line_flag_supersedes_the_old_tip() {
        let e = HashedNgramEmbedder::default();
        let cfg = RelayConfig::default();
        let old = Record { sentences: vec![flag_tip("-O2")], at_ms: 1_000 };
        let new = Record { sentences: vec![flag_tip("-O3")], at_ms: 2_000 };
        // 주제 키는 계열 하나, 정규 꼴은 값
        assert_eq!(relay_keys(&old.sentences[0]), vec![RelayKey { key: "-O".into(), kind: KeyKind::TechnicalToken }]);
        assert_eq!(claim(&old.sentences[0]).values, vec!["-O2".to_string()]);
        let all: Vec<RelayKey> = [&old, &new].iter().flat_map(|r| r.sentences.iter().flat_map(relay_keys)).collect();
        let keys = cluster_keys(&all, &e, &cfg);
        let (rel, cmp) = compare(&old, &new, &keys, &e, &cfg);
        match rel {
            Relation::Supersedes { changes, containment } => {
                assert_eq!((changes[0].old_values.clone(), changes[0].new_values.clone()), (vec!["-O2".to_string()], vec!["-O3".to_string()]));
                assert!(containment >= 0.99, "{containment}");
            }
            other => panic!("{other:?} {cmp:?}"),
        }
    }

    #[test]
    fn checkpoints_keep_short_flags_and_column_references() {
        let meta = CheckpointMeta {
            tab_title: Some("Terminal - cargo build -j 1 -O2"),
            screen_label: Some("C 열 값 확인"),
            badge_labels: &["Ctrl+B"],
            ..CheckpointMeta::default()
        };
        let keys = lift_checkpoint_relay_keys(&meta);
        let tech: Vec<&str> = keys.iter().filter(|k| k.kind == KeyKind::TechnicalToken).map(|k| k.key.as_str()).collect();
        for want in ["-j", "-O2", "col:C", "ctrl+b"] {
            assert!(tech.contains(&want), "{want} in {keys:?}");
        }
        let hidden = CheckpointMeta { sensitive_or_incognito: true, ..meta };
        assert!(lift_checkpoint_relay_keys(&hidden).is_empty());
    }

    #[test]
    fn pronouns_bridge_only_paired_recent_switches_sharing_a_precise_key() {
        use WorkSurface::*;
        let err = [RelayKey { key: "e0425".into(), kind: KeyKind::Identifier }];
        let topic = [RelayKey { key: "boss".into(), kind: KeyKind::Proper }];
        assert!(anaphora_bridge_allowed(Terminal, Ide, 9_000, &err));
        assert!(anaphora_bridge_allowed(Docs, Browser, ANAPHORA_BRIDGE_MS, &err));
        assert!(!anaphora_bridge_allowed(Terminal, Ide, ANAPHORA_BRIDGE_MS + 1, &err), "too late");
        assert!(!anaphora_bridge_allowed(Terminal, Browser, 1_000, &err), "not a paired switch");
        assert!(!anaphora_bridge_allowed(Terminal, Ide, 1_000, &topic), "a loose topic name is not a precise key");
        assert!(!anaphora_bridge_allowed(Terminal, Ide, 1_000, &[]), "nothing shared keeps the cut at 0.0");
        assert!(!anaphora_bridge_allowed(Other, Ide, 1_000, &err));
    }

    #[test]
    fn case_edges_are_reversible_and_their_grades_serialize_in_snake_case() {
        let a = entity_key_u64("  Doc-0042 ");
        assert_eq!(a, entity_key_u64("doc-0042"), "trimmed and lower-cased");
        assert_ne!(a, entity_key_u64("doc-0043"));
        let e = CaseEdge { from: a, to: entity_key_u64("doc-0043"), role: EdgeRole::CaseBinding, trust: EdgeTrust::FaultTracingOnly };
        assert_eq!(e.reversed().reversed(), e);
        assert_eq!(e.undirected(), e.reversed().undirected());
        assert!(EdgeRole::CaseBinding.merges_cases() && !EdgeRole::CarrierTraversal.merges_cases());
        assert!(EdgeTrust::Certified.may_auto_approve() && !EdgeTrust::FaultTracingOnly.may_auto_approve());
        assert_eq!(serde_json::to_string(&EdgeTrust::FaultTracingOnly).unwrap(), "\"fault_tracing_only\"");
        assert_eq!(serde_json::to_string(&EdgeRole::CarrierTraversal).unwrap(), "\"carrier_traversal\"");
        let back: CaseEdge = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn claims_keep_values_with_their_units() {
        let c = claim(&patch_note("20"));
        assert!(c.core.contains("damage") && c.core.contains("reduce"), "{c:?}");
        assert!(c.values.contains(&"20%".to_string()), "{c:?}");
        assert!(c.values.contains(&"1.04".to_string()), "{c:?}");
        assert!(c.qualifiers.is_empty() && !c.negated, "{c:?}");
    }

    #[test]
    fn a_newer_note_with_a_changed_value_supersedes_the_old_one() {
        let e = HashedNgramEmbedder::default();
        let cfg = RelayConfig::default();
        let old = Record { sentences: vec![patch_note("20")], at_ms: 1_000 };
        let new = Record { sentences: vec![patch_note("15")], at_ms: 2_000 };
        let keys = cluster_keys(&[old.clone(), new.clone()].iter().flat_map(|r| r.sentences.iter().flat_map(relay_keys)).collect::<Vec<_>>(), &e, &cfg);
        let (rel, cmp) = compare(&old, &new, &keys, &e, &cfg);
        match rel {
            Relation::Supersedes { changes, containment } => {
                assert_eq!(changes.len(), 1);
                assert!(changes[0].old_values.contains(&"20%".to_string()) && changes[0].new_values.contains(&"15%".to_string()));
                assert_eq!(changes[0].kind, ChangeKind::ValueChanged);
                assert!(containment >= 0.99, "{containment}");
            }
            other => panic!("{other:?} {cmp:?}"),
        }
        // 같은 내용을 다시 쓰면 중복, 옛 시각이면 대체가 아님
        assert!(matches!(compare(&old, &Record { at_ms: 3_000, ..old.clone() }, &keys, &e, &cfg).0, Relation::Duplicate { .. }));
        assert!(!matches!(compare(&new, &old, &keys, &e, &cfg).0, Relation::Supersedes { .. }), "older notes never supersede");
        // 다른 이야기
        let other = Record {
            sentences: vec![Sentence {
                text: "Inventory potions are in the second bag".into(),
                tokens: vec![tok("potions", "potion", "NOUN", "nsubj", 0)],
            }],
            at_ms: 4_000,
        };
        assert!(matches!(compare(&old, &other, &keys, &e, &cfg).0, Relation::Unrelated { .. }));
    }

    #[test]
    fn identical_keys_cluster_and_identifiers_never_merge_by_look() {
        let e = HashedNgramEmbedder::default();
        let cfg = RelayConfig::default();
        let k = |key: &str, kind| RelayKey { key: key.into(), kind };
        let c = cluster_keys(&[k("e0425", KeyKind::Identifier), k("e0426", KeyKind::Identifier), k("malenia", KeyKind::Proper)], &e, &cfg);
        assert_eq!(c.canon("e0425"), "e0425");
        assert_eq!(c.canon("e0426"), "e0426");
        assert_eq!(c.canon("unknown"), "unknown");
        assert!(c.threshold >= cfg.key_cos_floor);
    }

    /// 다국어 모델(granite) 흉내: 같은 뜻의 한 · 영 낱말을 같은 축에 둔다 (모델 없이 CR-07 을 보는 시험용)
    struct Bilingual;

    impl TextEmbedder for Bilingual {
        fn dim(&self) -> usize {
            8
        }
        fn embed(&self, text: &str) -> Vec<f32> {
            const AXES: [&[&str]; 6] = [&["말레니아", "malenia"], &["물새", "waterfowl"], &["난무", "dance"], &["피해", "damage"], &["감소", "reduce"], &["회피", "dodge"]];
            let mut v = vec![0.0; 8];
            for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
                if let Some(i) = AXES.iter().position(|a| a.iter().any(|p| w.starts_with(p))) {
                    v[i] += 1.0;
                }
            }
            v
        }
        fn space(&self) -> &str {
            "test-bilingual"
        }
    }

    /// "말레니아의 물새 난무 피해가 {value} 감소했다 ." (Stanza ko 모양)
    fn korean_damage(value: &str, at_ms: i64) -> Record {
        Record {
            sentences: vec![Sentence {
                text: format!("말레니아의 물새 난무 피해가 {value} 감소했다 ."),
                tokens: vec![
                    tok("말레니아의", "말레니아+의", "PROPN", "nmod", 4),
                    tok("물새", "물새", "NOUN", "compound", 3),
                    tok("난무", "난무", "NOUN", "compound", 4),
                    tok("피해가", "피해+가", "NOUN", "nsubj", 6),
                    tok(value, value, "NUM", "nummod", 6),
                    tok("감소했다", "감소+하+었+다", "VERB", "root", 0),
                    tok(".", ".", "PUNCT", "punct", 6),
                ],
            }],
            at_ms,
        }
    }

    /// "Malenia waterfowl dance damage reduced by {value} %"
    fn english_damage(value: &str, at_ms: i64) -> Record {
        let mut s = patch_note(value);
        s.text = format!("Malenia waterfowl dance damage reduced by {value} %");
        s.tokens.truncate(8);
        Record { sentences: vec![s], at_ms }
    }

    #[test]
    fn cr07_cross_language_cross_scope_patch_is_confirmed_locally_without_a_server_report() {
        let cfg = RelayConfig::default();
        let old = korean_damage("20%", 1_000);
        let new = english_damage("15", 2_000);
        let entries = [
            Entry { id: "ko-old", cc: "eldenring.exe", to: None, record: &old },
            Entry { id: "en-new", cc: "eldenring.exe", to: Some("L1"), record: &new },
        ];
        let report = curate_cross_scope(&entries, &Bilingual, &cfg);
        assert_eq!(
            report.state("ko-old"),
            SupersedeState::Confirmed { new_id: "en-new".into(), value_diff: Some(("20%".into(), "15%".into())) },
            "{report:?}"
        );
        assert!(report.recommendable("en-new") && !report.recommendable("ko-old"));
        let s = &report.supersessions[0];
        assert!(!s.same_scope(), "global old guide vs checkpoint new guide");
        assert_eq!((s.old_to.as_deref(), s.new_to.as_deref()), (None, Some("L1")));
        assert_eq!(report.final_version("ko-old"), "en-new");
        // 서버 보고용 판정은 같은 (cc, to) 만 본다: to 가 달라 대체 없음
        assert!(supersessions(&entries, &Bilingual, &cfg).is_empty());
        // 내 옛 글이라도 to 가 달라 pd_supersede 는 부르지 않는다 (서버 400 방지)
        let facts = SupersedeFacts { old_mine: true, signed_in: true, sharing_consent: true, old_stored: true, new_stored: true, ..Default::default() };
        let sync = supersede_sync(s.old_to.as_deref(), s.new_to.as_deref(), &facts);
        assert_eq!(sync, SupersedeSync::LocalOnly(LocalOnlyReason::CrossScope));
        let mut ledger = QuarantineLedger::new();
        assert!(ledger.record(QuarantineEntry::from_supersession(s, sync.into(), 5_000)));
        assert_eq!(ledger.entries()[0].sync_state, QuarantineSyncState::LocalOnlyCrossScope);
        assert!(ledger.is_quarantined("eldenring.exe", "ko-old", |_| Standing::default()));
        assert!(ledger.drain_ready_server_reports(|id| Some(format!("srv-{id}")), |_| Standing::default()).is_empty(), "0 pd_supersede calls");
    }

    /// 손으로 분석한 한국어 문장: (글, 원형, 품사, 관계, 머리)
    fn ko(parts: &[(&str, &str, &str, &str, usize)], at_ms: i64) -> Record {
        let tokens: Vec<Token> = parts.iter().map(|(t, l, u, d, h)| tok(t, l, u, d, *h)).collect();
        let text = tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        Record { sentences: vec![Sentence { text, tokens }], at_ms }
    }

    /// "말레니아 {phase} 감소율 {value}" (phase 가 비면 뺀다)
    fn phase_note(phase: &str, value: &str, at_ms: i64) -> Record {
        let mut parts = vec![("말레니아", "말레니아", "PROPN", "nmod", 3)];
        if !phase.is_empty() {
            parts.push((phase, phase, "NOUN", "nmod", 3));
        }
        let rate = parts.len() + 1;
        parts.push(("감소율", "감소율", "NOUN", "nsubj", rate + 1));
        parts.push((value, value, "NUM", "root", 0));
        // 머리 번호를 실제 자리로 (말레니아 · phase → 감소율, 감소율 → 값)
        let mut r = ko(&parts, at_ms);
        let n = r.sentences[0].tokens.len();
        for (i, t) in r.sentences[0].tokens.iter_mut().enumerate() {
            t.head = if i + 2 < n { n - 1 } else if i + 2 == n { n } else { 0 };
        }
        r
    }

    fn relation(old: &Record, new: &Record, e: &dyn TextEmbedder) -> (Relation, Comparison) {
        let cfg = RelayConfig::default();
        let all: Vec<RelayKey> = old.sentences.iter().chain(&new.sentences).flat_map(relay_keys).collect();
        let keys = cluster_keys(&all, e, &cfg);
        compare(old, new, &keys, e, &cfg)
    }

    #[test]
    fn cr08_phase_qualifiers_coexist_and_negation_supersedes() {
        let e = HashedNgramEmbedder::default();
        // 케이스 A: "1페이즈 20%" vs "2페이즈 15%" → 둘 다 맞는 말: 의미 연결, 양쪽 Active
        let p1 = phase_note("1페이즈", "20%", 1_000);
        let p2 = phase_note("2페이즈", "15%", 2_000);
        let (c1, c2) = (claim(&p1.sentences[0]), claim(&p2.sentences[0]));
        assert_eq!(c1.qualifiers, BTreeSet::from(["phase:1".to_string()]), "{c1:?}");
        assert_eq!(c1.values, vec!["20%".to_string()], "the phase is a qualifier, not a value: {c1:?}");
        assert!(condition_disjoint(&c1, &c2));
        let (rel, cmp) = relation(&p1, &p2, &e);
        assert!(matches!(rel, Relation::SemanticLink { .. }), "{rel:?} {cmp:?}");
        assert_eq!(cmp.coexisting, vec![(0, 0)]);
        let entries = [
            Entry { id: "p1", cc: "game", to: Some("malenia"), record: &p1 },
            Entry { id: "p2", cc: "game", to: None, record: &p2 },
        ];
        let report = curate_cross_scope(&entries, &e, &RelayConfig::default());
        assert_eq!((report.state("p1"), report.state("p2")), (SupersedeState::Active, SupersedeState::Active), "{report:?}");
        assert!(report.links.iter().any(|l| l.coexisting && l.old_id == "p1" && l.new_id == "p2"), "{report:?}");
        // 같은 페이즈끼리 · 페이즈가 없는 것끼리는 대체가 그대로 된다 (가드가 막은 것은 조건이 다른 경우뿐)
        assert!(matches!(relation(&phase_note("1페이즈", "20%", 1), &phase_note("1페이즈", "15%", 2), &e).0, Relation::Supersedes { .. }));
        assert!(matches!(relation(&phase_note("", "20%", 1), &phase_note("", "15%", 2), &e).0, Relation::Supersedes { .. }));

        // 케이스 B: "물새 난무는 점프로 회피 가능" → "… 회피 불가": 중복이 아니라 부정 반전 대체
        let dodge = |pred: &str, at| {
            ko(
                &[
                    ("물새", "물새", "NOUN", "compound", 2),
                    ("난무는", "난무+는", "NOUN", "dislocated", 5),
                    ("점프로", "점프+로", "NOUN", "obl", 5),
                    ("회피", "회피", "NOUN", "obj", 5),
                    (pred, pred, "NOUN", "root", 0),
                ],
                at,
            )
        };
        let (can, cannot) = (dodge("가능", 1_000), dodge("불가", 2_000));
        let (rel, cmp) = relation(&can, &cannot, &e);
        match &rel {
            Relation::Supersedes { changes, .. } => {
                assert_eq!(changes[0].kind, ChangeKind::PolarityFlip);
                assert_eq!((changes[0].old_values.clone(), changes[0].new_values.clone()), (vec!["가능".to_string()], vec!["불가".to_string()]));
                assert_eq!(value_diff(changes), Some(("가능".into(), "불가".into())));
            }
            other => panic!("{other:?} {cmp:?}"),
        }
        let entries = [
            Entry { id: "can", cc: "game", to: Some("malenia"), record: &can },
            Entry { id: "cannot", cc: "game", to: Some("malenia"), record: &cannot },
        ];
        let report = curate_cross_scope(&entries, &e, &RelayConfig::default());
        assert!(matches!(report.state("can"), SupersedeState::Confirmed { ref new_id, .. } if new_id == "cannot"), "{report:?}");
        assert!(report.duplicates.is_empty());
        // 서버 보고용 판정도 같다 (같은 cc · to)
        let s = supersessions(&entries, &e, &RelayConfig::default());
        assert_eq!((s.len(), s[0].changes[0].kind), (1, ChangeKind::PolarityFlip));
    }

    #[test]
    fn negation_flip_on_a_bare_sentence_and_in_english() {
        let e = HashedNgramEmbedder::default();
        // 주제어 없이 "점프로 회피 가능/불가": 복합 명사 키 "회피 가능" · "회피 불가" 는 같은 주제다
        let bare = |pred: &str, at| ko(&[("점프로", "점프+로", "NOUN", "obl", 3), ("회피", "회피", "NOUN", "compound", 3), (pred, pred, "NOUN", "root", 0)], at);
        let (rel, cmp) = relation(&bare("가능", 1), &bare("불가", 2), &e);
        assert!(matches!(rel, Relation::Supersedes { .. }), "{rel:?} {cmp:?}");
        assert!(cmp.align_threshold <= 0.5 + 1e-6, "hash floor: {cmp:?}");
        // 영어 not: 서술어 글에 부정이 붙는다
        let en = |negated: bool, at| {
            let mut tokens = vec![
                tok("Waterfowl", "waterfowl", "NOUN", "compound", 2),
                tok("dance", "dance", "NOUN", "nsubj:pass", 0),
                tok("can", "can", "AUX", "aux", 0),
            ];
            if negated {
                tokens.push(tok("not", "not", "PART", "advmod", 0));
            }
            tokens.extend([tok("be", "be", "AUX", "aux:pass", 0), tok("dodged", "dodge", "VERB", "root", 0), tok("by", "by", "ADP", "case", 0), tok("jumping", "jumping", "NOUN", "obl", 0)]);
            let root = tokens.iter().position(|t| t.deprel == "root").unwrap() + 1;
            for t in tokens.iter_mut() {
                if t.head == 0 && t.deprel != "root" {
                    t.head = if t.deprel == "case" { root + 2 } else { root };
                }
            }
            let text = tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
            Record { sentences: vec![Sentence { text, tokens }], at_ms: at }
        };
        let (old, new) = (en(false, 1), en(true, 2));
        assert!(!claim(&old.sentences[0]).negated && claim(&new.sentences[0]).negated);
        match relation(&old, &new, &e).0 {
            Relation::Supersedes { changes, .. } => {
                assert_eq!(changes[0].kind, ChangeKind::PolarityFlip);
                assert_eq!(value_diff(&changes), Some(("dodged".into(), "not dodged".into())));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn negation_outside_the_predicate_core_and_double_negation_do_not_flip() {
        // 관형절의 "안" 은 서술 핵의 부정이 아니다: "안 맞는 무기로도 회피 가능"
        let s = ko(&[("안", "안", "ADV", "advmod", 2), ("맞는", "맞+는", "VERB", "acl", 3), ("무기로도", "무기+로+도", "NOUN", "obl", 5), ("회피", "회피", "NOUN", "obj", 5), ("가능", "가능", "NOUN", "root", 0)], 1);
        assert!(!claim(&s.sentences[0]).negated);
        // 핵에 붙은 부정 둘은 서로 지운다: "회피 못 하지 않는다"
        let s = ko(&[("회피", "회피", "NOUN", "obj", 4), ("못", "못", "ADV", "advmod", 4), ("하지", "하+지", "VERB", "xcomp", 4), ("않는다", "않+는다", "AUX", "root", 0)], 1);
        assert!(!claim(&s.sentences[0]).negated, "{:?}", claim(&s.sentences[0]));
        // "없다" 는 "있다" 의 부정: 핵심 · 값은 긍정 꼴로
        let s = ko(&[("회피", "회피", "NOUN", "nsubj", 2), ("없다", "없+다", "ADJ", "root", 0)], 1);
        let c = claim(&s.sentences[0]);
        assert!(c.negated && c.values == vec!["있".to_string()], "{c:?}");
    }

    #[test]
    fn condition_qualifiers_cover_sections_modes_difficulty_and_platforms() {
        let q = |parts: &[(&str, &str)]| {
            let tokens: Vec<Token> = parts.iter().map(|(t, u)| tok(t, t, u, "nmod", 0)).collect();
            condition_qualifiers(&Sentence { text: String::new(), tokens }).0.into_iter().collect::<Vec<_>>()
        };
        assert_eq!(q(&[("2페이즈에서", "NOUN")]), vec!["phase:2"]);
        assert_eq!(q(&[("2페는", "NOUN")]), vec!["phase:2"]);
        assert_eq!(q(&[("phase", "NOUN"), ("02", "NUM")]), vec!["phase:2"]);
        assert_eq!(q(&[("second", "ADJ"), ("phase", "NOUN")]), vec!["phase:2"]);
        assert_eq!(q(&[("1막", "NOUN")]), vec!["act:1"]);
        assert_eq!(q(&[("막페", "NOUN")]), vec!["phase:last"]);
        assert_eq!(q(&[("PvP", "PROPN"), ("솔로로", "NOUN")]), vec!["mode:pvp", "party:solo"]);
        assert_eq!(q(&[("하드", "NOUN")]), vec!["difficulty:hard"]);
        assert_eq!(q(&[("영웅", "NOUN"), ("난이도", "NOUN")]), vec!["difficulty:heroic"]);
        assert_eq!(q(&[("노말모드", "NOUN")]), vec!["difficulty:normal"]);
        assert_eq!(q(&[("모바일", "NOUN"), ("PS5", "PROPN")]), vec!["platform:mobile", "platform:ps5"]);
        // 조건이 아닌 것: "일반 공격", "hard to dodge", 패치 번호, 금액
        assert!(q(&[("일반", "NOUN"), ("공격", "NOUN")]).is_empty());
        assert!(q(&[("hard", "ADJ"), ("to", "PART"), ("dodge", "VERB")]).is_empty());
        assert!(q(&[("patch", "NOUN"), ("1.04", "NUM")]).is_empty());
        assert!(q(&[("3,000원", "NOUN")]).is_empty());
    }

    #[test]
    fn effective_align_floor_lowers_only_the_hash_space() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.effective_align_floor(HASH_SPACE), 0.50);
        assert_eq!(cfg.effective_align_floor("granite-embedding-97m-multilingual-r2"), 0.75);
        assert_eq!(RelayConfig { align_cos_floor: 0.3, ..cfg }.effective_align_floor(HASH_SPACE), 0.3, "a lower explicit floor stays");
        assert_eq!(RelayConfig { hash_align_cos_floor: 1.0, ..cfg }.effective_align_floor(HASH_SPACE), 0.75, "adaptation can be turned off");
        assert_eq!(HashedNgramEmbedder::default().space(), HASH_SPACE);
    }

    #[test]
    fn value_diff_pairs_values_by_unit() {
        let ch = |o: &[&str], n: &[&str]| Change {
            old_sentence: 0,
            new_sentence: 0,
            core: vec![],
            old_values: o.iter().map(|s| s.to_string()).collect(),
            new_values: n.iter().map(|s| s.to_string()).collect(),
            kind: ChangeKind::ValueChanged,
        };
        assert_eq!(value_diff(&[ch(&["20%"], &["15%"])]), Some(("20%".into(), "15%".into())));
        // 단위 없는 패치 번호보다 단위가 같은 짝을 먼저
        assert_eq!(value_diff(&[ch(&["1.04", "20%"], &["1.05", "15%"])]), Some(("20%".into(), "15%".into())));
        assert_eq!(value_diff(&[ch(&["20%", "3초"], &["15%", "2초"])]), Some(("20%".into(), "15%".into())));
        assert_eq!(value_diff(&[ch(&["3,000원"], &["무료"])]), Some(("3,000원".into(), "무료".into())));
        assert_eq!(value_diff(&[ch(&["a", "b"], &["c", "d"])]), Some(("a, b".into(), "c, d".into())));
        assert_eq!(value_diff(&[ch(&["x"], &["x"])]), None);
        assert_eq!(value_diff(&[]), None);
    }

    #[test]
    fn supersede_sync_gate_checks_ownership_auth_scope_and_storage_in_order() {
        let mine = SupersedeFacts { new_mine: true, signed_in: true, sharing_consent: true, old_stored: true, new_stored: true, ..Default::default() };
        assert_eq!(supersede_sync(Some("t"), Some("t"), &mine), SupersedeSync::Report);
        assert_eq!(supersede_sync(None, None, &mine), SupersedeSync::Report, "both app-wide (to = None) is the same scope");
        assert_eq!(supersede_sync(Some("t"), Some("t"), &SupersedeFacts { new_stored: false, ..mine }), SupersedeSync::Defer);
        assert_eq!(supersede_sync(Some("t"), Some("u"), &mine), SupersedeSync::LocalOnly(LocalOnlyReason::CrossScope));
        assert_eq!(supersede_sync(Some("t"), Some("t"), &SupersedeFacts { sharing_consent: false, ..mine }), SupersedeSync::LocalOnly(LocalOnlyReason::SignedOut));
        let third = SupersedeFacts { new_mine: false, ..mine };
        assert_eq!(supersede_sync(Some("t"), Some("u"), &third), SupersedeSync::LocalOnly(LocalOnlyReason::ThirdParty), "ownership is checked first");
        for s in [SupersedeSync::Report, SupersedeSync::Defer] {
            assert_eq!(QuarantineSyncState::from(s), QuarantineSyncState::DeferredPending);
        }
        for st in [
            QuarantineSyncState::LocalOnlyCrossScope,
            QuarantineSyncState::DeferredPending,
            QuarantineSyncState::ReportedServer,
            QuarantineSyncState::ThirdPartyLocal,
            QuarantineSyncState::LocalOnlySignedOut,
        ] {
            assert_eq!(QuarantineSyncState::from_name(st.as_str()), Some(st));
        }
        assert_eq!(QuarantineSyncState::from_name("nope"), None);
    }

    fn entry(old: &str, new: &str, old_at: i64, new_at: i64, state: QuarantineSyncState) -> QuarantineEntry {
        QuarantineEntry {
            cc: "game".into(),
            old_id: old.into(),
            new_id: new.into(),
            old_to: None,
            new_to: None,
            old_at_ms: old_at,
            new_at_ms: new_at,
            value_diff: None,
            recorded_at_ms: new_at,
            sync_state: state,
        }
    }

    fn fallen() -> Standing {
        Standing { global_grade: Some(-1), global_evidence: 3.5, ..Default::default() }
    }

    #[test]
    fn chains_compress_to_the_final_version_and_heal_when_the_middle_or_end_falls() {
        let mut l = QuarantineLedger::new();
        assert!(l.record(entry("a", "b", 1, 2, QuarantineSyncState::ThirdPartyLocal)));
        assert!(l.record(entry("b", "c", 2, 3, QuarantineSyncState::ThirdPartyLocal)));
        let fine = |_: &str| Standing::default();
        assert_eq!(l.final_version("game", "a", fine), "c");
        assert!(l.is_quarantined("game", "a", fine) && l.is_quarantined("game", "b", fine) && !l.is_quarantined("game", "c", fine));
        // b 가 c 에 대체되어 로컬 discount 로 보여도(격리 때문) a ≺ b 는 유효하다: 사슬
        let b_down = |id: &str| if id == "b" { Standing { local_discount: true, ..Default::default() } } else { Standing::default() };
        assert!(l.is_quarantined("game", "a", b_down));
        // 끝(c)이 무너지면 b 가 복권되고 a 는 b 에 대체된 채로
        let c_down = |id: &str| if id == "c" { fallen() } else { Standing::default() };
        assert!(!l.is_quarantined("game", "b", c_down) && l.is_quarantined("game", "a", c_down));
        assert_eq!(l.final_version("game", "a", c_down), "b");
        // 근거가 모자란 전역 discount, 로컬 긍정 근거, 서버 superseded 는 무너진 것이 아니다
        let weak = |id: &str| if id == "c" { Standing { global_evidence: 2.0, ..fallen() } } else { Standing::default() };
        assert!(l.is_quarantined("game", "b", weak));
        let backed = |id: &str| if id == "c" { Standing { local_positive: true, ..fallen() } } else { Standing::default() };
        assert!(l.is_quarantined("game", "b", backed));
        let chained = |id: &str| if id == "c" { Standing { superseded: true, ..fallen() } } else { Standing::default() };
        assert!(l.is_quarantined("game", "b", chained));
        let gone = |id: &str| if id == "c" { Standing { gone: true, ..Default::default() } } else { Standing::default() };
        assert!(!l.is_quarantined("game", "b", gone));
    }

    #[test]
    fn the_ledger_rejects_time_travel_keeps_reported_state_and_caps_by_recency() {
        let mut l = QuarantineLedger::with_caps(2, 3);
        assert!(!l.record(entry("a", "b", 5, 5, QuarantineSyncState::ThirdPartyLocal)), "same time");
        assert!(!l.record(entry("a", "b", 6, 5, QuarantineSyncState::ThirdPartyLocal)), "older new");
        assert!(!l.record(entry("a", "a", 1, 2, QuarantineSyncState::ThirdPartyLocal)), "itself");
        assert!(l.record(entry("a", "b", 1, 2, QuarantineSyncState::DeferredPending)));
        let drained = l.drain_ready_server_reports(|id| Some(format!("srv-{id}")), |_| Standing::default());
        assert_eq!(drained, vec![("srv-a".to_string(), "srv-b".to_string())]);
        // 다시 적어도 보낸 상태는 그대로 (두 번 보내지 않음)
        assert!(l.record(entry("a", "b", 1, 2, QuarantineSyncState::DeferredPending)));
        assert_eq!(l.entries()[0].sync_state, QuarantineSyncState::ReportedServer);
        assert!(l.drain_ready_server_reports(|id| Some(id.to_string()), |_| Standing::default()).is_empty());
        // 보내지 못했으면 되돌려 다시 낸다
        assert!(l.requeue_server_report("game", "a", "b"));
        assert!(!l.requeue_server_report("game", "a", "b"));
        assert_eq!(l.drain_ready_server_reports(|id| Some(id.to_string()), |_| Standing::default()).len(), 1);
        // 상한: cc 마다 2건, 전체 3건 — 가장 오래 기록된 것부터 버린다
        l.record(entry("c", "d", 3, 4, QuarantineSyncState::ThirdPartyLocal));
        l.record(entry("e", "f", 5, 6, QuarantineSyncState::ThirdPartyLocal));
        assert_eq!(l.len(), 2);
        assert!(l.entries().iter().all(|e| e.old_id != "a"), "{:?}", l.entries());
        let mut other = entry("x", "y", 1, 2, QuarantineSyncState::ThirdPartyLocal);
        other.cc = "shop".into();
        l.record(other.clone());
        other.old_id = "z".into();
        other.recorded_at_ms = 9;
        l.record(other);
        assert_eq!(l.len(), 3);
        assert_eq!(QuarantineLedger::from_entries(l.entries().to_vec()).entries(), l.entries());
    }

    #[test]
    fn cn269_cross_cc_relay_federation_links_multilingual_names_with_zero_server_supersede() {
        struct CrossLangEmbedder;
        impl TextEmbedder for CrossLangEmbedder {
            fn dim(&self) -> usize {
                3
            }
            fn space(&self) -> &str {
                "granite-97m-r2-cls-v1"
            }
            fn embed(&self, text: &str) -> Vec<f32> {
                let lower = text.to_lowercase();
                if lower.contains("말레니아") || lower.contains("malenia") {
                    vec![0.99, 0.14, 0.0]
                } else if lower.contains("피해") || lower.contains("damage") {
                    vec![0.1, 0.99, 0.0]
                } else {
                    vec![0.0, 0.0, 1.0]
                }
            }
        }
        let emb = CrossLangEmbedder;
        let cfg = RelayConfig::default();

        // Checkpoint lifting (non-sentence UIA/vision metadata) & incognito block.
        let cp_keys = lift_checkpoint_relay_keys(&CheckpointMeta {
            vision_tag: Some("boss_arena"),
            screen_label: Some("말레니아 보스방"),
            badge_labels: &["안개문"],
            tab_title: None,
            active_topics: &["말레니아"],
            sensitive_or_incognito: false,
        });
        assert!(cp_keys.iter().any(|k| k.key == "말레니아"));
        assert!(lift_checkpoint_relay_keys(&CheckpointMeta {
            screen_label: Some("말레니아"),
            sensitive_or_incognito: true,
            ..Default::default()
        })
        .is_empty());

        let game_rec = Record {
            at_ms: 1_000,
            sentences: vec![Sentence {
                text: "말레니아 피해 20% 감소".into(),
                tokens: vec![
                    Token { text: "말레니아".into(), lemma: "말레니아".into(), upos: "PROPN".into(), deprel: "nsubj".into(), head: 2 },
                    Token { text: "피해".into(), lemma: "피해".into(), upos: "NOUN".into(), deprel: "obj".into(), head: 4 },
                    Token { text: "20%".into(), lemma: "20+%".into(), upos: "NUM".into(), deprel: "nummod".into(), head: 2 },
                    Token { text: "감소".into(), lemma: "감소".into(), upos: "VERB".into(), deprel: "root".into(), head: 0 },
                ],
            }],
        };
        let web_rec = Record {
            at_ms: 2_000,
            sentences: vec![Sentence {
                text: "Malenia damage 15% reduced".into(),
                tokens: vec![
                    Token { text: "Malenia".into(), lemma: "malenia".into(), upos: "PROPN".into(), deprel: "nsubj".into(), head: 2 },
                    Token { text: "damage".into(), lemma: "damage".into(), upos: "NOUN".into(), deprel: "obj".into(), head: 4 },
                    Token { text: "15%".into(), lemma: "15%".into(), upos: "NUM".into(), deprel: "nummod".into(), head: 2 },
                    Token { text: "reduced".into(), lemma: "reduce".into(), upos: "VERB".into(), deprel: "root".into(), head: 0 },
                ],
            }],
        };
        let entries = [
            Entry { id: "game-1", cc: "eldenring.exe", to: Some("0x1111"), record: &game_rec },
            Entry { id: "web-2", cc: "fextralife.com", to: Some("0x2222"), record: &web_rec },
        ];
        let (links, local_sup) = curate_relay_federation(&entries, &emb, &cfg);
        assert_eq!(links.len(), 1, "eldenring.exe and fextralife.com must connect via malenia");
        assert!(links[0].shared_keys.contains(&"malenia".to_string()));
        for s in &local_sup {
            assert!(!s.same_scope(), "cross-cc supersession must never trigger same-scope server sync");
            let sync = supersede_sync(
                s.old_to.as_deref(),
                s.new_to.as_deref(),
                &SupersedeFacts {
                    old_mine: true,
                    new_mine: true,
                    signed_in: true,
                    old_stored: true,
                    new_stored: true,
                    sharing_consent: true,
                },
            );
            assert_eq!(sync, SupersedeSync::LocalOnly(LocalOnlyReason::CrossScope));
        }

        // Meta transition score: shared key yields > 0, unrelated app yields 0.0 (PLINKO cliff preserved!).
        let web_keys = lift_checkpoint_relay_keys(&CheckpointMeta {
            tab_title: Some("Malenia Boss Guide - Fextralife"),
            ..Default::default()
        });
        let shop_keys = lift_checkpoint_relay_keys(&CheckpointMeta {
            tab_title: Some("Steam Summer Sale Discount"),
            ..Default::default()
        });
        let mut all = cp_keys.clone();
        all.extend(web_keys.clone());
        all.extend(shop_keys.clone());
        let cl = cluster_keys(&all, &emb, &cfg);
        let (s_related, shared) = meta_transition_score(&cp_keys, &web_keys, &emb.embed("말레니아"), &emb.embed("Malenia"), &cl, 1.0);
        assert!(s_related > 0.3 && shared.contains(&"malenia".to_string()));
        let (s_unrelated, empty) = meta_transition_score(&cp_keys, &shop_keys, &emb.embed("말레니아"), &emb.embed("Steam"), &cl, 1.0);
        assert_eq!(s_unrelated, 0.0);
        assert!(empty.is_empty());
    }

    #[test]
    fn quarantine_ledger_with_caps_round_trips_above_default_1024() {
        let mut raw_entries = Vec::with_capacity(1_200);
        for i in 0..1_200usize {
            raw_entries.push(QuarantineEntry {
                cc: format!("scope-{}", i % 10),
                old_id: format!("old-{i}"),
                new_id: format!("new-{i}"),
                old_to: None,
                new_to: None,
                old_at_ms: 1_000,
                new_at_ms: 2_000 + i as i64,
                value_diff: None,
                recorded_at_ms: 3_000 + i as i64,
                sync_state: QuarantineSyncState::LocalOnlyCrossScope,
            });
        }
        let default_l = QuarantineLedger::from_entries(raw_entries.clone());
        assert_eq!(default_l.len(), QuarantineLedger::TOTAL_CAP);

        let large_l = QuarantineLedger::from_entries_with_caps(raw_entries, 256, 2_048);
        assert_eq!(large_l.len(), 1_200);
        assert!(large_l.is_quarantined("scope-0", "old-0", |_| Standing::default()));
        assert!(large_l.is_quarantined("scope-9", "old-1199", |_| Standing::default()));
    }

    #[test]
    fn pinned_paragraph_indices_preserves_headers_negations_and_conditions() {
        let paras = [
            "# Section 1 Overview",
            "일반적인 배경 서술 문단으로 특별한 조건이나 수치가 없습니다.",
            "1페이즈 피해 감소율은 20%로 적용됩니다.",
            "회피 불가 패턴으로 변경되었습니다.",
            "또 다른 일반 서술 문단입니다.",
        ];
        let pins = pinned_paragraph_indices_text(&paras);
        assert!(pins.contains(&0), "header must be pinned: {pins:?}");
        assert!(pins.contains(&2), "condition + numeric paragraph must be pinned: {pins:?}");
        assert!(pins.contains(&3), "polarity/negation paragraph must be pinned: {pins:?}");
        assert!(!pins.contains(&1), "plain prose need not be pinned: {pins:?}");
        assert!(!pins.contains(&4), "plain prose need not be pinned: {pins:?}");
    }
}
