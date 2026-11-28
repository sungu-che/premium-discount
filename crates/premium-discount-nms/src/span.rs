//! Span NMS over an utterance (port of logis-center `query_shipping.rs` / `query_commerce.rs`).
//!
//! Every word span of length 1..=`max_len` is scored against the dictionary's domain bank; candidates are
//! sorted by score, then by length (longer wins ties), and any candidate that overlaps an already accepted span
//! by at least one word is suppressed. The survivors, in reading order, are the expression domains of the
//! utterance ("맨 앞으로 나와서 보스 패턴 알려줘" → system.topmost_on + coach.game_tip).
//!
//! 구문 압축 구간 ([`CompressedSpan`]): 이어진 어절 창만으로는 수식어가 길게 끼어든 문장("물새 난무는 패치 뒤에 2페이즈 들어가서
//! 처음 나올 때 어떻게 피해")에서 논항과 서술어가 한 구간에 묶이지 않는다. 그래서 창 구간에 더해, 건너뛰는 구간을 만든다:
//!
//! * [`head_arg_spans`] — 구문 분석([`Word::head`] · `deprel`)이 있을 때. 서술 핵(root · acl · advcl)마다 그 핵이 직접 거느린
//!   핵심 논항(nsubj · obj · obl · iobj · dislocated)과 논항에 붙은 복합 명사 사슬(compound · flat · nummod · fixed)만 읽는
//!   순서대로 잇는다. 끼어든 부사절 · 관형절 · 부사는 빠진다. Stanza 분석기가 [`unit_heads`] 로 머리를 채운다.
//! * [`noun_phrase_spans`] — 구문 분석이 없을 때(규칙 분석기). 조사로 끝을 아는 명사구([`noun_phrases`], 조사 뗀 원형)와 발화 끝
//!   서술어를 잇는다.
//!
//! [`syntactic_spans`] 가 둘 중 맞는 것을 고르고, [`span_assign_with`] · [`span_nms_with`] 가 창 구간과 같은 행렬 · 같은 규칙으로
//! 겨루게 한다. 압축 구간이 이기면 [`SpanHit::skipped`] 에 건너뛴 낱말이 남고, 겹침은 범위가 아니라 실제 낱말로 본다.

use crate::domain::{DomainBank, Hit};
use crate::embed::TextEmbedder;
use crate::normalize::{Role, Word};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpanHit {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Index into the bank.
    pub index: usize,
    pub score: f32,
    pub hit: Hit,
    /// 압축 구간이 `start..end` 안에서 건너뛴 낱말 번호 (끼어든 수식어). 창 구간은 비어 있다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<usize>,
}

impl SpanHit {
    /// 구간이 실제로 덮는 낱말인가 (건너뛴 낱말은 아님)
    pub fn covers(&self, i: usize) -> bool {
        i >= self.start && i < self.end && !self.skipped.contains(&i)
    }

    /// 구간의 낱말 번호 (읽는 순서)
    pub fn word_indices(&self) -> Vec<usize> {
        (self.start..self.end).filter(|i| !self.skipped.contains(i)).collect()
    }

    /// 두 구간이 낱말 하나라도 같이 덮는가. 창 구간끼리는 범위 겹침과 같다.
    pub fn overlaps(&self, other: &SpanHit) -> bool {
        self.start < other.end && self.end > other.start && self.word_indices().into_iter().any(|i| other.covers(i))
    }
}

/// Thresholds of the v2 selection ([`span_assign`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AssignConfig {
    pub max_len: usize,
    /// Minimum raw score (bias − w·prejudice) of a vector hit.
    pub accept: f32,
    /// Minimum lead of the winning domain over the runner-up on the same span, in double-centred residuals.
    pub margin: f32,
    /// Minimum double-centred residual of a vector hit (how far above its row and column means it stands).
    pub residual: f32,
    /// Hybrid scoring: a lexical (hashed n-gram) score above `lexical_floor` adds `lexical_weight × (score − floor)`
    /// to the semantic one before centring, so shared words break semantic near-ties (dense + sparse).
    pub lexical_weight: f32,
    pub lexical_floor: f32,
}

/// Characters of a span's content words (vector hits need at least two: a bare "이", "뭐" names nothing).
fn content_chars(words: &[Word]) -> usize {
    words
        .iter()
        .filter(|w| w.is_content())
        .map(|w| w.surface.chars().filter(|c| c.is_alphanumeric()).count())
        .sum()
}

/// A demonstrative pointing at a one-syllable spatial noun ("이 칸", "방금 그 줄"): the span names a place although
/// its content has one character, so it may still reach the vector stage.
fn deictic_spatial(words: &[Word]) -> bool {
    words
        .windows(2)
        .any(|w| crate::korean::is_deictic_spatial_pair(&w[0].surface, &w[1].surface))
}

/// What [`span_assign`] chose, plus the observations SDS keeps.
#[derive(Debug, Clone, Default)]
pub struct Assigned {
    pub hits: Vec<SpanHit>,
    /// Raw domain scores of each winning vector span (decay shapes for SDS).
    pub columns: Vec<Vec<f32>>,
    /// Ties the margin gate held back (domain index, rival index).
    pub held: Vec<(usize, usize)>,
}

