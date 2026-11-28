//! 도메인 점수: 사전의 글 노드마다 은행(bank) 항목 하나 ([`DomainSpec`] → [`DomainEntry`]).
//!
//! 항목의 실제 내용(`payload`, 예: Raon Zena 의 `zena_lexicon::Action`)은 부르는 쪽의 타입 `M` 이고, 점수 계산은 보지 않는다.
//! 사전 형식(logis-center `bias.json`, raon-zena `zena_lexicon`)에서 [`DomainSpec`] 목록을 만드는 것도 부르는 쪽이다.
//!
//! Every text node of the dictionary becomes one bank entry. A span of the user's words is scored the way
//! Qntology/logis-center scores fields against `bias.json`:
//!
//! 1. **exact match before vectors** — the span equals an `exact_match` entry (surface, josa-peeled lemma or
//!    request-stripped stem, spaces ignored) → 0.95; or, for Hangul, an entry is a prefix of the span's last word
//!    with at most two syllables left over (logis-center's agglutinative prefix match: "클리어했다" → "클리어")
//!    → 0.90. The longest entry wins; two domains tied on the same length → no exact verdict.
//! 2. **bias / prejudice vectors** — `bias` = max-pool cosine against the node's bias phrases (times the phrase
//!    weight), `prej` = max-pool cosine against its prejudice phrases (look-alikes that mean something else),
//!    `score = max(bias − prej·w, 0)` with `w = 0.3` for ≤ 2-word spans and `0.7` otherwise.
//!
//! The dictionary's `function` group (request endings, fillers) explains words away: a span made only of them
//! never names a domain.

use crate::analyze::Analyzer;
use crate::embed::{cosine, TextEmbedder};
use crate::korean::is_hangul;
use crate::normalize::{compact, normalize, Word};
use serde::Serialize;
use std::collections::HashSet;

/// How a domain was caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Hit {
    Exact,
    Prefix,
    Vector,
}

pub const EXACT_SCORE: f32 = 0.95;
pub const PREFIX_SCORE: f32 = 0.90;

/// 은행 항목 하나를 만드는 재료: 키, 부르는 쪽의 내용, bias 글(무게), prejudice 글(닮았지만 다른 뜻), 정확 일치 글
#[derive(Debug, Clone, PartialEq)]
pub struct DomainSpec<M> {
    pub key: String,
    pub payload: M,
    pub bias: Vec<(String, f32)>,
    pub prejudice: Vec<String>,
    pub exact: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DomainEntry<M> {
    pub key: String,
    pub payload: M,
    bias: Vec<(Vec<f32>, f32)>,
    prej: Vec<Vec<f32>>,
    /// `(compact form, word count)` of each exact-match entry.
    exact: Vec<(String, usize)>,
}

pub struct DomainBank<M, E: TextEmbedder> {
    entries: Vec<DomainEntry<M>>,
    function: HashSet<String>,
    pub embedder: E,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DomainScore {
    /// Index into the bank (`DomainBank::entry`).
    pub index: usize,
    pub score: f32,
    pub bias: f32,
    pub prej: f32,
    pub hit: Hit,
}

impl<M: Clone, E: TextEmbedder> DomainBank<M, E> {
    /// `specs` 의 항목과 `function_phrases`(요청 어미 · 군말: 이것만으로 된 구간은 도메인을 부르지 않는다)로 만든다.
    pub fn from_specs(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E) -> Self {
        Self::build(specs, function_phrases, embedder, false)
    }

    /// [`Self::from_specs`] 와 같되, 각 글을 내용어만으로 임베딩한다(규칙 분석기가 요청 어미 · 보조 용언을 뗀다:
    /// "리본 달아 봐" → "리본 달아"). 분석기를 돌릴 때 구간도 그렇게 임베딩되므로, 같은 무리의 공통 어미가 글을 닮게 하지 않는다.
    pub fn from_specs_content(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E) -> Self {
        Self::build(specs, function_phrases, embedder, true)
    }

