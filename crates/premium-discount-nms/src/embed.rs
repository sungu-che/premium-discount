//! Text embedding.
//!
//! [`HashedNgramEmbedder`] is a deterministic, model-free embedder: signed feature hashing of
//! word lemmas, character bi/tri-grams and Hangul **jamo** tri-grams into 384 dimensions,
//! L2-normalised. It is lexical, not semantic — paraphrases that share no surface form score
//! low — so the router keeps it behind [`TextEmbedder`] and a neural encoder (e.g. pooled
//! Raon-1-nano hidden states) can replace it without touching the NMS code.
//!
//! NLP-NMS v2 plugs in [`GraniteEmbedder`] (feature `neural`): granite-embedding-97m-multilingual-r2 through
//! `zena-nlp`, batched per utterance and cached per text. [`SharedEmbedder`] lets the router keep one embedder
//! across dictionary reloads.

use crate::korean::{is_hangul, jamo};
use crate::normalize::normalize;
use std::sync::Arc;

pub trait TextEmbedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Several texts at once (one forward pass for neural embedders).
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
    /// The vector space, stored next to anything computed from these vectors (SDS ledgers, caches).
    fn space(&self) -> &str {
        "hash-ngram-v1"
    }
    /// Owned vector space identifier used to isolate per-embedder SDS scopes.
    fn space_tag(&self) -> String {
        self.space().to_string()
    }
}

/// One embedder shared by the router's bank and its semantic NMS, kept across dictionary reloads.
#[derive(Clone)]
pub struct SharedEmbedder(pub Arc<dyn TextEmbedder>);

impl SharedEmbedder {
    pub fn new(e: impl TextEmbedder + 'static) -> Self {
        Self(Arc::new(e))
    }
}

impl TextEmbedder for SharedEmbedder {
    fn dim(&self) -> usize {
        self.0.dim()
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        self.0.embed(text)
    }
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.0.embed_batch(texts)
    }
    fn space(&self) -> &str {
        self.0.space()
    }
    fn space_tag(&self) -> String {
        self.0.space_tag()
    }
}

/// granite-embedding-97m-multilingual-r2 (ModernBERT, 384-d, CLS) with a text → vector cache.
#[cfg(feature = "neural")]
pub struct GraniteEmbedder {
    model: premium_discount_nlp::granite::Granite,
    cache: std::sync::Mutex<std::collections::HashMap<String, Vec<f32>>>,
}

#[cfg(feature = "neural")]
impl GraniteEmbedder {
    /// Entries kept before the cache is cleared (span texts of recent utterances, dictionary phrases).
    const CACHE_MAX: usize = 20_000;

    pub fn new(model: premium_discount_nlp::granite::Granite) -> Self {
        Self {
            model,
            cache: Default::default(),
        }
    }

    /// Load from the folder `premium_discount_nlp::models::granite_dir` finds, on the CPU.
    pub fn load_default() -> anyhow::Result<Self> {
        let dir = premium_discount_nlp::models::granite_dir().ok_or_else(|| anyhow::anyhow!("granite model not installed"))?;
        Ok(Self::new(premium_discount_nlp::granite::Granite::load(&dir, &candle_core::Device::Cpu)?))
    }
}

#[cfg(feature = "neural")]
impl TextEmbedder for GraniteEmbedder {
    fn dim(&self) -> usize {
        self.model.dim()
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_batch(&[text]).pop().unwrap_or_default()
    }
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let missing: Vec<&str> = {
            let mut seen = std::collections::HashSet::new();
            texts
                .iter()
                .copied()
                .filter(|t| !cache.contains_key(*t) && seen.insert(*t))
                .collect()
        };
        if !missing.is_empty() {
            if cache.len() + missing.len() > Self::CACHE_MAX {
                cache.clear();
            }
            match self.model.embed_batch(&missing) {
                Ok(vs) => {
                    for (t, v) in missing.iter().zip(vs) {
                        cache.insert(t.to_string(), v);
                    }
                }
                Err(e) => {
                    eprintln!("[nlp] granite embedding failed: {e:#}");
                    return texts.iter().map(|_| vec![0.0; self.model.dim()]).collect();
                }
            }
        }
        texts.iter().map(|t| cache[*t].clone()).collect()
    }
    fn space(&self) -> &str {
        self.model.space_tag()
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let d = premium_discount_cpu_kernels::dot_f32_simd(a, b);
    let na = premium_discount_cpu_kernels::dot_f32_simd(a, a);
    let nb = premium_discount_cpu_kernels::dot_f32_simd(b, b);
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        d / (na.sqrt() * nb.sqrt())
    }
}

#[derive(Debug, Clone)]
pub struct HashedNgramEmbedder {
    pub dim: usize,
}

impl Default for HashedNgramEmbedder {
    fn default() -> Self {
        Self { dim: 384 }
    }
}

/// 64-bit FNV-1a of `s` (structural addresses, entity keys): stable across platforms and builds.
pub fn fnv1a_64(s: &str) -> u64 {
    fnv1a(s, 0)
}

fn fnv1a(s: &str, salt: u8) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ salt as u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl HashedNgramEmbedder {
    fn add(&self, v: &mut [f32], feat: &str, salt: u8, w: f32) {
        let h = fnv1a(feat, salt);
        let i = (h % self.dim as u64) as usize;
        v[i] += if h >> 63 == 0 { w } else { -w };
    }

    fn char_ngrams(&self, v: &mut [f32], s: &str, ns: &[usize], salt: u8, w: f32) {
        let cs: Vec<char> = std::iter::once('^').chain(s.chars()).chain(std::iter::once('$')).collect();
        for &n in ns {
            if cs.len() < n {
                continue;
            }
            for win in cs.windows(n) {
                let g: String = win.iter().collect();
                self.add(v, &g, salt + n as u8, w);
            }
        }
    }
}

impl TextEmbedder for HashedNgramEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let n = normalize(text);
        let mut v = vec![0f32; self.dim];
        for w in &n.words {
            self.add(&mut v, &w.lemma, 1, 1.0);
            if w.stem != w.lemma {
                self.add(&mut v, &w.stem, 1, 0.8);
            }
            let base = if w.stem.chars().count() < w.lemma.chars().count() {
                &w.stem
            } else {
                &w.lemma
            };
            self.char_ngrams(&mut v, base, &[2, 3], 10, 0.5);
            if base.chars().any(is_hangul) {
                self.char_ngrams(&mut v, &jamo(base), &[3], 20, 0.3);
            }
        }
        for pair in n.words.windows(2) {
            self.add(&mut v, &format!("{} {}", pair[0].lemma, pair[1].lemma), 30, 0.6);
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn similar_requests_are_closer_than_unrelated() {
        let e = HashedNgramEmbedder::default();
        let a = e.embed("보스 패턴 어떻게 피해");
        let b = e.embed("제나야 보스 패턴은 어떻게 피하지?");
        let c = e.embed("인벤토리에서 물약 찾아줘");
        assert!(cosine(&a, &b) > 0.5, "{}", cosine(&a, &b));
        assert!(cosine(&a, &b) > cosine(&a, &c) + 0.3);
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-5);
        assert_eq!(cosine(&a, &vec![0.0; 384]), 0.0);
    }
}