/// 압축 구간이 어디서 왔나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SpanSource {
    /// 의존 구문 머리-논항 ([`head_arg_spans`])
    HeadArg,
    /// 조사로 끝을 아는 명사구 + 발화 끝 서술어 ([`noun_phrase_spans`])
    NounPhrase,
}

/// 이어지지 않아도 되는 구간: 낱말 번호 (오름차순, 중복 없음)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompressedSpan {
    pub words: Vec<usize>,
    pub source: SpanSource,
}

impl CompressedSpan {
    pub fn start(&self) -> usize {
        self.words.first().copied().unwrap_or(0)
    }

    /// 마지막 낱말 다음 번호
    pub fn end(&self) -> usize {
        self.words.last().map_or(0, |l| l + 1)
    }

    /// `start..end` 안에서 건너뛴 낱말
    pub fn skipped(&self) -> Vec<usize> {
        (self.start()..self.end()).filter(|i| self.words.binary_search(i).is_err()).collect()
    }

    /// 낱말 표면형을 띄어 이은 글
    pub fn text(&self, words: &[Word]) -> String {
        self.words.iter().filter_map(|&i| words.get(i)).map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// 낱말 번호가 오름차순이고 모두 `len` 안인가
    fn valid(&self, len: usize) -> bool {
        !self.words.is_empty() && self.words.windows(2).all(|p| p[0] < p[1]) && self.end() <= len
    }
}

/// 의존 관계 이름의 앞부분 ("nsubj:pass" → "nsubj")
fn rel(w: &Word) -> &str {
    w.deprel.split(':').next().unwrap_or("")
}

/// 정렬 · 중복 제거한 뒤, 두 낱말 이상이고 창 구간이 이미 보는 것(이어지고 `max_len` 이하)이 아니면 넣는다.
fn push_span(out: &mut Vec<CompressedSpan>, mut words: Vec<usize>, max_len: usize, source: SpanSource) {
    words.sort_unstable();
    words.dedup();
    if words.len() < 2 {
        return;
    }
    let contiguous = words.windows(2).all(|p| p[1] == p[0] + 1);
    if contiguous && words.len() <= max_len {
        return;
    }
    let span = CompressedSpan { words, source };
    if !out.iter().any(|s| s.words == span.words) {
        out.push(span);
    }
}

/// Stanza 처럼 낱말마다 머리 번호(1부터, 0 은 뿌리, 문장 안 번호)를 주는 분석을 단위(공백 어절) 머리로 바꾼다.
/// `units[u]` 는 단위 u 에 든 낱말들의 머리 번호다(premium-discount-nlp `QueryUnit.words[..].head` 와 같은 모양).
/// 단위의 머리는 단위 밖을 가리키는 첫 낱말이 기대는 낱말을 든 단위이고, 뿌리 낱말을 든 단위는 None 이다.
/// 뿌리가 꼭 하나일 때(한 문장)만 믿는다: 문장이 둘 이상이면 머리 번호가 문장마다 다시 1부터라 모두 None 을 준다.
pub fn unit_heads(units: &[Vec<usize>]) -> Vec<Option<usize>> {
    let owner: Vec<usize> = units.iter().enumerate().flat_map(|(u, ws)| std::iter::repeat_n(u, ws.len())).collect();
    let roots = units.iter().flatten().filter(|&&h| h == 0).count();
    let mut out = vec![None; units.len()];
    if roots != 1 {
        return out;
    }
    let mut first = 0;
    for (u, ws) in units.iter().enumerate() {
        if !ws.contains(&0) {
            let inside = |h: usize| h > first && h <= first + ws.len();
            out[u] = ws.iter().find(|&&h| !inside(h)).and_then(|&h| owner.get(h - 1).copied());
        }
        first += ws.len();
    }
    out
}

/// 낱말 `a` 와 그 아래로 compound · flat · nummod · fixed 로 이어진 낱말들 (복합 명사 사슬)
fn compound_chain(words: &[Word], a: usize) -> Vec<usize> {
    let mut chain = vec![a];
    let mut k = 0;
    while k < chain.len() && chain.len() <= words.len() {
        let h = chain[k];
        for (j, w) in words.iter().enumerate() {
            if w.head == Some(h) && matches!(rel(w), "compound" | "flat" | "nummod" | "fixed") && !chain.contains(&j) {
                chain.push(j);
            }
        }
        k += 1;
    }
    chain
}

/// 의존 구문 머리-논항 압축 구간. 서술 핵(root · acl · advcl)마다, 핵과 거기 붙은 보조 용언 · 부정에 더해
///
/// * 핵심 논항(nsubj · obj · obl · iobj · dislocated, 내용어) 하나씩과 그 복합 명사 사슬을 이은 구간,
/// * 논항이 둘 이상이면 모두를 이은 절 구간
///
/// 을 만든다. 창 구간이 이미 보는 것(이어지고 `max_len` 이하)은 뺀다. [`Word::head`] 가 하나도 없으면 빈 목록.
pub fn head_arg_spans(words: &[Word], max_len: usize) -> Vec<CompressedSpan> {
    let mut out = Vec::new();
    if !words.iter().any(|w| w.head.is_some()) {
        return out;
    }
    let children = |p: usize| (0..words.len()).filter(move |&j| j != p && words[j].head == Some(p));
    for p in 0..words.len() {
        if !matches!(rel(&words[p]), "root" | "acl" | "advcl") {
            continue;
        }
        // 핵 + 보조 용언 · 부정 ("알려 줘", "끄지 마", "안 가"): 부정 표시가 구간에서 빠지지 않게
        let mut core = vec![p];
        core.extend(children(p).filter(|&j| matches!(words[j].role, Role::Auxiliary | Role::Negation) || matches!(rel(&words[j]), "aux" | "cop")));
        let args: Vec<usize> = children(p)
            .filter(|&j| words[j].is_content() && matches!(rel(&words[j]), "nsubj" | "obj" | "obl" | "iobj" | "dislocated"))
            .collect();
        let mut clause = core.clone();
        for &a in &args {
            let chain = compound_chain(words, a);
            let mut one = core.clone();
            one.extend(&chain);
            push_span(&mut out, one, max_len, SpanSource::HeadArg);
            clause.extend(chain);
        }
        if args.len() > 1 {
            push_span(&mut out, clause, max_len, SpanSource::HeadArg);
        }
    }
    out
}

/// 조사로 끝을 아는 명사구 하나 ([`noun_phrases`])
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NounPhrase {
    /// 낱말 번호 (이어진 범위)
    pub words: Vec<usize>,
    /// 토픽 구: 마지막 낱말만 조사를 뗀 꼴 ("암월의 대검이랑" → "암월의 대검")
    pub text: String,
    /// 쓴 그대로 ("암월의 대검이랑")
    pub surface: String,
}

/// 흔한 부사 · 의문사 · 대명사 · 지시어 (토픽 명사구가 아니다)
const NOT_TOPIC: &[&str] = &[
    "어떻게", "어떡해", "왜", "언제", "어디서", "어디", "뭐", "너무", "진짜", "정말", "좀", "막", "아까", "방금", "지금", "다시", "계속",
    "잘", "더", "덜", "많이", "조금", "빨리", "같이", "그냥", "또", "이제", "아직", "벌써", "항상", "절대", "꼭", "제일", "가장", "완전",
    "엄청", "되게", "별로", "혹시", "아마", "먼저", "나중에", "바로", "처음", "자꾸", "일단", "약간", "그거", "이거", "저거", "그것", "이것",
    "저것", "여기", "거기", "저기", "나", "너", "내", "네", "저", "제", "우리",
];
/// 혼자서는 토픽이 아닌 의존 명사 ("중에", "때", "거"): 한 낱말짜리 명사구면 버린다
const BOUND_NOUNS: &[&str] = &["중", "것", "거", "수", "때", "적", "데", "줄", "뿐", "쪽", "번", "개"];
/// 명사 끝 글자이기도 한 한 글자 조사 ("명도", "속도", "도로"): 남는 원형이 한 음절이면 조사로 보지 않는다
const AMBIGUOUS_TAILS: &[&str] = &["도", "이", "로", "만", "들"];

/// 구문 분석 없이 본 명사구의 낱말인가: 내용어이고, 품사가 있으면 명사 · 고유명사 · 수 · 외래어, 없으면 부사 · 대명사 · 연결어미 ·
/// 관형형("있는", "하던")이 아닌 것
fn nominal(w: &Word) -> bool {
    if !w.is_content() || w.negated {
        return false;
    }
    if !w.upos.is_empty() {
        return matches!(w.upos.as_str(), "NOUN" | "PROPN" | "NUM" | "X");
    }
    let s = w.surface.as_str();
    let connective = crate::analyze::adversative_ending(w)
        || (s.chars().count() >= 2 && ["고", "서", "면", "니까", "려고", "도록"].iter().any(|t| s.ends_with(t)) && w.lemma == w.surface);
    // 관형형: 조사로 떼어지지 않은 "-는 · -던" ("있는", "하던"; 받침 없는 명사 + 주제 조사 "는" 은 원형이 짧아진다)
    let adnominal = (s.ends_with('는') || s.ends_with('던')) && w.lemma == w.surface && s.chars().count() >= 2;
    !NOT_TOPIC.contains(&w.lemma.as_str()) && !NOT_TOPIC.contains(&s) && !connective && !adnominal
}

/// 조사가 붙어 명사구가 끝나는 낱말인가. 관형격 "의" 는 명사구를 잇고("말레니아의 물새 난무"), 한 음절 원형 뒤의 애매한 한 글자
/// 조사("명도", "속도")는 명사의 일부로 본다.
fn case_marked(w: &Word) -> bool {
    let Some(tail) = w.surface.strip_prefix(w.lemma.as_str()).filter(|t| !t.is_empty()) else { return false };
    let one_syllable_stem = w.lemma.chars().count() == 1;
    tail != "의" && !(one_syllable_stem && AMBIGUOUS_TAILS.contains(&tail))
}

/// 발화 끝 서술어 묶음의 첫 낱말: 마지막 내용어 (뒤의 보조 용언 · 부정 · 기능어는 묶음에 든다). 내용어가 없으면 None.
fn final_predicate(words: &[Word]) -> Option<usize> {
    let p = words.iter().rposition(|w| w.is_content())?;
    let w = &words[p];
    let predicate = if w.upos.is_empty() { !case_marked(w) } else { matches!(w.upos.as_str(), "VERB" | "ADJ" | "AUX") };
    predicate.then_some(p)
}

/// 구문 분석 없이 뽑는 명사구 (토픽 구 후보): 발화 끝 서술어 앞에서, 이어진 명사 낱말들을 조사가 붙은 낱말(관형격 "의" 빼고)에서
/// 끊는다. normalize 가 만든 조사 뗀 원형([`Word::lemma`])을 쓴다. zena-core 의 지시어 복원이 토픽 엔티티 후보로 받는다.
pub fn noun_phrases(words: &[Word]) -> Vec<NounPhrase> {
    let limit = final_predicate(words).unwrap_or(words.len());
    let mut out = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let flush = |cur: &mut Vec<usize>, out: &mut Vec<NounPhrase>| {
        let (Some(&first), Some(&last)) = (cur.first(), cur.last()) else { return };
        let lone_bound = first == last && BOUND_NOUNS.contains(&words[last].lemma.as_str());
        if lone_bound {
            cur.clear();
            return;
        }
        let mut text: Vec<&str> = words[first..last].iter().map(|w| w.surface.as_str()).collect();
        text.push(if case_marked(&words[last]) { words[last].lemma.as_str() } else { words[last].surface.as_str() });
        out.push(NounPhrase {
            words: std::mem::take(cur),
            text: text.join(" "),
            surface: words[first..=last].iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" "),
        });
    };
    for (i, w) in words.iter().enumerate().take(limit) {
        if !nominal(w) {
            flush(&mut cur, &mut out);
            continue;
        }
        cur.push(i);
        if case_marked(w) {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// 구문 분석이 없을 때의 압축 구간: 명사구마다 (머리가 끝에 오는 한국어라 끝 4낱말까지) + 발화 끝 서술어 묶음, 명사구가 둘 이상이면
/// 모두 + 서술어 묶음. 창 구간이 이미 보는 것은 뺀다.
pub fn noun_phrase_spans(words: &[Word], max_len: usize) -> Vec<CompressedSpan> {
    let mut out = Vec::new();
    let Some(p) = final_predicate(words) else { return out };
    let predicate: Vec<usize> = (p..words.len()).collect();
    let phrases = noun_phrases(words);
    let mut all = predicate.clone();
    for np in &phrases {
        let tail = &np.words[np.words.len().saturating_sub(4)..];
        let mut one = predicate.clone();
        one.extend_from_slice(tail);
        push_span(&mut out, one, max_len, SpanSource::NounPhrase);
        all.extend_from_slice(tail);
    }
    if phrases.len() > 1 {
        push_span(&mut out, all, max_len, SpanSource::NounPhrase);
    }
    out
}

/// 맞는 압축 구간: 구문 머리가 있으면 [`head_arg_spans`], 없으면 [`noun_phrase_spans`]
pub fn syntactic_spans(words: &[Word], max_len: usize) -> Vec<CompressedSpan> {
    if words.iter().any(|w| w.head.is_some()) {
        head_arg_spans(words, max_len)
    } else {
        noun_phrase_spans(words, max_len)
    }
}

/// 후보 구간 하나: 창(`own` 없음) 또는 압축 구간(`own` = 낱말 사본 번호)
struct Candidate {
    start: usize,
    end: usize,
    skipped: Vec<usize>,
    own: Option<usize>,
}

/// 창 구간(1..=`max_len`, `eligible` 을 지나는 것)과 압축 구간(`extra`, 올바르고 `eligible` 인 것)을 모은다.
fn candidates<M: Clone, E: TextEmbedder>(
    words: &[Word],
    extra: &[CompressedSpan],
    bank: &DomainBank<M, E>,
    max_len: usize,
    eligible_only: bool,
) -> (Vec<Candidate>, Vec<Vec<Word>>) {
    let mut cands = Vec::new();
    for s in 0..words.len() {
        for e in (s + 1)..=(s + max_len).min(words.len()) {
            if !eligible_only || bank.eligible(&words[s..e]) {
                cands.push(Candidate { start: s, end: e, skipped: Vec::new(), own: None });
            }
        }
    }
    let mut owned: Vec<Vec<Word>> = Vec::new();
    for c in extra.iter().filter(|c| c.valid(words.len())) {
        let ws: Vec<Word> = c.words.iter().map(|&i| words[i].clone()).collect();
        let (start, end, skipped) = (c.start(), c.end(), c.skipped());
        let dup = cands.iter().any(|x| x.start == start && x.end == end && x.skipped == skipped);
        if dup || (eligible_only && !bank.eligible(&ws)) {
            continue;
        }
        cands.push(Candidate { start, end, skipped, own: Some(owned.len()) });
        owned.push(ws);
    }
    (cands, owned)
}

/// NLP-NMS v2 span selection — logis-center's Plinko laws 2–3 over spans × domains:
///
/// 1. every span with a content word (up to `max_len` words) is scored against every domain in one batch;
/// 2. exact / prefix hits are kept as in v1 (they decide before vectors);
/// 3. the vector scores are **double-centred** (`premium_discount_sds::matrix::double_center`): a domain that scores high on
///    every span, or a span that scores high on every domain (a bare "봐"), loses its pull;
/// 4. each domain claims at most one span and each span one domain, largest residual first, only with a lead of
///    `margin` over the runner-up (`exclusive_assign_adaptive`; near ties go to `allow_tie`, e.g. the SDS
///    confusion ledger) and a raw score of at least `accept`;
/// 5. overlapping winners are suppressed greedily by score, and a domain appears once — as in [`span_nms`].
pub fn span_assign<M: Clone, E: TextEmbedder, L: TextEmbedder>(
    words: &[Word],
    bank: &DomainBank<M, E>,
    lexical: Option<&DomainBank<M, L>>,
    cfg: &AssignConfig,
    allow_tie: impl Fn(usize, usize, premium_discount_sds::TieKind) -> bool,
) -> Assigned {
    span_assign_with(words, &[], bank, lexical, cfg, allow_tie)
}

/// [`span_assign`] 에 압축 구간 `extra`(보통 [`syntactic_spans`])를 더해 같은 행렬에서 겨루게 한다. `extra` 가 비면 [`span_assign`]
/// 과 같다. 압축 구간의 글은 그 낱말들만 이은 것이고, 이긴 압축 구간은 실제로 덮은 낱말만 다른 구간과 겹친다.
pub fn span_assign_with<M: Clone, E: TextEmbedder, L: TextEmbedder>(
    words: &[Word],
    extra: &[CompressedSpan],
    bank: &DomainBank<M, E>,
    lexical: Option<&DomainBank<M, L>>,
    cfg: &AssignConfig,
    allow_tie: impl Fn(usize, usize, premium_discount_sds::TieKind) -> bool,
) -> Assigned {
    let (ranges, owned) = candidates(words, extra, bank, cfg.max_len, true);
    let mut out = Assigned::default();
    if ranges.is_empty() || bank.is_empty() {
        return out;
    }
    let spans: Vec<&[Word]> = ranges
        .iter()
        .map(|c| match c.own {
            Some(k) => owned[k].as_slice(),
            None => &words[c.start..c.end],
        })
        .collect();
    let mut raw = bank.raw_scores(&spans);
    // Dense + sparse: the lexical bank (same dictionary, same order) adds its evidence above the floor.
    if let Some(lex) = lexical.filter(|l| l.len() == bank.len() && cfg.lexical_weight > 0.0) {
        for (col, lcol) in raw.iter_mut().zip(lex.raw_scores(&spans)) {
            for (d, l) in col.iter_mut().zip(lcol) {
                if d.hit == Hit::Vector {
                    d.score += cfg.lexical_weight * (l.score - cfg.lexical_floor).max(0.0);
                }
            }
        }
    }
    let text = |j: usize| spans[j].iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ");
    let hit_at = |j: usize, index: usize, score: f32, hit: Hit| SpanHit {
        start: ranges[j].start,
        end: ranges[j].end,
        text: text(j),
        index,
        score,
        hit,
        skipped: ranges[j].skipped.clone(),
    };

    let mut cands: Vec<SpanHit> = Vec::new();
    // Exact / prefix hits first; their cells stay out of the vector matrix.
    let mut matrix = vec![vec![premium_discount_sds::matrix::ABSENT; ranges.len()]; bank.len()];
    for (j, scores) in raw.iter().enumerate() {
        let short = content_chars(spans[j]) < 2 && !deictic_spatial(spans[j]);
        for d in scores {
            if d.hit == Hit::Vector {
                if !short {
                    matrix[d.index][j] = d.score;
                }
            } else {
                cands.push(hit_at(j, d.index, d.score, d.hit));
            }
        }
    }
    let centred = premium_discount_sds::matrix::double_center(&matrix);
    let (assigned, diags) = premium_discount_sds::matrix::exclusive_assign_adaptive(&centred, cfg.residual, cfg.margin, allow_tie);
    for d in diags.iter().filter(|d| !d.accepted) {
        out.held.push((d.field, d.rival));
    }
    for (domain, a) in assigned.iter().enumerate() {
        let Some((j, _residual, _margin)) = *a else { continue };
        let raw_score = raw[j][domain].score;
        if raw_score < cfg.accept {
            continue;
        }
        cands.push(hit_at(j, domain, raw_score, Hit::Vector));
        out.columns.push(raw[j].iter().map(|d| d.score).collect());
    }
    cands.sort_by(|a, b| b.score.total_cmp(&a.score).then((b.end - b.start).cmp(&(a.end - a.start))));
    let mut winners: Vec<SpanHit> = Vec::new();
    for c in cands {
        let overlaps = winners.iter().any(|w| c.overlaps(w));
        if !overlaps && !winners.iter().any(|w| w.index == c.index) {
            winners.push(c);
        }
    }
    winners.sort_by_key(|w| w.start);
    out.hits = winners;
    out
}

pub fn span_nms<M: Clone, E: TextEmbedder>(words: &[Word], bank: &DomainBank<M, E>, max_len: usize, accept: f32) -> Vec<SpanHit> {
    span_nms_with(words, &[], bank, max_len, accept)
}

/// [`span_nms`] 에 압축 구간 `extra` 를 더한 것 (`extra` 가 비면 같다)
pub fn span_nms_with<M: Clone, E: TextEmbedder>(
    words: &[Word],
    extra: &[CompressedSpan],
    bank: &DomainBank<M, E>,
    max_len: usize,
    accept: f32,
) -> Vec<SpanHit> {
    let (ranges, owned) = candidates(words, extra, bank, max_len, false);
    let mut cands = Vec::new();
    for c in &ranges {
        let span = match c.own {
            Some(k) => owned[k].as_slice(),
            None => &words[c.start..c.end],
        };
        let Some(best) = bank.best(span) else { continue };
        if best.score >= accept {
            let text = span.iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ");
            cands.push(SpanHit {
                start: c.start,
                end: c.end,
                text,
                index: best.index,
                score: best.score,
                hit: best.hit,
                skipped: c.skipped.clone(),
            });
        }
    }
    cands.sort_by(|a, b| b.score.total_cmp(&a.score).then((b.end - b.start).cmp(&(a.end - a.start))));
    let mut winners: Vec<SpanHit> = Vec::new();
    for c in cands {
        if !winners.iter().any(|w| c.overlaps(w)) {
            winners.push(c);
        }
    }
    // The same domain twice in one utterance collapses to the stronger span.
    winners.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut seen = Vec::new();
    winners.retain(|w| {
        let fresh = !seen.contains(&w.index);
        seen.push(w.index);
        fresh
    });
    winners.sort_by_key(|w| w.start);
    winners
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{Analyzer, RuleAnalyzer};
    use crate::domain::DomainSpec;
    use crate::embed::HashedNgramEmbedder;
    use crate::normalize::normalize;

    fn bank() -> DomainBank<&'static str, HashedNgramEmbedder> {
        let spec = |key: &str, bias: &[&str], exact: &[&str]| DomainSpec {
            key: key.into(),
            payload: "",
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: vec![],
            exact: exact.iter().map(|e| e.to_string()).collect(),
        };
        DomainBank::from_specs(
            vec![
                spec("window.front", &["화면 맨 앞으로 나와", "맨 앞으로 와"], &["맨 앞으로"]),
                spec("coach.tip", &["보스 패턴 어떻게 피해", "공략 알려 줘"], &[]),
                spec("feel.thanks", &["고마워", "감사해"], &["고마워"]),
            ],
            &["해 줘".to_string(), "좀".to_string()],
            HashedNgramEmbedder::default(),
        )
    }

    #[test]
    fn one_utterance_two_domains() {
        let bank = bank();
        let n = normalize("화면 맨 앞으로 나와서 보스 패턴 어떻게 피해");
        let hits = span_nms(&n.words, &bank, 5, 0.6);
        let keys: Vec<_> = hits.iter().map(|h| bank.entry(h.index).key.as_str()).collect();
        assert_eq!(keys, vec!["window.front", "coach.tip"], "{hits:?}");
        assert!(hits.windows(2).all(|w| w[0].end <= w[1].start));
    }

    /// 회피 공략 · 패치 노트 · 감사 세 도메인 (해시 임베더)
    fn dodge_bank() -> DomainBank<&'static str, HashedNgramEmbedder> {
        let spec = |key: &str, bias: &[&str]| DomainSpec {
            key: key.into(),
            payload: "",
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: vec![],
            exact: vec![],
        };
        DomainBank::from_specs_content(
            vec![
                spec("coach.dodge", &["물새 난무 피해", "물새 난무 회피"]),
                spec("coach.patch", &["패치 뒤에", "패치 내용"]),
                spec("feel.thanks", &["고마워", "감사해"]),
            ],
            &["해 줘".to_string(), "좀".to_string()],
            HashedNgramEmbedder::default(),
        )
    }

    const LONG: &str = "물새 난무는 패치 뒤에 2페이즈 들어가서 처음 나올 때 어떻게 피해";

    /// [`LONG`] 의 Stanza 식 구문 분석을 손으로 (단위 번호 머리): 피해(root) ← 난무는(dislocated) ← 물새(compound),
    /// 들어가서(advcl) ← 2페이즈(obl) · 뒤에(obl) ← 패치(nmod), 나올(acl) ← 처음(advmod), 때(obl) ← 나올, 어떻게(advmod)
    fn parsed_long() -> Vec<Word> {
        let mut n = normalize(LONG);
        RuleAnalyzer.analyze(&mut n);
        let parse: [(&str, &str, Option<usize>); 11] = [
            ("NOUN", "compound", Some(1)),
            ("NOUN", "dislocated", Some(10)),
            ("NOUN", "nmod", Some(3)),
            ("NOUN", "obl", Some(5)),
            ("NOUN", "obl", Some(5)),
            ("VERB", "advcl", Some(10)),
            ("ADV", "advmod", Some(7)),
            ("VERB", "acl", Some(8)),
            ("NOUN", "obl", Some(10)),
            ("ADV", "advmod", Some(10)),
            ("VERB", "root", None),
        ];
        assert_eq!(n.words.len(), parse.len());
        for (w, (upos, deprel, head)) in n.words.iter_mut().zip(parse) {
            w.upos = upos.into();
            w.deprel = deprel.into();
            w.head = head;
        }
        n.words
    }

    fn cfg() -> AssignConfig {
        AssignConfig { max_len: 5, accept: 0.6, margin: 0.0, residual: -0.99, lexical_weight: 0.0, lexical_floor: 0.0 }
    }

    fn dodge_hit(a: &Assigned, bank: &DomainBank<&'static str, HashedNgramEmbedder>) -> Option<SpanHit> {
        a.hits.iter().find(|h| bank.entry(h.index).key == "coach.dodge").cloned()
    }

    #[test]
    fn a_demonstrative_and_a_one_syllable_spatial_noun_reach_the_vector_stage() {
        let spec = |key: &str, bias: &[&str]| DomainSpec {
            key: key.into(),
            payload: "",
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: vec![],
            exact: vec![],
        };
        let bank = DomainBank::from_specs_content(
            vec![spec("screen.line", &["그 줄", "이 줄"]), spec("screen.tab", &["옆 탭", "저 탭"])],
            &["알려 줘".to_string()],
            HashedNgramEmbedder::default(),
        );
        let none = None::<&DomainBank<&str, HashedNgramEmbedder>>;
        let assign = |text: &str| {
            let mut n = normalize(text);
            RuleAnalyzer.analyze(&mut n);
            (span_assign(&n.words, &bank, none, &cfg(), |_, _, _| false), n.words)
        };
        let (a, words) = assign("그 줄");
        assert!(!words[0].is_content(), "the demonstrative is a function word: {words:?}");
        assert!(a.hits.iter().any(|h| bank.entry(h.index).key == "screen.line"), "{a:?}");
        let (b, _) = assign("옆 탭에");
        assert!(b.hits.iter().any(|h| bank.entry(h.index).key == "screen.tab"), "a particle on the noun: {b:?}");
        // a bare one-syllable word still names nothing, nor does a demonstrative before another noun
        assert!(assign("줄").0.hits.is_empty());
        assert!(!crate::korean::is_deictic_spatial_pair("그", "말"));
        assert!(crate::korean::is_deictic_spatial_pair("아래", "행의"));
    }

    #[test]
    fn cr04_head_arg_span_catches_a_long_modifier_paraphrase() {
        let bank = dodge_bank();
        let words = parsed_long();
        // 창 구간만으로는 "물새 난무" 와 서술어 "피해" 가 8낱말 떨어져 한 구간에 묶이지 않는다
        let windows = span_assign(&words, &bank, None::<&DomainBank<&str, HashedNgramEmbedder>>, &cfg(), |_, _, _| false);
        let extra = syntactic_spans(&words, 5);
        assert!(extra.iter().all(|s| s.source == SpanSource::HeadArg));
        let clause = extra.iter().find(|s| s.words == vec![0, 1, 10]).expect("dislocated topic + compound + root predicate");
        assert_eq!(clause.text(&words), "물새 난무는 피해");
        assert_eq!(clause.skipped(), (2..10).collect::<Vec<_>>());
        let with = span_assign_with(&words, &extra, &bank, None::<&DomainBank<&str, HashedNgramEmbedder>>, &cfg(), |_, _, _| false);
        let hit = dodge_hit(&with, &bank).expect("the compressed span names the dodge tip");
        assert_eq!((hit.text.as_str(), hit.start, hit.end), ("물새 난무는 피해", 0, 11), "{with:?}");
        assert_eq!(hit.word_indices(), vec![0, 1, 10]);
        assert!(!hit.covers(5) && hit.covers(10));
        let window_best = dodge_hit(&windows, &bank).map_or(0.0, |h| h.score);
        assert!(hit.score > window_best + 0.05, "compressed {} vs windows {window_best}", hit.score);
        // 건너뛴 부사절의 "패치 뒤에" 는 여전히 다른 도메인이 가져갈 수 있다 (실제 낱말로 겹침을 본다)
        assert!(with.hits.iter().any(|h| bank.entry(h.index).key == "coach.patch" && h.start >= 2 && h.end <= 10), "{with:?}");
    }

    #[test]
    fn noun_phrase_fallback_strips_particles_without_a_parse() {
        let bank = dodge_bank();
        let mut n = normalize(LONG);
        RuleAnalyzer.analyze(&mut n);
        let words = n.words;
        assert!(words.iter().all(|w| w.head.is_none()));
        let nps = noun_phrases(&words);
        let texts: Vec<&str> = nps.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts.first(), Some(&"물새 난무"), "{nps:?}");
        assert!(texts.contains(&"패치 뒤"), "{nps:?}");
        assert!(!texts.iter().any(|t| t.contains("어떻게") || t.contains("들어가서")), "{nps:?}");
        let extra = syntactic_spans(&words, 5);
        assert!(extra.iter().all(|s| s.source == SpanSource::NounPhrase));
        assert!(extra.iter().any(|s| s.words == vec![0, 1, 10]), "{extra:?}");
        let with = span_assign_with(&words, &extra, &bank, None::<&DomainBank<&str, HashedNgramEmbedder>>, &cfg(), |_, _, _| false);
        let hit = dodge_hit(&with, &bank).expect("the noun phrase + predicate names the dodge tip");
        assert_eq!(hit.text, "물새 난무는 피해", "{with:?}");
        // v1 탐욕 선택도 같은 압축 구간을 쓴다
        let greedy = span_nms_with(&words, &extra, &bank, 5, 0.6);
        assert!(greedy.iter().any(|h| bank.entry(h.index).key == "coach.dodge" && !h.skipped.is_empty()), "{greedy:?}");
    }