    fn build(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E, content_only: bool) -> Self {
        // Every bias / prejudice phrase in one batch (one forward pass for a neural embedder).
        let phrases: Vec<String> = specs
            .iter()
            .flat_map(|n| n.bias.iter().map(|(t, _)| t.clone()).chain(n.prejudice.iter().cloned()))
            .map(|p| {
                if content_only {
                    let mut n = normalize(&p);
                    crate::analyze::RuleAnalyzer.analyze(&mut n);
                    Self::span_text(&n.words)
                } else {
                    p
                }
            })
            .collect();
        let refs: Vec<&str> = phrases.iter().map(String::as_str).collect();
        let mut vecs = embedder.embed_batch(&refs).into_iter();
        let entries = specs
            .into_iter()
            .map(|n| DomainEntry {
                bias: n.bias.iter().map(|(_, w)| (vecs.next().unwrap_or_default(), *w)).collect(),
                prej: n.prejudice.iter().map(|_| vecs.next().unwrap_or_default()).collect(),
                exact: n
                    .exact
                    .iter()
                    .map(|e| (compact(&normalize(e).text), normalize(e).words.len().max(1)))
                    .filter(|(c, _)| !c.is_empty())
                    .collect(),
                key: n.key,
                payload: n.payload,
            })
            .collect();
        let function = function_phrases
            .iter()
            .flat_map(|p| {
                let n = normalize(p);
                let whole = compact(&n.text);
                n.words.into_iter().map(|w| compact(&w.surface)).chain(std::iter::once(whole))
            })
            .filter(|w| !w.is_empty())
            .collect();
        Self {
            entries,
            function,
            embedder,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entry(&self, i: usize) -> &DomainEntry<M> {
        &self.entries[i]
    }
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }

    /// A word the function bank explains away (request ending, filler).
    pub fn is_function_word(&self, w: &Word) -> bool {
        [&w.surface, &w.lemma, &w.stem].iter().any(|f| self.function.contains(&compact(f)))
    }

    /// Whether a span can name a domain: at least one content word (analyzer role) that the function bank does
    /// not explain away.
    pub fn eligible(&self, words: &[Word]) -> bool {
        words.iter().any(|w| w.is_content() && !self.is_function_word(w))
    }

    /// The text a span is embedded as: its content words (all words when it has none).
    pub fn span_text(words: &[Word]) -> String {
        let content: Vec<&str> = words.iter().filter(|w| w.is_content()).map(|w| w.surface.as_str()).collect();
        if content.is_empty() {
            words.iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ")
        } else {
            content.join(" ")
        }
    }

    /// STEP 1 (logis-center): exact or agglutinative-prefix match of a span against every `exact_match` entry.
    pub fn exact(&self, words: &[Word]) -> Option<(usize, Hit)> {
        if words.is_empty() {
            return None;
        }
        let n = words.len();
        let head: String = words[..n - 1].iter().map(|w| compact(&w.surface)).collect();
        let last = &words[n - 1];
        // The span with its last word as written, josa-peeled, request-stripped, or as the analyzer's lemma.
        let forms: Vec<String> = [Some(&last.surface), Some(&last.lemma), Some(&last.stem), last.alt.as_ref()]
            .into_iter()
            .flatten()
            .map(|f| format!("{head}{}", compact(f)))
            .collect();
        let surface = &forms[0];
        // (entry, matched length, exact beats prefix)
        let mut hits: Vec<(usize, usize, Hit)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            for (ec, ew) in &e.exact {
                let len = ec.chars().count();
                // Spacing does not matter for an exact hit ("종료해 줘" = "종료해줘"); a prefix only extends the
                // span's last word, so the entry must have as many words as the span.
                if forms.iter().any(|f| f == ec) {
                    hits.push((i, len, Hit::Exact));
                } else if *ew == n && len >= 2 && surface.starts_with(ec.as_str()) && ec.chars().last().is_some_and(is_hangul) {
                    let rest: Vec<char> = surface[ec.len()..].chars().collect();
                    if !rest.is_empty() && rest.len() <= 2 && rest.iter().all(|c| is_hangul(*c)) {
                        hits.push((i, len, Hit::Prefix));
                    }
                }
            }
        }
        let rank = |h: &(usize, usize, Hit)| (h.1, h.2 == Hit::Exact);
        let top = hits.iter().map(rank).max()?;
        let mut winners: Vec<&(usize, usize, Hit)> = hits.iter().filter(|h| rank(h) == top).collect();
        winners.dedup_by_key(|h| h.0);
        // The longest entry wins; two domains tied on it give no exact verdict (logis-center).
        (winners.len() == 1).then(|| (winners[0].0, winners[0].2))
    }

    /// Every domain scored for each span, in bank order (`out[span][domain]`): the spans' content texts are
    /// embedded in one batch, exact / prefix hits override their domain's vector score.
    pub fn raw_scores(&self, spans: &[&[Word]]) -> Vec<Vec<DomainScore>> {
        let texts: Vec<String> = spans.iter().map(|s| Self::span_text(s)).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vecs = self.embedder.embed_batch(&refs);
        spans.iter().zip(&vecs).map(|(s, v)| self.scores_for(s, v)).collect()
    }

    /// Every domain scored for `words` (a span), best first.
    pub fn score_words(&self, words: &[Word]) -> Vec<DomainScore> {
        let v = self.embedder.embed(&Self::span_text(words));
        let mut out = self.scores_for(words, &v);
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out
    }

