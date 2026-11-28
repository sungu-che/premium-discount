//! 분석 질의의 부분 부정: 질의에서 찾은 사건 · 페이지 · 행동 구간마다 포함인지 제외인지 가린다 (12개 언어, 모델 없음).
//!
//! "장바구니 페이지는 봤는데 결제 페이지는 안 간 기록", "cart page visits but not the checkout", "Warenkorbseite ohne Kasse" 처럼
//! 퍼널 질의는 한 문장에 포함할 단계와 뺄 단계를 함께 쓴다. 규칙:
//!
//! 1. **절 나누기**: 역접 접속사("but · aber · mais · pero · ma · mas · maar · ale · لكن · 하지만 · 但是 · のに" …)와 한국어 역접
//!    어미("-는데 · -지만")에서 끊는다. 앞 절은 양보절, 뒤 절이 주절이다. "though · obwohl · aunque" … 는 뒤 절을 양보절로 만든다.
//! 2. **부정 표지 하나가 같은 절의 구간 하나를 뒤집는다**: 앞으로 덮는 표지(not · nicht · pas · no · non · não · niet · ne · لم ·
//!    without · ohne · sans · sin · senza · sem · zonder · bez · دون · 没有 · 不 …)는 뒤의 가장 가까운 구간, 뒤로 덮는 표지(한국어
//!    "안 · 못 · 않 · 없이 · 제외 · 빼고", 일본어 "ない · なかった · せず · なし · 以外")는 앞의 가장 가까운 구간. 그쪽에 구간이
//!    없으면 반대쪽에서 찾는다.
//! 3. **홀수면 제외, 짝수면 포함** (이중 부정은 긍정): 한국어 · 영어 낱말 구간은 premium-discount-nms 의 [`read_span`] 부정 수와
//!    견줘 큰 쪽을 쓴다 ("결제하지 않은" 의 긴 부정).
//! 4. **주절 우선**: 같은 키가 포함과 제외로 함께 나오면 주절(양보절이 아닌 절)의 것이, 그래도 같으면 뒤의 것이 이긴다.
//!
//! 표는 질의 언어(`lang`)에서 고른다: 이탈리아어 "ma"(그러나), 체코어 "ale"(그러나), 스페인어 "sin"(없이) 같은 낱말이 다른 언어
//! 글에서 표지로 잘못 읽히지 않게 한다. 체코어 접두 부정("nezaplatily")처럼 낱말 안에 붙은 부정은 아직 읽지 않는다.
//!
//! [`read_span`]: premium_discount_nms::analyze::read_span

/// 한 언어의 표지 표
struct Table {
    /// 뒤의 구간을 뒤집는 낱말
    forward: &'static [&'static str],
    /// 앞의 구간을 뒤집는 낱말
    backward: &'static [&'static str],
    /// 앞 절을 양보절로 만드는 접속사 (낱말)
    adversative: &'static [&'static str],
    /// 뒤 절을 양보절로 만드는 접속사 (낱말)
    concessive_next: &'static [&'static str],
    /// 띄어 쓰지 않는 글(중국어 · 일본어)에서 글 안에서 찾는 표지: (글, 앞으로 덮나)
    glued: &'static [(&'static str, bool)],
    /// 띄어 쓰지 않는 글의 역접 접속사 (글 안에서 찾음)
    glued_adversative: &'static [&'static str],
    /// 낱말이 이 꼬리로 끝나면 그 뒤에서 끊고 앞 절을 양보절로 (한국어 역접 어미)
    adversative_endings: &'static [&'static str],
}

const EMPTY: &[&str] = &[];
const NO_GLUED: &[(&str, bool)] = &[];