    #[test]
    fn noun_phrases_keep_genitives_and_end_at_case_particles() {
        let mut n = normalize("암월의 대검이랑 명도 월은 중에 뭐가 나아");
        RuleAnalyzer.analyze(&mut n);
        let nps = noun_phrases(&n.words);
        assert_eq!(nps[0].text, "암월의 대검", "{nps:?}");
        assert_eq!(nps[0].surface, "암월의 대검이랑");
        assert_eq!(nps[1].text, "명도 월", "{nps:?}");
        let mut n = normalize("말레니아의 물새 난무 어떻게 피해");
        RuleAnalyzer.analyze(&mut n);
        assert_eq!(noun_phrases(&n.words)[0].text, "말레니아의 물새 난무");
        // 서술어만 있는 발화, 빈 발화
        assert!(noun_phrases(&normalize("고마워").words).is_empty());
        assert!(noun_phrase_spans(&[], 5).is_empty());
    }

    #[test]
    fn short_or_contiguous_clauses_add_no_extra_span() {
        // "보스 패턴 알려 줘": 머리-논항 구간이 이어지고 5낱말 이하 → 창이 이미 본다
        let mut n = normalize("보스 패턴 알려 줘");
        RuleAnalyzer.analyze(&mut n);
        let parse = [("compound", Some(1)), ("obj", Some(2)), ("root", None), ("aux", Some(2))];
        for (w, (deprel, head)) in n.words.iter_mut().zip(parse) {
            w.deprel = deprel.into();
            w.head = head;
        }
        assert!(head_arg_spans(&n.words, 5).is_empty());
        // 같은 절이라도 창보다 길면 넣는다
        assert_eq!(head_arg_spans(&n.words, 3).len(), 1);
        // 압축 구간이 없으면 _with 는 원래 함수와 같다
        let bank = bank();
        let n = normalize("화면 맨 앞으로 나와서 보스 패턴 어떻게 피해");
        assert_eq!(span_nms_with(&n.words, &[], &bank, 5, 0.6), span_nms(&n.words, &bank, 5, 0.6));
        // 잘못된 압축 구간(범위 밖 · 순서 틀림)은 버린다
        let bad = [
            CompressedSpan { words: vec![3, 99], source: SpanSource::HeadArg },
            CompressedSpan { words: vec![5, 2], source: SpanSource::HeadArg },
        ];
        assert_eq!(span_nms_with(&n.words, &bad, &bank, 5, 0.6), span_nms(&n.words, &bank, 5, 0.6));
    }