    fn scores_for(&self, words: &[Word], v: &[f32]) -> Vec<DomainScore> {
        let pw = if words.len() <= 2 { 0.3 } else { 0.7 };
        let exact = self.exact(words);
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let bias = e.bias.iter().map(|(b, w)| cosine(v, b) * w).fold(0f32, f32::max);
                let prej = e.prej.iter().map(|p| cosine(v, p)).fold(0f32, f32::max);
                let mut s = DomainScore {
                    index: i,
                    score: (bias - prej * pw).max(0.0),
                    bias,
                    prej,
                    hit: Hit::Vector,
                };
                if let Some((j, h)) = exact {
                    if j == i {
                        // An exact / prefix match confirms the domain whatever the vectors say.
                        let x = if h == Hit::Exact { EXACT_SCORE } else { PREFIX_SCORE };
                        s.score = s.score.max(x);
                        s.hit = h;
                    }
                }
                s
            })
            .collect()
    }

    /// Every domain scored for free text, best first.
    pub fn score(&self, text: &str) -> Vec<DomainScore> {
        self.score_words(&normalize(text).words)
    }

    /// The best domain of a span, unless the span has no content word.
    pub fn best(&self, words: &[Word]) -> Option<DomainScore> {
        if !self.eligible(words) {
            return None;
        }
        self.score_words(words).into_iter().next()
    }

    /// Exact tokens shared across two or more distinct domains in this bank (ambiguous vocabulary mask).
    pub fn cross_domain_mask(&self) -> HashSet<String> {
        let mut owner_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for e in &self.entries {
            let mut per_entry: HashSet<String> = HashSet::new();
            for (ex, _) in &e.exact {
                per_entry.insert(ex.clone());
            }
            for tok in per_entry {
                *owner_counts.entry(tok).or_insert(0) += 1;
            }
        }
        owner_counts
            .into_iter()
            .filter_map(|(tok, cnt)| (cnt >= 2).then_some(tok))
            .collect()
    }

    /// Bank-neutral score normalization: divides centered scores by overall standard deviation
    /// (`net_sd`) and subtracts the Gumbel expected extreme-value maximum `sqrt(2 * ln(N))`.
    pub fn bank_neutral_scores(raw: &[DomainScore]) -> Vec<f32> {
        let n = raw.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![raw[0].score];
        }
        let nf = n as f64;
        let mean = raw.iter().map(|r| r.score as f64).sum::<f64>() / nf;
        let var = raw.iter().map(|r| (r.score as f64 - mean).powi(2)).sum::<f64>() / nf;
        let net_sd = var.sqrt().max(1e-6);
        let gumbel = (2.0 * nf.ln()).sqrt();
        raw.iter()
            .map(|r| (((r.score as f64 - mean) / net_sd) - gumbel) as f32)
            .collect()
    }
}

/// Configuration for [`BiasedTieredBank`] sector-biased cosine matching with unit-sphere cold pruning.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TieredBankConfig {
    /// Prior bonus added to active-sector top-1 score (`S_gate = s_q8_best + beta_sector`).
    pub beta_sector: f32,
    /// Conservative noise margin subtracted from cold-tier (`Q4_K`) scores (`s_q4 - delta_q4 > S_gate`).
    pub delta_q4: f32,
    /// Minimum absolute score floor required for an accepted verdict.
    pub abs_floor: f32,
    /// Minimum top-1 minus top-2 separation margin for high-confidence acceptance.
    pub margin_high: f32,
    /// Number of unit-sphere clusters for cold-tier pruning (default `64`).
    pub cold_clusters: usize,
    /// Prejudice penalty weight `w_prej` in `max(bias - w_prej * prej, 0)`.
    pub prejudice_weight: f32,
}

impl Default for TieredBankConfig {
    fn default() -> Self {
        Self {
            beta_sector: 0.035,
            delta_q4: 0.012,
            abs_floor: 0.50,
            margin_high: 0.08,
            cold_clusters: 64,
            prejudice_weight: 0.50,
        }
    }
}

/// One entry in a sector-tiered cosine bank (generic, domain-literal-free, `wasm32`-clean).
#[derive(Debug, Clone)]
pub struct TieredEntry<M> {
    pub key: String,
    pub sector: u32,
    /// Priority-pinned entries (e.g. safety / privacy) are always scored in the active high-precision tier
    /// and are exempt from any sector penalty.
    pub priority_pinned: bool,
    pub payload: M,
    /// L2-normalized bias prototype vectors with weights.
    pub bias_unit: Vec<(Vec<f32>, f32)>,
    /// L2-normalized prejudice prototype vectors.
    pub prej_unit: Vec<Vec<f32>>,
}

