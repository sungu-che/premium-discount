//! Synthetic N:N corpus audit: can a test scenario verify anything on this data?
//!
//! A relational search scenario is meaningless when the query already names its answer (the target document's own
//! number in the query text), when the answers are degenerate (empty, or always one document), when the same query
//! repeats, or when one template / language carries the whole set. [`audit_synthetic_corpus`] measures those and
//! returns [`SynthVerdict::Unverifiable`] with the reasons, so a corpus is rejected (and regenerated) before anyone
//! tunes on it. Split seeds are parameters: the auditor never holds a seed value of its own.
//!
//! Model-free, `wasm32-unknown-unknown` clean, no scenario literals.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One query of the corpus
#[derive(Debug, Clone, Copy)]
pub struct SynthQuery<'a> {
    pub id: &'a str,
    pub text: &'a str,
    /// Relevant document ids (the gold)
    pub relevant: &'a [&'a str],
    pub template: &'a str,
    pub lang: &'a str,
}

/// One document: the identifiers printed in it (its own number, …)
#[derive(Debug, Clone, Copy)]
pub struct SynthDoc<'a> {
    pub id: &'a str,
    pub own_ids: &'a [&'a str],
}

/// Pass lines
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SynthAuditCfg {
    /// Queries whose text contains a relevant document's own identifier (≤ this share)
    pub max_id_leak_rate: f64,
    /// Queries whose text names every relevant document by identifier (≤ this share): the answer is given away
    pub max_answer_leak_rate: f64,
    /// Queries with exactly one relevant document (≤ this share): relational templates should fan out
    pub max_single_relevant_rate: f64,
    /// Repeated query texts (≤ this share)
    pub max_duplicate_rate: f64,
    /// At least this many templates and languages
    pub min_templates: usize,
    pub min_langs: usize,
    /// Identifiers shorter than this are not looked for in the text (too many accidental hits)
    pub min_id_chars: usize,
}

impl Default for SynthAuditCfg {
    fn default() -> Self {
        SynthAuditCfg {
            max_id_leak_rate: 0.10,
            max_answer_leak_rate: 0.05,
            max_single_relevant_rate: 0.90,
            max_duplicate_rate: 0.02,
            min_templates: 3,
            min_langs: 1,
            min_id_chars: 4,
        }
    }
}

/// Whether the corpus can verify a scenario
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "verdict", content = "reasons")]
pub enum SynthVerdict {
    Usable,
    Unverifiable(Vec<String>),
}

/// Measurements of one corpus
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynthAuditReport {
    pub queries: usize,
    pub docs: usize,
    /// Share of queries whose text contains an own identifier of one of their relevant documents
    pub id_leak_rate: f64,
    /// Share of queries whose text names every relevant document
    pub answer_leak_rate: f64,
    /// Queries without a relevant document (always a defect)
    pub empty_relevant: usize,
    /// Relevant ids that are not documents of the corpus (always a defect)
    pub dangling_relevant: usize,
    pub single_relevant_rate: f64,
    pub mean_relevant: f64,
    pub duplicate_query_rate: f64,
    pub templates: usize,
    pub langs: usize,
    /// Leak rate per template (the template that gives answers away shows here)
    pub per_template_leak: BTreeMap<String, f64>,
    /// The holdout seed (when one is planned) differs from the development seed
    pub seed_isolated: bool,
    pub verdict: SynthVerdict,
}

fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Audits a corpus. `dev_seed` is the development split's seed, `sealed_seed` the sealed split's seed if one is planned
/// (only compared, never used to generate anything).
pub fn audit_synthetic_corpus(
    queries: &[SynthQuery<'_>],
    docs: &[SynthDoc<'_>],
    dev_seed: u64,
    sealed_seed: Option<u64>,
    cfg: &SynthAuditCfg,
) -> SynthAuditReport {
    let doc_ids: BTreeMap<&str, &SynthDoc<'_>> = docs.iter().map(|d| (d.id, d)).collect();
    let n = queries.len();
    let share = |k: usize| if n == 0 { 0.0 } else { k as f64 / n as f64 };
    let (mut leaks, mut answers, mut empty, mut dangling, mut single, mut total_relevant) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut per_template: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let (mut templates, mut langs) = (BTreeSet::new(), BTreeSet::new());
    for q in queries {
        templates.insert(q.template);
        langs.insert(q.lang);
        *seen.entry(normalized(q.text)).or_insert(0) += 1;
        let text = q.text.to_lowercase();
        let named = |rid: &str| {
            doc_ids.get(rid).is_some_and(|d| {
                d.own_ids.iter().any(|own| own.chars().count() >= cfg.min_id_chars && text.contains(&own.to_lowercase()))
            })
        };
        if q.relevant.is_empty() {
            empty += 1;
        }
        dangling += q.relevant.iter().filter(|r| !doc_ids.contains_key(*r)).count();
        if q.relevant.len() == 1 {
            single += 1;
        }
        total_relevant += q.relevant.len();
        let leak = q.relevant.iter().any(|r| named(r));
        let answer = !q.relevant.is_empty() && q.relevant.iter().all(|r| named(r));
        leaks += usize::from(leak);
        answers += usize::from(answer);
        let t = per_template.entry(q.template.to_string()).or_insert((0, 0));
        t.0 += 1;
        t.1 += usize::from(leak);
    }
    let duplicates: usize = seen.values().map(|&c| c.saturating_sub(1)).sum();
    let seed_isolated = sealed_seed.is_none_or(|s| s != dev_seed);
    let mut report = SynthAuditReport {
        queries: n,
        docs: docs.len(),
        id_leak_rate: share(leaks),
        answer_leak_rate: share(answers),
        empty_relevant: empty,
        dangling_relevant: dangling,
        single_relevant_rate: share(single),
        mean_relevant: if n == 0 { 0.0 } else { total_relevant as f64 / n as f64 },
        duplicate_query_rate: share(duplicates),
        templates: templates.len(),
        langs: langs.len(),
        per_template_leak: per_template.into_iter().map(|(k, (q, l))| (k, l as f64 / q as f64)).collect(),
        seed_isolated,
        verdict: SynthVerdict::Usable,
    };
    let mut reasons = Vec::new();
    if n == 0 || docs.is_empty() {
        reasons.push("empty corpus".to_string());
    }
    if report.id_leak_rate > cfg.max_id_leak_rate {
        reasons.push(format!("id_leak_rate {:.3} > {:.3}", report.id_leak_rate, cfg.max_id_leak_rate));
    }
    if report.answer_leak_rate > cfg.max_answer_leak_rate {
        reasons.push(format!("answer_leak_rate {:.3} > {:.3}", report.answer_leak_rate, cfg.max_answer_leak_rate));
    }
    if empty > 0 {
        reasons.push(format!("{empty} queries without a relevant document"));
    }
    if dangling > 0 {
        reasons.push(format!("{dangling} relevant ids are not documents"));
    }
    if report.single_relevant_rate > cfg.max_single_relevant_rate {
        reasons.push(format!("single_relevant_rate {:.3} > {:.3}", report.single_relevant_rate, cfg.max_single_relevant_rate));
    }
    if report.duplicate_query_rate > cfg.max_duplicate_rate {
        reasons.push(format!("duplicate_query_rate {:.3} > {:.3}", report.duplicate_query_rate, cfg.max_duplicate_rate));
    }
    if report.templates < cfg.min_templates {
        reasons.push(format!("{} templates < {}", report.templates, cfg.min_templates));
    }
    if report.langs < cfg.min_langs {
        reasons.push(format!("{} languages < {}", report.langs, cfg.min_langs));
    }
    if !seed_isolated {
        reasons.push("the sealed split shares the development seed".to_string());
    }
    if !reasons.is_empty() {
        report.verdict = SynthVerdict::Unverifiable(reasons);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corpus_that_quotes_its_answers_is_unverifiable_and_a_relational_one_is_usable() {
        let docs = [
            SynthDoc { id: "d1", own_ids: &["INV-0001"] },
            SynthDoc { id: "d2", own_ids: &["INV-0002"] },
            SynthDoc { id: "d3", own_ids: &["PO-0300"] },
        ];
        // half the queries quote the invoice number they are looking for
        let leaky = [
            SynthQuery { id: "q1", text: "show invoice INV-0001", relevant: &["d1"], template: "t1", lang: "en" },
            SynthQuery { id: "q2", text: "invoice inv-0002 please", relevant: &["d2"], template: "t1", lang: "en" },
            SynthQuery { id: "q3", text: "invoices of order PO-0300", relevant: &["d1", "d2"], template: "t2", lang: "en" },
            SynthQuery { id: "q4", text: "the order behind the late invoices", relevant: &["d3"], template: "t3", lang: "en" },
        ];
        let r = audit_synthetic_corpus(&leaky, &docs, 1, Some(2), &SynthAuditCfg::default());
        assert_eq!((r.id_leak_rate, r.answer_leak_rate), (0.5, 0.5));
        assert_eq!(r.per_template_leak["t1"], 1.0);
        assert!(matches!(&r.verdict, SynthVerdict::Unverifiable(why) if why.iter().any(|w| w.starts_with("id_leak_rate"))), "{r:?}");
        // the relational wording: answers reached through the order number, never named
        let relational = [
            SynthQuery { id: "q3", text: "invoices of order PO-0300", relevant: &["d1", "d2"], template: "t2", lang: "en" },
            SynthQuery { id: "q4", text: "the order behind the late invoices", relevant: &["d3"], template: "t3", lang: "en" },
            SynthQuery { id: "q5", text: "주문 PO-0300 의 계산서", relevant: &["d1", "d2"], template: "t2", lang: "ko" },
            SynthQuery { id: "q6", text: "paid invoices", relevant: &["d1", "d2"], template: "t1", lang: "en" },
        ];
        let ok = audit_synthetic_corpus(&relational, &docs, 1, Some(2), &SynthAuditCfg::default());
        assert_eq!(ok.verdict, SynthVerdict::Usable, "{ok:?}");
        assert_eq!((ok.templates, ok.langs), (3, 2));
        assert!((ok.mean_relevant - 1.75).abs() < 1e-12);
    }

    #[test]
    fn degenerate_answers_duplicates_and_shared_seeds_are_reasons() {
        let docs = [SynthDoc { id: "d1", own_ids: &["A-1"] }];
        let qs = [
            SynthQuery { id: "q1", text: "x", relevant: &[], template: "t", lang: "en" },
            SynthQuery { id: "q2", text: "X ", relevant: &["d9"], template: "t", lang: "en" },
        ];
        let r = audit_synthetic_corpus(&qs, &docs, 7, Some(7), &SynthAuditCfg::default());
        let SynthVerdict::Unverifiable(why) = &r.verdict else { panic!("{r:?}") };
        for want in ["without a relevant", "are not documents", "duplicate_query_rate", "templates <", "shares the development seed"] {
            assert!(why.iter().any(|w| w.contains(want)), "{want}: {why:?}");
        }
        assert_eq!(r.duplicate_query_rate, 0.5, "\"x\" and \"X \" are the same query");
        // short ids are not searched for (too many accidental hits)
        assert_eq!(r.id_leak_rate, 0.0);
        assert!(audit_synthetic_corpus(&[], &[], 1, None, &SynthAuditCfg::default()).verdict != SynthVerdict::Usable);
    }
}