    #[test]
    fn unit_heads_map_word_heads_to_units_for_one_sentence_only() {
        // 단위 0: [2], 단위 1: [3, 2] (두 낱말, 하나는 안을 가리킴), 단위 2: [0] 뿌리
        assert_eq!(unit_heads(&[vec![2], vec![4, 2], vec![0]]), vec![Some(1), Some(2), None]);
        // 뿌리가 둘(문장 둘) → 믿지 않는다
        assert_eq!(unit_heads(&[vec![0], vec![1], vec![0]]), vec![None, None, None]);
        // 범위 밖 번호는 None
        assert_eq!(unit_heads(&[vec![9], vec![0]]), vec![None, None]);
        assert!(unit_heads(&[]).is_empty());
    }

    #[test]
    fn sds_tie_breaker_refuses_flat_and_uses_confusion_winner() {
        let mut ledger = premium_discount_sds::Ledger::in_memory("tie");
        let scope = premium_discount_sds::Scope::new("nms", "app://test", "slot");
        let keys = vec!["coach.dodge".to_string(), "coach.patch".to_string()];
        for _ in 0..6 {
            ledger.record_confusion(&scope, &keys[0], &keys[1], 0.12);
        }
        let tb = sds_tie_breaker(&ledger, &scope, &keys);
        assert!(tb(0, 1, premium_discount_sds::TieKind::PairConfusion));
        assert!(!tb(0, 1, premium_discount_sds::TieKind::Flat));
        assert!(!tb(1, 0, premium_discount_sds::TieKind::PairConfusion));
    }
}

/// Adapter binding `DomainBank` pair ties to `Ledger::confusion_winner_checked` while
/// refusing `TieKind::Flat` ties unconditionally.
pub fn sds_tie_breaker<'a>(
    ledger: &'a premium_discount_sds::Ledger,
    scope: &'a premium_discount_sds::Scope,
    keys: &'a [String],
) -> impl Fn(usize, usize, premium_discount_sds::TieKind) -> bool + 'a {
    move |a, b, kind| {
        if kind == premium_discount_sds::TieKind::Flat {
            return false;
        }
        let (Some(ka), Some(kb)) = (keys.get(a), keys.get(b)) else {
            return false;
        };
        ledger
            .confusion_winner_checked(scope, ka, kb)
            .is_some_and(|(w, _, _)| w == *ka)
    }
}