#[derive(Debug, Clone)]
struct ColdCluster {
    center: Vec<f32>,
    radius: f32,
    /// Indices into `BiasedTieredBank::entries` belonging to this cluster.
    entry_indices: Vec<usize>,
    /// Maximum phrase weight across bias vectors in this cluster.
    max_weight: f32,
}

/// Output of [`BiasedTieredBank::match_unit`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TieredMatchOutcome {
    pub best_index: Option<usize>,
    pub best_score: f32,
    pub second_score: f32,
    pub accepted: bool,
    pub rescued_from_cold: bool,
    pub clusters_checked: usize,
    pub clusters_opened: usize,
    pub cold_entries_scored: usize,
    /// Variance-isolated z-scores (`(entry_index, z_score)`), normalized strictly on active-tier statistics.
    pub neutral_z: Vec<(usize, f32)>,
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt();
    if n > 1e-12 && n.is_finite() {
        v.iter().map(|&x| (x as f64 / n) as f32).collect()
    } else {
        vec![0.0; v.len()]
    }
}

#[inline]
fn dot_unit(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(&x, &y)| x * y).sum()
}

/// Sector-biased 2-tier cosine matching bank (`wasm32`-clean):
/// 1. Scores active-sector entries + `priority_pinned` entries first at full precision.
/// 2. Prunes inactive-sector clusters via spherical Cauchy-Schwarz bound `c_m . q + rho_m - delta_q4 <= S_gate`.
/// 3. Selectively rescues crossing cold clusters and projects rescued scores onto active-tier `(mu, sd)`.
pub struct BiasedTieredBank<M> {
    dim: usize,
    entries: Vec<TieredEntry<M>>,
    cfg: TieredBankConfig,
}

impl<M: Clone> BiasedTieredBank<M> {
    pub fn new(dim: usize, entries: Vec<TieredEntry<M>>, cfg: TieredBankConfig) -> Self {
        let normalized = entries
            .into_iter()
            .map(|mut e| {
                e.bias_unit = e
                    .bias_unit
                    .into_iter()
                    .map(|(v, w)| (l2_normalize(&v), w))
                    .collect();
                e.prej_unit = e.prej_unit.into_iter().map(|v| l2_normalize(&v)).collect();
                e
            })
            .collect();
        Self { dim, entries: normalized, cfg }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, idx: usize) -> &TieredEntry<M> {
        &self.entries[idx]
    }

    pub fn config(&self) -> &TieredBankConfig {
        &self.cfg
    }

    fn score_entry(&self, idx: usize, q_unit: &[f32]) -> f32 {
        let e = &self.entries[idx];
        let bias = e
            .bias_unit
            .iter()
            .map(|(b, w)| dot_unit(q_unit, b) * w)
            .fold(0.0f32, f32::max);
        let prej = e
            .prej_unit
            .iter()
            .map(|p| dot_unit(q_unit, p))
            .fold(0.0f32, f32::max);
        (bias - self.cfg.prejudice_weight * prej).max(0.0)
    }