const KO: Table = Table {
    forward: EMPTY,
    backward: &["안", "못", "않", "않은", "않고", "않았고", "않았던", "않았는데", "못한", "못했고", "아닌", "없이", "없는", "제외", "제외한", "제외하고", "빼고", "말고"],
    adversative: &["근데", "그런데", "하지만", "그렇지만", "그러나", "다만", "반면"],
    concessive_next: EMPTY,
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: &["는데", "은데", "지만", "는데도", "었는데", "았는데"],
};
const EN: Table = Table {
    forward: &[
        "not", "never", "no", "don't", "dont", "didn't", "didnt", "doesn't", "doesnt", "haven't", "hasn't", "hadn't", "wasn't", "weren't",
        "won't", "can't", "cannot", "without", "except", "excluding",
    ],
    backward: EMPTY,
    adversative: &["but", "however", "yet"],
    concessive_next: &["though", "although"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const JA: Table = Table {
    forward: EMPTY,
    backward: EMPTY,
    adversative: EMPTY,
    concessive_next: EMPTY,
    glued: &[("なかった", false), ("ません", false), ("せず", false), ("ずに", false), ("ない", false), ("なし", false), ("以外", false), ("除く", false), ("除き", false)],
    glued_adversative: &["のに", "けれど", "けど", "しかし", "でも"],
    adversative_endings: EMPTY,
};
const ZH: Table = Table {
    forward: EMPTY,
    backward: EMPTY,
    adversative: EMPTY,
    concessive_next: EMPTY,
    glued: &[("没有", true), ("沒有", true), ("除了", true), ("没", true), ("沒", true), ("不", true), ("未", true), ("无", true), ("無", true)],
    glued_adversative: &["但是", "可是", "然而", "但", "却"],
    adversative_endings: EMPTY,
};
const DE: Table = Table {
    forward: &["nicht", "nie", "niemals", "kein", "keine", "keinen", "keinem", "keiner", "ohne", "außer", "ausser"],
    backward: EMPTY,
    adversative: &["aber", "jedoch", "doch", "sondern"],
    concessive_next: &["obwohl", "obgleich"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const FR: Table = Table {
    // "ne … pas" 는 "pas" 하나로 센다
    forward: &["pas", "jamais", "aucun", "aucune", "sans", "sauf"],
    backward: EMPTY,
    adversative: &["mais", "cependant", "pourtant"],
    concessive_next: &["bien", "quoique"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const ES: Table = Table {
    forward: &["no", "nunca", "jamás", "sin", "excepto", "salvo"],
    backward: EMPTY,
    adversative: &["pero", "sino", "sin embargo"],
    concessive_next: &["aunque"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const IT: Table = Table {
    forward: &["non", "mai", "senza", "tranne", "eccetto"],
    backward: EMPTY,
    adversative: &["ma", "però", "tuttavia"],
    concessive_next: &["sebbene", "benché"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const PT: Table = Table {
    forward: &["não", "nao", "nunca", "sem", "exceto"],
    backward: EMPTY,
    adversative: &["mas", "porém", "contudo"],
    concessive_next: &["embora"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const NL: Table = Table {
    forward: &["niet", "nooit", "geen", "zonder", "behalve"],
    backward: EMPTY,
    adversative: &["maar", "echter"],
    concessive_next: &["hoewel", "ofschoon"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const CS: Table = Table {
    forward: &["ne", "nikdy", "bez", "kromě"],
    backward: EMPTY,
    adversative: &["ale", "však", "avšak"],
    concessive_next: &["ačkoli", "ačkoliv"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};
const AR: Table = Table {
    forward: &["لم", "لا", "لن", "ليس", "دون", "بدون", "بلا", "غير"],
    backward: EMPTY,
    adversative: &["لكن", "ولكن"],
    concessive_next: &["رغم"],
    glued: NO_GLUED,
    glued_adversative: EMPTY,
    adversative_endings: EMPTY,
};

fn table_of(lang: &str) -> Option<&'static Table> {
    let l = lang.trim().to_ascii_lowercase();
    let base = l.split(['-', '_']).next().unwrap_or("");
    Some(match base {
        "ko" => &KO,
        "en" => &EN,
        "ja" => &JA,
        "zh" => &ZH,
        "de" => &DE,
        "fr" => &FR,
        "es" => &ES,
        "it" => &IT,
        "pt" => &PT,
        "nl" => &NL,
        "cs" => &CS,
        "ar" => &AR,
        _ => return None,
    })
}

/// 질의 글의 낱말 하나 (글자 번호 `[start, end)`, 소문자)
struct Tok {
    start: usize,
    end: usize,
    text: String,
}

fn tokens(chars: &[char]) -> Vec<Tok> {
    let sep = |c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '"' | '“' | '”' | '「' | '」' | '、' | '。' | '，' | '،');
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if sep(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !sep(chars[i]) {
            i += 1;
        }
        out.push(Tok { start, end: i, text: chars[start..i].iter().collect() });
    }
    out
}

fn find_all(chars: &[char], needle: &str) -> Vec<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || n.len() > chars.len() {
        return Vec::new();
    }
    (0..=chars.len() - n.len()).filter(|&i| chars[i..i + n.len()] == n[..]).collect()
}

/// 구간 하나의 읽기
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanPolarity {
    /// 이 구간을 덮은 부정 표지 수
    pub negations: u8,
    /// 구간이 든 절이 양보절이 아니다 (화자의 결론)
    pub main: bool,
}

impl SpanPolarity {
    /// 홀수 번 부정이면 제외
    pub fn excluded(&self) -> bool {
        self.negations % 2 == 1
    }
}

/// `text` 에서 찾은 구간들(글자 번호 `[start, end)`, `text.to_lowercase()` 기준)의 부정 수와 절 위치. 모르는 언어는 모두 포함 ·
/// 주절로 본다
pub fn span_polarities(text: &str, lang: &str, spans: &[(usize, usize)]) -> Vec<SpanPolarity> {
    let neutral = vec![SpanPolarity { negations: 0, main: true }; spans.len()];
    let Some(t) = table_of(lang) else { return neutral };
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let toks = tokens(&chars);
    // 절 경계: (경계 글자 번호, 앞 절이 양보절, 뒤 절이 양보절)
    let mut cuts: Vec<(usize, bool, bool)> = Vec::new();
    for (k, tok) in toks.iter().enumerate() {
        let last = k + 1 == toks.len();
        if k > 0 && t.adversative.contains(&tok.text.as_str()) {
            cuts.push((tok.start, true, false));
        } else if k > 0 && !last && t.concessive_next.contains(&tok.text.as_str()) {
            cuts.push((tok.start, false, true));
        } else if !last && t.adversative_endings.iter().any(|e| tok.text.ends_with(e) && tok.text.chars().count() > e.chars().count()) {
            cuts.push((tok.end, true, false));
        }
    }
    for conj in t.glued_adversative {
        for at in find_all(&chars, conj) {
            if at > 0 {
                cuts.push((at, true, false));
            }
        }
    }
    cuts.sort_by_key(|c| c.0);
    cuts.dedup_by_key(|c| c.0);
    // 절: [start, end), 양보절인가
    let mut clauses: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = 0;
    let mut concessive_next = false;
    for &(at, prev_concessive, next_concessive) in &cuts {
        clauses.push((start, at, prev_concessive || concessive_next));
        start = at;
        concessive_next = next_concessive;
    }
    clauses.push((start, chars.len(), concessive_next));
    let clause_of = |pos: usize| clauses.iter().position(|c| pos >= c.0 && pos < c.1).unwrap_or(clauses.len() - 1);

    // 표지: (글자 번호, 앞으로 덮나)
    let mut markers: Vec<(usize, bool)> = Vec::new();
    for tok in &toks {
        if t.forward.contains(&tok.text.as_str()) {
            markers.push((tok.start, true));
        } else if t.backward.contains(&tok.text.as_str()) {
            markers.push((tok.start, false));
        }
    }
    for &(m, forward) in t.glued {
        for at in find_all(&chars, m) {
            // 더 긴 표지 안의 짧은 표지는 세지 않는다 ("没有" 안의 "没", "なかった" 안의 "な…")
            let inside = markers.iter().any(|&(p, _)| p <= at && at < p + 2);
            if !inside {
                markers.push((at, forward));
            }
        }
    }
    let mut out = neutral;
    for (i, &(s, _)) in spans.iter().enumerate() {
        out[i].main = !clauses[clause_of(s)].2;
    }
    for &(at, forward) in &markers {
        let cl = clause_of(at);
        let same: Vec<usize> = (0..spans.len()).filter(|&i| clause_of(spans[i].0) == cl && !(spans[i].0 <= at && at < spans[i].1)).collect();
        let after = same.iter().copied().filter(|&i| spans[i].0 >= at).min_by_key(|&i| spans[i].0);
        let before = same.iter().copied().filter(|&i| spans[i].1 <= at).max_by_key(|&i| spans[i].1);
        let target = if forward { after.or(before) } else { before.or(after) };
        if let Some(i) = target {
            out[i].negations = out[i].negations.saturating_add(1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `phrases` 가 `text` 에 나오는 글자 번호
    fn spans_of(text: &str, phrases: &[&str]) -> Vec<(usize, usize)> {
        let lower: Vec<char> = text.to_lowercase().chars().collect();
        let mut from = 0;
        phrases
            .iter()
            .map(|p| {
                let n: Vec<char> = p.to_lowercase().chars().collect();
                let at = (from..=lower.len() - n.len()).find(|&i| lower[i..i + n.len()] == n[..]).unwrap_or_else(|| panic!("{p} in {text}"));
                from = at + n.len();
                (at, at + n.len())
            })
            .collect()
    }

    fn excluded(text: &str, lang: &str, phrases: &[&str]) -> Vec<bool> {
        span_polarities(text, lang, &spans_of(text, phrases)).iter().map(SpanPolarity::excluded).collect()
    }

    #[test]
    fn twelve_languages_include_the_first_step_and_exclude_the_negated_one() {
        let cases: [(&str, &str, [&str; 2]); 12] = [
            ("ko", "장바구니 페이지는 봤는데 결제 페이지는 안 간 기록", ["장바구니 페이지", "결제 페이지"]),
            ("en", "sessions that opened the cart page but not the checkout", ["cart page", "checkout"]),
            ("ja", "カートページは見たのに決済ページには進まなかった記録", ["カートページ", "決済ページ"]),
            ("zh-hans", "看了购物车页面但是没有进入结算页面的记录", ["购物车页面", "结算页面"]),
            ("de", "Besuche mit Warenkorbseite, aber ohne Kasse", ["warenkorbseite", "kasse"]),
            ("fr", "visites de la page panier mais sans aller au paiement", ["page panier", "au paiement"]),
            ("es", "visitas a la página del carrito pero no al pago", ["página del carrito", "al pago"]),
            ("it", "visite alla pagina carrello ma non al pagamento", ["pagina carrello", "al pagamento"]),
            ("pt", "visitas à página do carrinho mas não ao pagamento", ["página do carrinho", "ao pagamento"]),
            ("nl", "bezoeken aan de winkelwagenpagina maar niet afrekenen", ["winkelwagenpagina", "afrekenen"]),
            ("cs", "návštěvy stránky košíku ale ne pokladny", ["stránky košíku", "pokladny"]),
            ("ar", "زيارات صفحة السلة لكن بدون صفحة الدفع", ["صفحة السلة", "صفحة الدفع"]),
        ];
        for (lang, text, phrases) in cases {
            assert_eq!(excluded(text, lang, &phrases), vec![false, true], "{lang}: {text}");
            let p = span_polarities(text, lang, &spans_of(text, &phrases));
            assert!(!p[0].main && p[1].main, "{lang}: the step before the adversative is the concessive clause");
        }
    }

    #[test]
    fn double_negation_restores_and_unknown_languages_stay_neutral() {
        // 두 표지가 같은 구간을 덮으면 긍정
        assert_eq!(excluded("not without the checkout page", "en", &["checkout page"]), vec![false]);
        assert_eq!(excluded("visits without checkout", "en", &["checkout"]), vec![true]);
        // 한국어 뒤로 덮는 표지: "결제 페이지 제외"
        assert_eq!(excluded("장바구니 페이지 방문 중 결제 페이지 제외", "ko", &["장바구니 페이지", "결제 페이지"]), vec![false, true]);
        // 다른 언어의 낱말은 표지가 아니다: 영어 글의 "sin" · "ma"
        assert_eq!(excluded("cart page sin checkout", "en", &["cart page", "checkout"]), vec![false, false]);
        assert_eq!(excluded("cart page ma checkout", "xx", &["cart page", "checkout"]), vec![false, false]);
        assert!(span_polarities("", "ko", &[]).is_empty());
    }
}
