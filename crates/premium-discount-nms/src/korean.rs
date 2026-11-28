//! Closed-class Korean particle (josa) / copula peeling.
//!
//! Derived from Qntology/logis-center `utils/ai_utils.rs` (`NOMINAL_TAILS_ML`,
//! `COPULA_TAILS_ML`, `hangul_tail_agrees`, `closed_tail_lemma`; Apache-2.0, Copyright 2026
//! Qntology). Changes: Korean subset only; request/sentence endings for spoken commands added.

pub const NOMINAL_TAILS: &[&str] = &[
    "이", "가", "을", "를", "은", "는", "과", "와", "으로", "로", "의", "에", "에서", "도", "만", "들", "들을", "들의", "들이", "과의",
    "와의", // spoken additions
    "한테", "에게", "랑", "이랑", "까지", "부터",
];

/// Demonstratives and position words that point at one place on a screen or in a table before a one-syllable noun
/// ("이 칸", "그 줄", "아래 행"). Such a span names something although its content word has a single character.
pub const DEICTIC_HEADS: &[&str] = &["이", "그", "저", "위", "아래", "옆"];
/// One-syllable nouns of screen / table positions a demonstrative can point at.
pub const DEICTIC_SPATIAL_NOUNS: &[&str] = &["칸", "열", "행", "줄", "표", "창", "탭", "값", "키"];

/// `head` is a demonstrative or position word and `noun` (particle peeled: "칸에" → "칸") a one-syllable spatial noun.
pub fn is_deictic_spatial_pair(head: &str, noun: &str) -> bool {
    DEICTIC_HEADS.contains(&head) && DEICTIC_SPATIAL_NOUNS.contains(&noun_lemma(noun).as_str())
}

pub const COPULA_TAILS: &[&str] = &[
    "이고",
    "이며",
    "이면",
    "인데",
    "인지",
    "이다",
    "입니다",
    "이어야",
    "이라면",
    "이거나",
    "이야",
    "야",
];

/// Spoken request/sentence endings stripped from verb phrases ("숨어있어" → "숨어있", "꺼줘" → "꺼").
pub const REQUEST_ENDINGS: &[&str] = &[
    "해줄래",
    "해줘요",
    "해주세요",
    "해줘",
    "줄래",
    "줘요",
    "주세요",
    "줘",
    "할래",
    "하자",
    "해요",
    "해",
    "어요",
    "아요",
    "어",
    "아",
    "요",
];

fn last_jong(stem: &str) -> Option<u32> {
    let c = stem.chars().last()? as u32;
    (0xAC00..=0xD7A3).contains(&c).then(|| (c - 0xAC00) % 28)
}

/// Does `tail` agree with the final consonant (batchim) of `stem`?
pub fn hangul_tail_agrees(stem: &str, tail: &str) -> bool {
    let Some(jong) = last_jong(stem) else {
        return true;
    };
    let has_final = jong != 0;
    let rieul = jong == 8;
    match tail {
        "이" | "을" | "은" | "과" | "과의" | "이랑" => has_final,
        "가" | "를" | "는" | "와" | "와의" | "랑" => !has_final,
        "으로" => has_final && !rieul,
        "로" => !has_final || rieul,
        "야" => !has_final,
        "이야" => has_final,
        _ => true,
    }
}

fn longest_first(list: &[&'static str]) -> Vec<&'static str> {
    let mut v = list.to_vec();
    v.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    v
}

/// Peel at most two closed-class layers (`상품들을` → `상품`, `보스한테` → `보스`). Returns the word
/// unchanged when nothing agrees. One-syllable stems are allowed, as in the original.
pub fn noun_lemma(word: &str) -> String {
    let mut cur = word.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let tails: Vec<&str> = longest_first(NOMINAL_TAILS)
        .into_iter()
        .chain(longest_first(COPULA_TAILS))
        .collect();
    for _ in 0..2 {
        let mut peeled = false;
        for t in &tails {
            if let Some(stem) = cur.strip_suffix(t) {
                if !stem.is_empty() && hangul_tail_agrees(stem, t) {
                    cur = stem.to_string();
                    peeled = true;
                    break;
                }
            }
        }
        if !peeled {
            break;
        }
    }
    cur
}

/// Strip one spoken request ending from a predicate (`꺼줘` → `꺼`, `숨어있어` → `숨어있`).
pub fn predicate_stem(word: &str) -> String {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    for e in longest_first(REQUEST_ENDINGS) {
        if let Some(s) = w.strip_suffix(e) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    w.to_string()
}

pub fn is_hangul(c: char) -> bool {
    matches!(c as u32, 0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F)
}

/// Decompose Hangul syllables into compatibility jamo (초성/중성/종성) for typo-robust n-grams.
pub fn jamo(s: &str) -> String {
    const CHO: [char; 19] = [
        'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
    ];
    const JUNG: [char; 21] = [
        'ㅏ', 'ㅐ', 'ㅑ', 'ㅒ', 'ㅓ', 'ㅔ', 'ㅕ', 'ㅖ', 'ㅗ', 'ㅘ', 'ㅙ', 'ㅚ', 'ㅛ', 'ㅜ', 'ㅝ', 'ㅞ', 'ㅟ', 'ㅠ', 'ㅡ', 'ㅢ', 'ㅣ',
    ];
    const JONG: [&str; 28] = [
        "", "ㄱ", "ㄲ", "ㄳ", "ㄴ", "ㄵ", "ㄶ", "ㄷ", "ㄹ", "ㄺ", "ㄻ", "ㄼ", "ㄽ", "ㄾ", "ㄿ", "ㅀ", "ㅁ", "ㅂ", "ㅄ", "ㅅ", "ㅆ", "ㅇ",
        "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ",
    ];
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        let u = c as u32;
        if (0xAC00..=0xD7A3).contains(&u) {
            let i = u - 0xAC00;
            out.push(CHO[(i / 588) as usize]);
            out.push(JUNG[((i % 588) / 28) as usize]);
            out.push_str(JONG[(i % 28) as usize]);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn josa_agreement_and_peeling() {
        assert!(hangul_tail_agrees("보스", "가"));
        assert!(!hangul_tail_agrees("보스", "이"));
        assert!(hangul_tail_agrees("물약", "을"));
        assert!(hangul_tail_agrees("길", "로"));
        assert!(!hangul_tail_agrees("길", "으로"));
        assert_eq!(noun_lemma("물약은"), "물약");
        assert_eq!(noun_lemma("보스한테"), "보스");
        assert_eq!(noun_lemma("상품들을"), "상품");
        assert_eq!(noun_lemma("인벤토리에서"), "인벤토리");
        // "가방" ends in a batchim, so a "을" tail would be peeled, but "가" must not be.
        assert_eq!(noun_lemma("가방"), "가방");
    }

    #[test]
    fn request_endings_and_jamo() {
        assert_eq!(predicate_stem("꺼줘"), "꺼");
        assert_eq!(predicate_stem("숨어있어"), "숨어있");
        assert_eq!(predicate_stem("종료해"), "종료");
        assert_eq!(jamo("각"), "ㄱㅏㄱ");
        assert_eq!(jamo("a나"), "aㄴㅏ");
    }
}