    /// Build deterministic unit-sphere clusters over all inactive-sector entries (excluding `priority_pinned`).
    fn build_cold_clusters(&self, active_sector: u32) -> Vec<ColdCluster> {
        // Collect (entry_idx, unit_vec, weight) for every bias vector of every cold entry.
        let mut pts: Vec<(usize, &[f32], f32)> = Vec::new();
        for (idx, e) in self.entries.iter().enumerate() {
            if e.priority_pinned || e.sector == active_sector {
                continue;
            }
            for (v, w) in &e.bias_unit {
                if v.len() == self.dim {
                    pts.push((idx, v.as_slice(), *w));
                }
            }
        }
        if pts.is_empty() {
            return Vec::new();
        }
        let k = self.cfg.cold_clusters.clamp(1, pts.len());
        let mut centers: Vec<Vec<f32>> = (0..k)
            .map(|c| pts[c * pts.len() / k].1.to_vec())
            .collect();
        let mut assign = vec![0usize; pts.len()];
        for it in 0..=4 {
            for (pi, &(_, v, _)) in pts.iter().enumerate() {
                let mut best = (f32::NEG_INFINITY, 0usize);
                for (ci, cv) in centers.iter().enumerate() {
                    let sim = dot_unit(v, cv);
                    if sim > best.0 {
                        best = (sim, ci);
                    }
                }
                assign[pi] = best.1;
            }
            if it == 4 {
                break;
            }
            let mut sums = vec![vec![0.0f64; self.dim]; k];
            let mut cnts = vec![0usize; k];
            for (pi, &ci) in assign.iter().enumerate() {
                cnts[ci] += 1;
                for (s, &v) in sums[ci].iter_mut().zip(pts[pi].1) {
                    *s += v as f64;
                }
            }
            for ci in 0..k {
                if cnts[ci] > 0 {
                    let mean: Vec<f32> = sums[ci].iter().map(|&s| (s / cnts[ci] as f64) as f32).collect();
                    centers[ci] = mean;
                }
            }
        }
        let mut radii = vec![0.0f32; k];
        let mut max_w = vec![0.0f32; k];
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); k];
        for (pi, &ci) in assign.iter().enumerate() {
            let (entry_idx, v, w) = pts[pi];
            let dist = v
                .iter()
                .zip(&centers[ci])
                .map(|(&a, &b)| ((a as f64) - (b as f64)).powi(2))
                .sum::<f64>()
                .sqrt() as f32;
            radii[ci] = radii[ci].max(dist);
            max_w[ci] = max_w[ci].max(w);
            if !buckets[ci].contains(&entry_idx) {
                buckets[ci].push(entry_idx);
            }
        }
        buckets
            .into_iter()
            .enumerate()
            .filter_map(|(ci, entry_indices)| {
                if entry_indices.is_empty() {
                    None
                } else {
                    Some(ColdCluster {
                        center: centers[ci].clone(),
                        radius: radii[ci] * (1.0 + 1e-4) + 1e-6,
                        entry_indices,
                        max_weight: max_w[ci],
                    })
                }
            })
            .collect()
    }

    /// Match a query vector against the active sector (+ priority-pinned entries) first,
    /// then prune or rescue inactive-sector clusters with `(beta_sector, delta_q4)`.
    pub fn match_query(&self, query: &[f32], active_sector: u32) -> TieredMatchOutcome {
        let q_unit = l2_normalize(query);
        // Step 1: Active sector + priority-pinned high-precision scoring
        let mut active_scores: Vec<(usize, f32)> = Vec::new();
        for (idx, e) in self.entries.iter().enumerate() {
            if e.priority_pinned || e.sector == active_sector {
                active_scores.push((idx, self.score_entry(idx, &q_unit)));
            }
        }
        let mut best_idx: Option<usize> = None;
        let mut best_score = 0.0f32;
        let mut second_score = 0.0f32;
        for &(idx, s) in &active_scores {
            if best_idx.is_none() || s > best_score {
                if best_idx.is_some() {
                    second_score = best_score;
                }
                best_score = s;
                best_idx = Some(idx);
            } else if s > second_score {
                second_score = s;
            }
        }

        // Variance-isolated active statistics (mu_act, sd_act) computed strictly on active tier.
        let (mu_act, sd_act) = if active_scores.len() >= 2 {
            let nf = active_scores.len() as f64;
            let mean = active_scores.iter().map(|(_, s)| *s as f64).sum::<f64>() / nf;
            let var = active_scores
                .iter()
                .map(|(_, s)| (*s as f64 - mean).powi(2))
                .sum::<f64>()
                / nf;
            (mean, var.sqrt().max(1e-6))
        } else {
            (0.0, 1.0)
        };

        let s_gate = best_score + self.cfg.beta_sector;

        // Step 2: Spherical Cauchy-Schwarz cluster guard over cold entries
        let clusters = self.build_cold_clusters(active_sector);
        let clusters_checked = clusters.len();
        let mut clusters_opened = 0usize;
        let mut cold_entries_scored = 0usize;
        let mut rescued_from_cold = false;
        let mut rescued_scores: Vec<(usize, f32)> = Vec::new();
        let mut scored_cold_set: HashSet<usize> = HashSet::new();

        for cl in &clusters {
            let ub = (dot_unit(&cl.center, &q_unit) + cl.radius + 1e-4) * cl.max_weight;
            if ub - self.cfg.delta_q4 <= s_gate {
                continue;
            }
            clusters_opened += 1;
            for &entry_idx in &cl.entry_indices {
                if !scored_cold_set.insert(entry_idx) {
                    continue;
                }
                cold_entries_scored += 1;
                let s_cold = self.score_entry(entry_idx, &q_unit);
                rescued_scores.push((entry_idx, s_cold));
                let eff_cold = s_cold - self.cfg.delta_q4;
                if eff_cold > s_gate && eff_cold > best_score {
                    second_score = best_score;
                    best_score = s_cold;
                    best_idx = Some(entry_idx);
                    rescued_from_cold = true;
                } else if s_cold > second_score && Some(entry_idx) != best_idx {
                    second_score = s_cold;
                }
            }
        }

        // Step 3: Variance-isolated projection of active + rescued entries onto (mu_act, sd_act)
        let mut neutral_z: Vec<(usize, f32)> = active_scores
            .iter()
            .chain(rescued_scores.iter())
            .map(|&(idx, s)| (idx, (((s as f64) - mu_act) / sd_act) as f32))
            .collect();
        neutral_z.sort_by(|a, b| b.1.total_cmp(&a.1));

        let accepted = best_idx.is_some()
            && best_score >= self.cfg.abs_floor
            && (best_score - second_score) >= self.cfg.margin_high;

        TieredMatchOutcome {
            best_index: best_idx,
            best_score,
            second_score,
            accepted,
            rescued_from_cold,
            clusters_checked,
            clusters_opened,
            cold_entries_scored,
            neutral_z,
        }
    }
}

/// What [`BankBackend::best`] found
#[derive(Debug, Clone, PartialEq)]
pub struct BankMatch<M> {
    pub key: String,
    pub payload: M,
    /// [`EXACT_SCORE`] · [`PREFIX_SCORE`] for lexical hits; the cosine score otherwise (calibrated when the backend has
    /// a scale)
    pub score: f32,
    pub hit: Hit,
    /// Passed the backend's own gate. [`BankBackend::Full`] has none (always `true`; callers keep their `accept`);
    /// [`BankBackend::BiasedTiered`] uses the tiered floor and margin.
    pub accepted: bool,
    /// Won from an inactive sector's cold tier
    pub rescued_from_cold: bool,
}

/// One scoring interface over the plain bank and the sector-tiered bank, so a caller switches backends without
/// changing what it stores or sends.
pub enum BankBackend<M, E: TextEmbedder> {
    /// Exact / agglutinative-prefix hits, then the best bias − prejudice cosine over every entry
    Full(DomainBank<M, E>),
    /// Exact / prefix hits on `lexical` (0.95 / 0.90 as before), then the sector-tiered cosine stage on `tiered`
    /// (active sector + priority-pinned entries at full precision, inactive sectors behind the cold-cluster guard)
    BiasedTiered {
        lexical: DomainBank<M, E>,
        tiered: BiasedTieredBank<M>,
        /// `Some(knots)`: tiered scores are reported on the calibrated scale
        /// ([`calibrate_knots`](premium_discount_sds::knots::calibrate_knots)) and the tiered `abs_floor` /
        /// `margin_high` are read on that scale. Pruning inside the tiered bank stays on raw cosines.
        scale_knots: Option<&'static [(f32, f32)]>,
    },
}

impl<M: Clone, E: TextEmbedder> BankBackend<M, E> {
    /// The lexical bank (exact / prefix matching, function words, the embedder)
    pub fn lexical(&self) -> &DomainBank<M, E> {
        match self {
            BankBackend::Full(bank) => bank,
            BankBackend::BiasedTiered { lexical, .. } => lexical,
        }
    }

    /// The best entry for a span, `None` when the span has no content word (or the tiered stage finds none).
    /// `active_sector` is ignored by [`BankBackend::Full`].
    pub fn best(&self, words: &[Word], active_sector: u32) -> Option<BankMatch<M>> {
        match self {
            BankBackend::Full(bank) => {
                let s = bank.best(words)?;
                let e = bank.entry(s.index);
                Some(BankMatch {
                    key: e.key.clone(),
                    payload: e.payload.clone(),
                    score: s.score,
                    hit: s.hit,
                    accepted: true,
                    rescued_from_cold: false,
                })
            }
            BankBackend::BiasedTiered { lexical, tiered, scale_knots } => {
                if !lexical.eligible(words) {
                    return None;
                }
                if let Some((i, hit)) = lexical.exact(words) {
                    let e = lexical.entry(i);
                    return Some(BankMatch {
                        key: e.key.clone(),
                        payload: e.payload.clone(),
                        score: if hit == Hit::Exact { EXACT_SCORE } else { PREFIX_SCORE },
                        hit,
                        accepted: true,
                        rescued_from_cold: false,
                    });
                }
                let q = lexical.embedder.embed(&DomainBank::<M, E>::span_text(words));
                let out = tiered.match_query(&q, active_sector);
                let idx = out.best_index?;
                let (score, accepted) = match scale_knots {
                    None => (out.best_score, out.accepted),
                    Some(k) => {
                        use premium_discount_sds::knots::calibrate_knots;
                        let (s1, s2) = (calibrate_knots(out.best_score, k), calibrate_knots(out.second_score, k));
                        let cfg = tiered.config();
                        (s1, s1 >= cfg.abs_floor && s1 - s2 >= cfg.margin_high)
                    }
                };
                let e = tiered.entry(idx);
                Some(BankMatch {
                    key: e.key.clone(),
                    payload: e.payload.clone(),
                    score,
                    hit: Hit::Vector,
                    accepted,
                    rescued_from_cold: out.rescued_from_cold,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashedNgramEmbedder;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Act {
        Quit,
        Cheer,
        Stock,
    }

    fn bank() -> DomainBank<Act, HashedNgramEmbedder> {
        let spec = |key: &str, payload: Act, bias: &[&str], prej: &[&str], exact: &[&str]| DomainSpec {
            key: key.into(),
            payload,
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: prej.iter().map(|p| p.to_string()).collect(),
            exact: exact.iter().map(|e| e.to_string()).collect(),
        };
        DomainBank::from_specs(
            vec![
                spec("system.exit", Act::Quit, &["앱 꺼 줘", "종료해 줘"], &["게임 종료 화면"], &["종료해 줘", "꺼 줘"]),
                spec("emotion.celebrate", Act::Cheer, &["이겼다", "클리어했다"], &[], &["클리어"]),
                spec("market.down", Act::Stock, &["주가 떨어졌어"], &[], &["떡락"]),
            ],
            &["해 줘".to_string(), "좀".to_string(), "진짜".to_string()],
            HashedNgramEmbedder::default(),
        )
    }

    #[test]
    fn exact_and_agglutinative_prefix_matches_come_before_vectors() {
        let b = bank();
        let w = |t: &str| normalize(t).words;
        let key = |r: Option<(usize, Hit)>| r.map(|(i, h)| (b.entry(i).key.clone(), h));
        assert_eq!(key(b.exact(&w("클리어"))), Some(("emotion.celebrate".into(), Hit::Exact)));
        assert_eq!(key(b.exact(&w("클리어했다"))), Some(("emotion.celebrate".into(), Hit::Prefix)), "two syllables of ending");
        assert_eq!(key(b.exact(&w("떡락했어"))), Some(("market.down".into(), Hit::Prefix)));
        assert_eq!(key(b.exact(&w("종료해 줘"))), Some(("system.exit".into(), Hit::Exact)), "spaces are ignored");
        assert_eq!(b.exact(&w("클리어하였습니다")), None, "more than two syllables left over");
        assert_eq!(b.exact(&w("clearance")), None, "prefix matching is for Hangul only");
        assert_eq!(b.entry(b.score("떡락 가즈아")[0].index).payload, Act::Stock);
    }

    #[test]
    fn function_words_alone_name_nothing_and_the_payload_is_carried() {
        let b = bank();
        assert!(b.best(&normalize("해 줘").words).is_none());
        assert!(b.best(&normalize("좀 진짜").words).is_none());
        let best = b.best(&normalize("종료해 줘").words).unwrap();
        assert_eq!(b.entry(best.index).payload, Act::Quit);
        assert_eq!(b.index_of("market.down"), Some(2));
    }

    #[test]
    fn cross_domain_mask_and_bank_neutral_gumbel() {
        let spec = |key: &str, payload: Act, exact: &[&str]| DomainSpec {
            key: key.into(),
            payload,
            bias: vec![("shared".into(), 1.0)],
            prejudice: vec![],
            exact: exact.iter().map(|e| e.to_string()).collect(),
        };
        let b = DomainBank::from_specs(
            vec![
                spec("a", Act::Quit, &["shared", "alpha"]),
                spec("b", Act::Cheer, &["shared", "beta"]),
            ],
            &[],
            HashedNgramEmbedder::default(),
        );
        let mask = b.cross_domain_mask();
        assert!(mask.contains("shared") && !mask.contains("alpha"));
        let scores = vec![
            DomainScore { index: 0, score: 0.9, bias: 0.9, prej: 0.0, hit: Hit::Vector },
            DomainScore { index: 1, score: 0.3, bias: 0.3, prej: 0.0, hit: Hit::Vector },
        ];
        let z = DomainBank::<Act, HashedNgramEmbedder>::bank_neutral_scores(&scores);
        assert!(z[0] > z[1] && z[0].is_finite() && z[1].is_finite());
    }

    #[test]
    fn biased_tiered_bank_prunes_cold_clusters_and_rescues_true_out_of_sector_hit() {
        let entries = vec![
            TieredEntry {
                key: "act_0".into(),
                sector: 1,
                priority_pinned: false,
                payload: 10u32,
                bias_unit: vec![(vec![1.0, 0.0, 0.0, 0.0], 1.0)],
                prej_unit: vec![],
            },
            TieredEntry {
                key: "act_1".into(),
                sector: 1,
                priority_pinned: false,
                payload: 11u32,
                bias_unit: vec![(vec![0.0, 1.0, 0.0, 0.0], 1.0)],
                prej_unit: vec![],
            },
            TieredEntry {
                key: "priority_guard".into(),
                sector: 99,
                priority_pinned: true,
                payload: 99u32,
                bias_unit: vec![(vec![0.0, 0.0, 1.0, 0.0], 1.0)],
                prej_unit: vec![],
            },
            TieredEntry {
                key: "cold_0".into(),
                sector: 2,
                priority_pinned: false,
                payload: 20u32,
                bias_unit: vec![(vec![0.0, 0.0, 0.0, 1.0], 1.0)],
                prej_unit: vec![],
            },
        ];
        let bank = BiasedTieredBank::new(4, entries, TieredBankConfig::default());

        // 1) Active-sector hit prunes cold cluster completely (0 cold entries scored)
        let r_act = bank.match_query(&[0.98, 0.1, 0.0, 0.0], 1);
        assert_eq!(r_act.best_index, Some(0));
        assert!(r_act.accepted);
        assert!(!r_act.rescued_from_cold);
        assert_eq!(r_act.cold_entries_scored, 0);

        // 2) Priority-pinned entry wins immediately regardless of active sector
        let r_pri = bank.match_query(&[0.0, 0.0, 1.0, 0.0], 1);
        assert_eq!(r_pri.best_index, Some(2));
        assert!(!r_pri.rescued_from_cold);
        assert_eq!(r_pri.cold_entries_scored, 0);

        // 3) Out-of-sector query punches through gate and rescues cold_0
        let r_cold = bank.match_query(&[0.05, 0.0, 0.0, 0.99], 1);
        assert_eq!(r_cold.best_index, Some(3));
        assert!(r_cold.rescued_from_cold);
        assert_eq!(r_cold.cold_entries_scored, 1);
    }

    /// Calibrated scale that halves positive scores (raw 1.0 → 0.5)
    static HALF: [(f32, f32); 3] = [(-1.0, -1.0), (0.0, 0.0), (1.0, 0.5)];

    fn tiered_of(lexical: &DomainBank<Act, HashedNgramEmbedder>, sectors: &[u32]) -> BiasedTieredBank<Act> {
        let phrases: [&[&str]; 3] = [&["앱 꺼 줘", "종료해 줘"], &["이겼다", "클리어했다"], &["주가 떨어졌어"]];
        let entries = (0..lexical.len())
            .map(|i| {
                let e = lexical.entry(i);
                TieredEntry {
                    key: e.key.clone(),
                    sector: sectors[i],
                    priority_pinned: false,
                    payload: e.payload,
                    bias_unit: phrases[i].iter().map(|p| (lexical.embedder.embed(&DomainBank::<Act, HashedNgramEmbedder>::span_text(&normalize(p).words)), 1.0)).collect(),
                    prej_unit: vec![],
                }
            })
            .collect();
        BiasedTieredBank::new(lexical.embedder.dim, entries, TieredBankConfig::default())
    }

    #[test]
    fn bank_backend_switches_full_and_tiered_without_changing_lexical_hits() {
        let full = BankBackend::Full(bank());
        let tiered = BankBackend::BiasedTiered { tiered: tiered_of(&bank(), &[0, 1, 2]), lexical: bank(), scale_knots: None };
        let w = |t: &str| normalize(t).words;
        // exact / prefix hits are the lexical bank's in both backends
        for b in [&full, &tiered] {
            let m = b.best(&w("종료해 줘"), 1).unwrap();
            assert_eq!((m.key.as_str(), m.hit, m.score, m.payload), ("system.exit", Hit::Exact, EXACT_SCORE, Act::Quit));
            assert_eq!(b.best(&w("클리어했다"), 0).unwrap().hit, Hit::Prefix);
            assert!(b.best(&w("해 줘 좀"), 0).is_none(), "function words alone name nothing");
        }
        // a vector query: the full bank scores every entry; the tiered bank rescues it from a cold sector
        let q = w("주가 떨어졌네");
        let f = full.best(&q, 0).unwrap();
        let t = tiered.best(&q, 0).unwrap();
        assert_eq!((f.key.as_str(), f.hit), ("market.down", Hit::Vector));
        assert_eq!((t.key.as_str(), t.hit, t.rescued_from_cold, t.accepted), ("market.down", Hit::Vector, true, true), "{t:?}");
        assert!((t.score - f.score).abs() < 1e-5, "same cosine, two backends: {} vs {}", t.score, f.score);
        // with a scale, the score is calibrated and the tiered floor is read on that scale
        let scaled = BankBackend::BiasedTiered { tiered: tiered_of(&bank(), &[0, 1, 2]), lexical: bank(), scale_knots: Some(&HALF) };
        let s = scaled.best(&q, 0).unwrap();
        assert!((s.score - t.score * 0.5).abs() < 1e-5);
        assert_eq!(s.accepted, s.score >= TieredBankConfig::default().abs_floor);
        assert_eq!(scaled.lexical().len(), 3);
    }
}
