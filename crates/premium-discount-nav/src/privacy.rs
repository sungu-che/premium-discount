//! 이름 · 위치 단서의 개인정보 거르기 (영역 지도를 서버에 올리기 전에)와 민감 화면 판정
//!
//! 사람 이름처럼 모양만으로 알 수 없는 개인정보는 거르지 못한다. 그래서 지도는 동의한 경우에만 올리고,
//! 서버는 영역 이름을 평가와 같은 검수(욕설 · 개인정보 검사)에 한 번 더 넣는다.
//!
//! 민감 화면은 여러 층의 신호를 합쳐 판정한다([`evaluate_privacy`]): 트리가 알려 준 개인정보 칸([`crate::sensitive_screen`]),
//! 속성 없이 그린 가상 보안 키패드 · 나눠진 OTP 칸(기하 규칙, [`virtual_keypad`] · [`split_pin_fields`]), 그리고 다른 crate 가
//! 감지해 값으로 넘기는 신호(화면 캡처를 막은 창, 결제 오버레이 창, 비전의 개인정보 판정, 시드 구문, 알림 토스트 영역).
//! 결과는 그대로 둠 · 토스트만 가림 · 화면 전체 잠금(사유와 함께) 중 하나다([`PrivacyShield`]).
//! 기하 규칙은 오탐을 막으려고 모양과 뜻을 함께 본다: 3×4 격자라도 숫자 · 기능 키 · 의도적으로 빈 라벨이 없으면
//! (게임 가방, 상품 카드) 키패드가 아니고, 위아래로도 같은 칸이 쌓인 입력칸 줄(낱말 맞히기 판)은 OTP 가 아니다.

use premium_discount_identity::{detect_seed_phrase_span, redact_seed_phrases};
use serde::{Deserialize, Serialize};

use crate::region::{sensitive_screen, Rect, Region, Role, Source, MERGE_IOU};

/// 지도에 넣는 이름의 최대 글자 수
pub const NAME_MAX_CHARS: usize = 40;

/// 숫자가 이만큼 이상 든 낱말은 개인정보로 본다 (전화 · 카드 · 계좌 · 주민등록번호 조각)
const PRIVATE_DIGITS: usize = 4;

/// 이 말 바로 앞뒤의 2 ~ 3자리 숫자는 보안코드로 본다
const SECURITY_CODE_WORDS: [&str; 5] = ["cvc", "cvv", "otp", "인증번호", "보안코드"];

/// 이름에서 개인정보로 보이는 낱말을 뺀다: 이메일, 숫자가 네 개 이상 든 낱말, 보안코드 말(CVC · CVV · OTP · 인증번호 ·
/// 보안코드) 바로 앞뒤 · 거기 붙은 2 ~ 3자리 숫자, 시드 구문(BIP-39 단어표 낱말 12개 이상, premium-discount-identity).
/// 숫자 사이 `/` 앞뒤의 공백은 먼저 붙인다: 띄어 쓴 카드 유효기간(`12 / 25`)도 숫자 네 개짜리 낱말이 되어 빠진다.
/// 남은 낱말을 공백 하나로 잇고 [`NAME_MAX_CHARS`] 글자에서 자른다. 연도 · 가격처럼 숫자가 긴 낱말도 함께 빠진다
/// (거르는 쪽으로 기운다).
pub fn scrub_name(name: &str) -> String {
    let mut text = join_digit_slashes(name);
    redact_seed_phrases(&mut text, " ");
    let words: Vec<&str> = text.split_whitespace().collect();
    let kept: Vec<&str> = words.iter().enumerate().filter(|(i, w)| !looks_private(w) && !near_security_code(&words, *i)).map(|(_, w)| *w).collect();
    let joined = kept.join(" ");
    joined.chars().take(NAME_MAX_CHARS).collect::<String>().trim_end().to_string()
}

fn looks_private(word: &str) -> bool {
    let email = word.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.'));
    email || word.chars().filter(|c| c.is_ascii_digit()).count() >= PRIVATE_DIGITS
}

/// 숫자 사이에 놓인 `/` 앞뒤의 공백을 지운다 (`12 / 25` → `12/25`. `10 / 개` 는 그대로)
fn join_digit_slashes(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let prev = |i: usize| (0..i).rev().find(|&j| !chars[j].is_whitespace());
    let next = |i: usize| (i + 1..chars.len()).find(|&j| !chars[j].is_whitespace());
    let digit = |j: Option<usize>| j.is_some_and(|j| chars[j].is_ascii_digit());
    let slash_between_digits = |s: usize| chars[s] == '/' && digit(prev(s)) && digit(next(s));
    chars
        .iter()
        .enumerate()
        .filter(|&(i, c)| !(c.is_whitespace() && (next(i).is_some_and(&slash_between_digits) || prev(i).is_some_and(&slash_between_digits))))
        .map(|(_, c)| *c)
        .collect()
}

fn security_word(word: &str) -> bool {
    let word = word.to_lowercase();
    SECURITY_CODE_WORDS.iter().any(|k| word.contains(k))
}

/// 보안코드 말 바로 앞뒤의 2 ~ 3자리 숫자(`CVC 123`), 또는 보안코드 말에 붙은 2 ~ 3자리 숫자(`OTP:482`)
fn near_security_code(words: &[&str], i: usize) -> bool {
    let word = words[i];
    let digits = word.chars().filter(char::is_ascii_digit).count();
    if security_word(word) {
        return (2..=3).contains(&digits);
    }
    let bare = word.trim_matches(|c: char| c.is_ascii_punctuation());
    let short_code = (2..=3).contains(&bare.len()) && bare.chars().all(|c| c.is_ascii_digit());
    short_code && (i.checked_sub(1).is_some_and(|j| security_word(words[j])) || words.get(i + 1).is_some_and(|w| security_word(w)))
}

/// 라벨에 시드 구문이 보이는가: 라벨 하나에 BIP-39 낱말 12개 이상, 또는 지갑 앱처럼 칸마다 번호를 붙인 낱말
/// (`1. abandon` … `12. about`)을 번호 순으로 이었을 때 12개 이상 (premium-discount-identity 의 단어표)
fn seed_phrase_in_labels(regions: &[Region]) -> bool {
    if regions.iter().any(|r| detect_seed_phrase_span(&r.name).is_some()) {
        return true;
    }
    let mut numbered: Vec<(u32, &str)> = regions.iter().filter_map(|r| numbered_word(&r.name).map(|n| (n, r.name.as_str()))).collect();
    numbered.sort_by_key(|(n, _)| *n);
    numbered.dedup_by_key(|(n, _)| *n);
    let joined = numbered.iter().map(|(_, name)| *name).collect::<Vec<_>>().join(" ");
    detect_seed_phrase_span(&joined).is_some()
}

/// `7. word` · `7) word` · `7 word` 꼴이면 그 번호 (1 ~ 2자리, 뒤는 낱말 하나)
fn numbered_word(name: &str) -> Option<u32> {
    let name = name.trim();
    let digits = name.find(|c: char| !c.is_ascii_digit())?;
    let rest = name[digits..].trim_start_matches(['.', ')']).trim();
    let one_word = !rest.is_empty() && !rest.contains(char::is_whitespace);
    ((1..=2).contains(&digits) && one_word).then(|| name[..digits].parse().ok()).flatten()
}

/// 위치 단서에서 네 자리 이상 이어진 숫자를 `#` 로 바꾼다 (`#user-123456` → `#user-#`).
/// 화면마다 바뀌는 id 를 지워 같은 영역이 같은 단서를 갖게 하는 몫도 있다.
pub fn scrub_anchor(anchor: &str) -> String {
    let mut out = String::with_capacity(anchor.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= PRIVATE_DIGITS {
            out.push('#');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in anchor.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// 화면 전체 잠금의 사유. 여럿이면 앞에 둔 것이 사유가 된다 ([`privacy_triggers`])
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyTrigger {
    /// 캡처하는 모니터에 화면 캡처를 막은 창이 보인다 (WDA_MONITOR · WDA_EXCLUDEFROMCAPTURE, premium-discount-window 가 감지)
    DisplayAffinityProtected,
    /// 결제 오버레이 · 자격 증명 · 금융 보안 모듈의 창, 또는 결제 · 인증 경로의 창이 보인다
    OverlayPaymentWindow,
    /// 트리가 알려 준 개인정보 칸 (`Region.sensitive`: 비밀번호, `autocomplete="cc-*"`)
    TreeSensitiveField,
    /// 속성 없는 가상 보안 키패드 · 나눠진 OTP 칸 (기하 규칙)
    VirtualSecurityKeypad,
    /// BIP-39 시드 구문 (premium-discount-identity 가 감지)
    SeedPhraseExposure,
    /// 비전이 개인정보 화면으로 보았다 (VisionKind::Privacy)
    VisionPrivacyDomain,
}

impl PrivacyTrigger {
    /// 화면의 픽셀 · 글로만 아는 사유인가. 창 · 트리 · 주소만 보는 확인(픽셀 캡처 없이)으로는 다시 볼 수 없어서, 잠금을
    /// 풀 때 잠근 표면(창 · 페이지)을 떠났는지를 본다 ([`crate::PrivacyStateMachine`])
    pub fn pixel_only(self) -> bool {
        matches!(self, PrivacyTrigger::SeedPhraseExposure | PrivacyTrigger::VisionPrivacyDomain)
    }
}

/// 민감 화면 판정
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyShield {
    /// 그대로 둔다
    Clear,
    /// 이 영역(창 기준 0 ~ 1, 알림 토스트)만 가리고 나머지 화면은 그대로 쓴다
    PartialRedaction { zones: Vec<Rect> },
    /// 화면 전체를 잠근다: [`crate::sensitive_screen`] 과 같이 기록 · 비전 · 지도 · 체크포인트 없이 트리 후보로만 번호를 매긴다
    FullLockdown(PrivacyTrigger),
}

impl PrivacyShield {
    pub fn trigger(&self) -> Option<PrivacyTrigger> {
        match self {
            PrivacyShield::FullLockdown(t) => Some(*t),
            _ => None,
        }
    }

    /// 기록해도 되는가 (`pd_nav_number` 의 `record`). 전체 잠금이면 false, 토스트만 가렸으면 가린 영역 밖은 기록한다
    pub fn record(&self) -> bool {
        !matches!(self, PrivacyShield::FullLockdown(_))
    }

    /// 후보 영역에 판정을 적용한다: 토스트 영역과 조금이라도 겹친 후보는 빼고, 전체 잠금이면 트리 후보만 남긴다
    pub fn redact(&self, regions: Vec<Region>) -> Vec<Region> {
        match self {
            PrivacyShield::Clear => regions,
            PrivacyShield::PartialRedaction { zones } => regions.into_iter().filter(|r| !zones.iter().any(|z| overlaps(z, &r.rect))).collect(),
            PrivacyShield::FullLockdown(_) => regions.into_iter().filter(|r| r.source == Source::Tree).collect(),
        }
    }
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// 민감 화면 판정에 넣는 신호. 창 · 비전 · 시드 구문 신호는 다른 crate 가 감지해 값으로 넘긴다
#[derive(Debug, Clone, Copy, Default)]
pub struct PrivacyProbeInput<'a> {
    /// 이 화면의 후보 영역 (합치기 전이어도 된다)
    pub regions: &'a [Region],
    /// 캡처하는 모니터에 화면 캡처를 막은 창(WDA_MONITOR · WDA_EXCLUDEFROMCAPTURE)이 가려지지 않고 보인다
    pub os_capture_protected: bool,
    /// 알려진 결제 오버레이 · 자격 증명 · 금융 보안 모듈 프로세스의 창, 또는 결제 · 인증 경로의 창이 보인다
    pub os_overlay_payment_detected: bool,
    /// 알림 토스트가 덮은 영역 (창 기준 0 ~ 1). 이것만 있으면 그 영역만 가린다
    pub os_notification_zones: &'a [Rect],
    /// 비전이 VisionKind::Privacy 임계값을 넘었다
    pub vision_privacy_hit: bool,
    /// 창 제목 · 주소 같은 글에서 시드 구문이 잡혔다 (`premium_discount_identity::detect_seed_phrase_span`).
    /// 후보 영역의 라벨은 이 판정이 직접 본다
    pub seed_phrase_detected: bool,
    /// 인증 · 결제 문맥 (라벨이 빈 키패드를 키패드로 보는 근거)
    pub auth_or_payment_context: bool,
}

/// 화면 전체 잠금 사유를 모두, 앞선 순서대로 (창 → 트리 칸 → 기하 규칙 → 시드 구문 → 비전)
pub fn privacy_triggers(input: &PrivacyProbeInput<'_>) -> Vec<PrivacyTrigger> {
    let regions = input.regions;
    [
        (input.os_capture_protected, PrivacyTrigger::DisplayAffinityProtected),
        (input.os_overlay_payment_detected, PrivacyTrigger::OverlayPaymentWindow),
        (sensitive_screen(regions), PrivacyTrigger::TreeSensitiveField),
        (virtual_keypad(regions, input.auth_or_payment_context) || split_pin_fields(regions), PrivacyTrigger::VirtualSecurityKeypad),
        (input.seed_phrase_detected || seed_phrase_in_labels(regions), PrivacyTrigger::SeedPhraseExposure),
        (input.vision_privacy_hit, PrivacyTrigger::VisionPrivacyDomain),
    ]
    .into_iter()
    .filter_map(|(hit, trigger)| hit.then_some(trigger))
    .collect()
}

/// 화면 일부만 가리는 영역의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    /// OS 알림 토스트
    OsNotificationToast,
    /// 터미널 · 편집기 안의 비밀 한 줄 (키 · 토큰 · 환경 변수 값)
    InlineSecretLine,
    /// 문서 · 표 안의 민감한 칸 하나 (고유 식별 정보 · 급여 · 진단 …)
    SensitiveDocumentCell,
}

/// 화면 일부만 가리는 영역 (창 기준 0 ~ 1). 무엇이 비밀인지는 부르는 쪽이 판정해 넘긴다
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InlineRedactionZone {
    pub rect: Rect,
    pub kind: RedactionKind,
}

/// [`evaluate_privacy`] 에 인라인 가림 영역을 더한 판정: 인라인 영역과 겹친 후보는 트리 · 키패드 · 시드 구문 판정에서 빼고(비밀
/// 한 줄 · 민감한 칸 하나로 창 전체를 잠그지 않는다), 그 영역을 토스트와 함께 가린다. 창 · 시드 구문(제목) · 비전 신호의 잠금은
/// 그대로다. 영역이 없으면 [`evaluate_privacy`] 와 같다
pub fn evaluate_privacy_inline(input: &PrivacyProbeInput<'_>, inline: &[InlineRedactionZone]) -> PrivacyShield {
    if inline.is_empty() {
        return evaluate_privacy(input);
    }
    let outside: Vec<Region> = input.regions.iter().filter(|r| !inline.iter().any(|z| overlaps(&z.rect, &r.rect))).cloned().collect();
    let narrowed = PrivacyProbeInput { regions: &outside, ..*input };
    if let Some(trigger) = privacy_triggers(&narrowed).first() {
        return PrivacyShield::FullLockdown(*trigger);
    }
    let mut zones: Vec<Rect> = input.os_notification_zones.to_vec();
    zones.extend(inline.iter().map(|z| z.rect));
    PrivacyShield::PartialRedaction { zones }
}

/// 여러 층의 신호를 합쳐 판정한다: 잠금 사유가 하나라도 있으면 전체 잠금(앞선 사유), 알림 토스트만 있으면 그 영역만 가림,
/// 아니면 그대로 둠. [`crate::sensitive_screen`] 과 [`crate::merge`] 는 이 판정과 따로 지금처럼 동작한다
pub fn evaluate_privacy(input: &PrivacyProbeInput<'_>) -> PrivacyShield {
    if let Some(trigger) = privacy_triggers(input).first() {
        return PrivacyShield::FullLockdown(*trigger);
    }
    if !input.os_notification_zones.is_empty() {
        return PrivacyShield::PartialRedaction {
            zones: input.os_notification_zones.to_vec(),
        };
    }
    PrivacyShield::Clear
}

/// 가상 키패드의 칸 수 (3×4 · 4×3 · 2×6 격자, 몇 칸은 비어도 된다)
const KEYPAD_CELLS: std::ops::RangeInclusive<usize> = 10..=14;
/// 같은 키패드 · OTP 줄의 칸은 가로 · 세로 크기가 이 비율 안에서 같다
const CELL_SIZE_TOLERANCE: f32 = 0.15;
/// 키패드 라벨에 서로 다른 한 자리 숫자와 가림 글자가 이만큼 이상 있다
const KEYPAD_MARKS: usize = 6;
/// 키패드 기능 키 (라벨 전체가 같아야 한다)
const KEYPAD_FUNCTION_KEYS: [&str; 7] = ["전체삭제", "재배열", "초기화", "clear", "shuffle", "backspace", "⌫"];
/// 눌린 숫자를 가린 라벨
const MASK_LABELS: [&str; 5] = ["*", "•", "●", "∗", "＊"];
/// "숫자 7", "key 7" 처럼 한 자리 숫자 앞에 붙는 말
const DIGIT_PREFIXES: [&str; 6] = ["숫자", "번호", "key", "digit", "number", "num"];
/// 나눠진 OTP · PIN 칸 수
const OTP_FIELDS: std::ops::RangeInclusive<usize> = 4..=8;
/// OTP · PIN 칸 이름에 흔한 말
const OTP_WORDS: [&str; 9] = ["otp", "pin", "code", "digit", "verification", "인증", "코드", "자리", "번호"];

/// 속성(`sensitive`) 없이 그린 가상 보안 키패드가 있는가. 두 관문을 모두 지나야 한다.
///
/// 1. 모양: 같은 부모 위치 단서(마지막 `>` · `/` 앞)를 가졌거나 서로 붙어 있는, 크기 차이가 15% 안인 칸 10 ~ 14개가
///    3×4 · 4×3 · 2×6 · 6×2 격자를 이룬다. 칸 좌표가 창 기준 0 ~ 1 이라 창의 가로세로 비를 모르므로, 격자 전체의
///    종횡비 대신 칸 하나의 종횡비(0.2 ~ 5)만 본다.
/// 2. 뜻 (하나 이상): 라벨에 서로 다른 한 자리 숫자 · 가림 글자가 6개 이상, 기능 키(전체삭제 · 재배열 · 초기화 · clear ·
///    shuffle · backspace)가 있다, 또는 트리가 준 라벨이 일부러 빈 칸이 10개 이상이고 인증 · 결제 문맥이거나 격자 바로
///    위아래에 입력창이 있다. 비전 후보는 라벨이 늘 비어 있어 이 근거로 세지 않는다.
pub fn virtual_keypad(regions: &[Region], auth_or_payment_context: bool) -> bool {
    let cells = distinct(regions.iter().filter(|r| r.source != Source::Event && !matches!(r.role, Role::Input | Role::Menu)));
    let inputs: Vec<&Rect> = regions.iter().filter(|r| r.role == Role::Input).map(|r| &r.rect).collect();
    keypad_groups(&cells).iter().any(|group| {
        let Some(grid) = keypad_grid(group) else {
            return false;
        };
        let mut digits: Vec<char> = grid.iter().filter_map(|r| single_digit(&r.name)).collect();
        digits.sort_unstable();
        digits.dedup();
        let masks = grid.iter().filter(|r| MASK_LABELS.contains(&r.name.trim())).count();
        let function_key = group.iter().any(|r| KEYPAD_FUNCTION_KEYS.contains(&r.name.trim().to_lowercase().as_str()));
        let blank = grid.iter().filter(|r| r.source == Source::Tree && r.name.trim().is_empty()).count();
        digits.len() + masks >= KEYPAD_MARKS || function_key || (blank >= *KEYPAD_CELLS.start() && (auth_or_payment_context || input_beside(&grid, &inputs)))
    })
}

/// 한 줄로 이어진 좁은 입력칸 4 ~ 8개(나눠진 OTP · PIN)가 있는가. 칸은 크기가 15% 안에서 같고, 가로 간격이 칸 너비
/// 이하다. 같은 크기의 입력칸이 위아래 줄에도 쌓여 있으면(낱말 맞히기 판 · 표) 아니고, 칸 이름이 서로 다른 설명(연 · 월 ·
/// 일 · 시)이면 보통 입력 form 이라 아니다
pub fn split_pin_fields(regions: &[Region]) -> bool {
    let fields = distinct(regions.iter().filter(|r| r.role == Role::Input && r.source != Source::Event && r.rect.w < 2.5 * r.rect.h));
    fields.iter().any(|seed| {
        let mut row: Vec<&Region> = fields
            .iter()
            .copied()
            .filter(|f| similar(f.rect.w, seed.rect.w) && similar(f.rect.h, seed.rect.h) && (f.rect.center().1 - seed.rect.center().1).abs() < 0.5 * seed.rect.h)
            .collect();
        row.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
        runs(&row).iter().any(|run| OTP_FIELDS.contains(&run.len()) && !stacked(run, &fields) && pin_names(run))
    })
}

/// 겹친 후보(같은 칸을 트리와 비전이 함께 낸 것)는 하나만 센다. 트리 후보가 남는다
fn distinct<'a>(candidates: impl Iterator<Item = &'a Region>) -> Vec<&'a Region> {
    let mut all: Vec<&Region> = candidates.collect();
    all.sort_by_key(|r| r.source != Source::Tree);
    let mut kept: Vec<&Region> = Vec::new();
    for r in all {
        if kept.iter().all(|k| k.rect.iou(&r.rect) < MERGE_IOU) {
            kept.push(r);
        }
    }
    kept
}

fn similar(a: f32, b: f32) -> bool {
    (a - b).abs() < CELL_SIZE_TOLERANCE * a.max(b)
}

/// 위치 단서의 부모: 마지막 `>` · `/` 앞 (없으면 None. 비전의 격자 칸 `cell:3,1` 은 부모가 없다)
fn anchor_parent(anchor: &str) -> Option<&str> {
    anchor.rfind(['>', '/']).filter(|i| *i > 0).map(|i| anchor[..i].trim())
}

/// 키패드 후보 묶음: 같은 부모 위치 단서를 가진 칸들, 그리고 크기가 비슷하고 서로 붙어 있는(중심 사이가 칸 크기의
/// 1.6배 안, 대각선 포함) 칸들의 연결 덩어리
fn keypad_groups<'a>(cells: &[&'a Region]) -> Vec<Vec<&'a Region>> {
    let mut by_parent: Vec<(&str, Vec<&Region>)> = Vec::new();
    for c in cells {
        if let Some(parent) = anchor_parent(&c.anchor) {
            match by_parent.iter_mut().find(|(p, _)| *p == parent) {
                Some((_, members)) => members.push(c),
                None => by_parent.push((parent, vec![c])),
            }
        }
    }
    let mut out: Vec<Vec<&Region>> = by_parent.into_iter().map(|(_, members)| members).collect();
    let mut root: Vec<usize> = (0..cells.len()).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for i in 0..cells.len() {
        for j in i + 1..cells.len() {
            let (a, b) = (&cells[i].rect, &cells[j].rect);
            let ((ax, ay), (bx, by)) = (a.center(), b.center());
            let touching = (ax - bx).abs() <= 1.6 * a.w.max(b.w) && (ay - by).abs() <= 1.6 * a.h.max(b.h);
            if similar(a.w, b.w) && similar(a.h, b.h) && touching {
                let (ri, rj) = (find(&mut root, i), find(&mut root, j));
                root[ri] = rj;
            }
        }
    }
    let mut components: Vec<(usize, Vec<&Region>)> = Vec::new();
    for (i, c) in cells.iter().enumerate() {
        let r = find(&mut root, i);
        match components.iter_mut().find(|(k, _)| *k == r) {
            Some((_, members)) => members.push(c),
            None => components.push((r, vec![c])),
        }
    }
    out.extend(components.into_iter().map(|(_, members)| members));
    out.retain(|g| g.len() >= *KEYPAD_CELLS.start());
    out
}

/// 묶음에서 크기가 중앙값의 15% 안인 칸들이 키패드 모양의 격자를 이루면 그 칸들
fn keypad_grid<'a>(group: &[&'a Region]) -> Option<Vec<&'a Region>> {
    if group.len() < *KEYPAD_CELLS.start() {
        return None;
    }
    let median = |values: Vec<f32>| {
        let mut v = values;
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let mw = median(group.iter().map(|r| r.rect.w).collect());
    let mh = median(group.iter().map(|r| r.rect.h).collect());
    let grid: Vec<&Region> = group
        .iter()
        .copied()
        .filter(|r| (r.rect.w - mw).abs() < CELL_SIZE_TOLERANCE * mw && (r.rect.h - mh).abs() < CELL_SIZE_TOLERANCE * mh)
        .collect();
    if !KEYPAD_CELLS.contains(&grid.len()) || grid.iter().any(|r| !(0.2..=5.0).contains(&(r.rect.w / r.rect.h))) {
        return None;
    }
    let rows = lines(grid.iter().map(|r| r.rect.center().1).collect(), 0.5 * mh);
    let cols = lines(grid.iter().map(|r| r.rect.center().0).collect(), 0.5 * mw);
    let (x0, y0, x1, y1) = bounds(&grid);
    let shaped = matches!((rows, cols), (4, 3) | (3, 4) | (2, 6) | (6, 2)) && grid.len() <= rows * cols;
    let compact = x1 - x0 <= cols as f32 * mw * 1.8 && y1 - y0 <= rows as f32 * mh * 1.8;
    (shaped && compact).then_some(grid)
}

/// 정렬한 좌표를 `tolerance` 안의 것끼리 묶은 줄 수
fn lines(mut values: Vec<f32>, tolerance: f32) -> usize {
    values.sort_by(f32::total_cmp);
    let mut count = 0;
    let mut start = f32::NEG_INFINITY;
    for v in values {
        if v - start > tolerance {
            count += 1;
            start = v;
        }
    }
    count
}

fn bounds(cells: &[&Region]) -> (f32, f32, f32, f32) {
    cells.iter().fold((f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY), |(x0, y0, x1, y1), r| {
        (x0.min(r.rect.x), y0.min(r.rect.y), x1.max(r.rect.x + r.rect.w), y1.max(r.rect.y + r.rect.h))
    })
}

/// 라벨이 한 자리 숫자인가: `7`, 또는 `숫자 7` · `key 7` 처럼 정해진 말 + 숫자
fn single_digit(name: &str) -> Option<char> {
    let name = name.trim();
    let last = name.chars().last().filter(char::is_ascii_digit)?;
    let prefix = name[..name.len() - 1].trim().to_lowercase();
    (prefix.is_empty() || DIGIT_PREFIXES.contains(&prefix.as_str())).then_some(last)
}

/// 격자 바로 위아래(격자 높이만큼)에 가로로 겹치는 입력창이 있는가 (PIN 칸 바로 아래의 키패드)
fn input_beside(grid: &[&Region], inputs: &[&Rect]) -> bool {
    let (x0, y0, x1, y1) = bounds(grid);
    let h = y1 - y0;
    inputs.iter().any(|i| i.x < x1 && i.x + i.w > x0 && i.y + i.h >= y0 - h && i.y <= y1 + h)
}

/// 왼쪽부터 놓인 같은 줄의 칸을, 앞 칸과의 간격이 칸 너비 이하인 것끼리 잇는다
fn runs<'a>(row: &[&'a Region]) -> Vec<Vec<&'a Region>> {
    let mut out: Vec<Vec<&Region>> = Vec::new();
    for f in row {
        let continues = out.last().and_then(|run| run.last()).is_some_and(|last| {
            let gap = f.rect.x - (last.rect.x + last.rect.w);
            f.rect.x > last.rect.x && gap <= last.rect.w
        });
        match out.last_mut() {
            Some(run) if continues => run.push(f),
            _ => out.push(vec![f]),
        }
    }
    out
}

/// 같은 크기의 입력칸이 이 줄의 위아래(칸 높이의 3배 안)에도 둘 이상 있다: 표나 낱말 판이다
fn stacked(run: &[&Region], fields: &[&Region]) -> bool {
    let (first, last) = (&run[0].rect, &run[run.len() - 1].rect);
    let (x0, x1, cy) = (first.x, last.x + last.w, first.center().1);
    fields
        .iter()
        .filter(|f| !run.iter().any(|r| std::ptr::eq(*r, **f)))
        .filter(|f| similar(f.rect.w, first.w) && similar(f.rect.h, first.h))
        .filter(|f| {
            let (fx, fy) = f.rect.center();
            let dy = (fy - cy).abs();
            fx > x0 && fx < x1 && dy >= 0.5 * first.h && dy <= 3.0 * first.h
        })
        .count()
        >= 2
}

/// 나눠진 OTP · PIN 칸의 이름: 모두 같거나, 칸마다 비었거나 숫자 · OTP 말이 들어 있다
fn pin_names(run: &[&Region]) -> bool {
    let names: Vec<String> = run.iter().map(|r| r.name.trim().to_lowercase()).collect();
    names.windows(2).all(|w| w[0] == w[1]) || names.iter().all(|n| n.is_empty() || n.chars().any(|c| c.is_ascii_digit()) || OTP_WORDS.iter().any(|w| n.contains(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_words_are_dropped() {
        assert_eq!(scrub_name("홍길동 hong@example.com 님에게 보내기"), "홍길동 님에게 보내기");
        assert_eq!(scrub_name("010-1234-5678 로 전화"), "로 전화");
        assert_eq!(scrub_name("카드 1234 5678 9012 3456"), "카드");
        assert_eq!(scrub_name("  검색   입력창 "), "검색 입력창");
        assert_eq!(scrub_name("2쪽 다음"), "2쪽 다음");
        assert_eq!(scrub_name("@멘션"), "@멘션");
        let long = "가".repeat(60);
        assert_eq!(scrub_name(&long).chars().count(), NAME_MAX_CHARS);
    }

    #[test]
    fn long_digit_runs_in_anchors_become_hashes() {
        assert_eq!(scrub_anchor("main>form#user-123456>input:nth(2)"), "main>form#user-#>input:nth(2)");
        assert_eq!(scrub_anchor("TextBox_20261116_99"), "TextBox_#_99");
        assert_eq!(scrub_anchor("cell:12,7"), "cell:12,7");
    }

    /// BIP-39 공식 12낱말 시드 구문 벡터 (체크섬 유효)
    const SEED: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";

    #[test]
    fn card_dates_security_codes_and_seed_phrases_leave_names() {
        assert_eq!(scrub_name("유효기간 12 / 25 입력"), "유효기간 입력", "띄어 쓴 유효기간도 숫자 네 개짜리 낱말이다");
        assert_eq!(scrub_name("10 / 개"), "10 / 개", "숫자 사이에 놓인 / 만 붙인다");
        assert_eq!(scrub_name("CVC 123 확인"), "CVC 확인");
        assert_eq!(scrub_name("인증번호 48 받기"), "인증번호 받기");
        assert_eq!(scrub_name("OTP:482 입력"), "입력");
        assert_eq!(scrub_name("482 (CVV)"), "(CVV)");
        assert_eq!(scrub_name("3단계 인증번호 받기"), "3단계 인증번호 받기", "보안코드 옆이 아닌 숫자 낱말은 남는다");
        assert_eq!(scrub_name("수량 12 담기"), "수량 12 담기");
        assert_eq!(scrub_name(&format!("복구 구문 {SEED} 보관")), "복구 구문 보관");
        let plain = "the brown fox jumps over the lazy dog";
        assert_eq!(scrub_name(plain), plain, "평범한 영어 문장은 시드 구문이 아니다");
    }

    use crate::region::merge;
    use crate::testing::region;

    /// 3열 × 4행 격자 (칸 0.08, 간격 0.01). 위치 단서는 `<parent>>key:<i>`
    fn grid(parent: &str, labels: &[&str], role: Role, source: Source) -> Vec<Region> {
        labels
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let (row, col) = ((i / 3) as f32, (i % 3) as f32);
                region(&format!("{parent}>key:{i}"), role, name, (0.40 + col * 0.09, 0.30 + row * 0.09, 0.08, 0.08), source)
            })
            .collect()
    }

    /// 한 줄로 이어진 입력칸 (너비 0.045, 높이 0.07, 간격 0.015)
    fn row_of_inputs(names: &[&str], y: f32) -> Vec<Region> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| region(&format!("form>input:nth({i})"), Role::Input, name, (0.30 + i as f32 * 0.06, y, 0.045, 0.07), Source::Tree))
            .collect()
    }

    fn regions_only(regions: &[Region]) -> PrivacyProbeInput<'_> {
        PrivacyProbeInput {
            regions,
            ..Default::default()
        }
    }

    const BAG: [&str; 12] = ["포션", "검", "방패", "활", "화살", "갑옷", "투구", "반지", "목걸이", "망토", "장화", "열쇠"];

    #[test]
    fn an_inline_secret_line_is_redacted_without_locking_the_window() {
        // 터미널: 명령 줄 세 개와 비밀 값 줄 하나 (트리가 그 줄을 민감 칸으로 표시)
        let mut lines: Vec<Region> = (0..3).map(|i| region(&format!("term>line:{i}"), Role::Text, "", (0.02, 0.10 + i as f32 * 0.05, 0.9, 0.04), Source::Tree)).collect();
        let mut secret = region("term>line:3", Role::Text, "", (0.02, 0.25, 0.9, 0.04), Source::Tree);
        secret.sensitive = true;
        lines.push(secret);
        let plain = PrivacyProbeInput { regions: &lines, ..Default::default() };
        assert_eq!(evaluate_privacy(&plain), PrivacyShield::FullLockdown(PrivacyTrigger::TreeSensitiveField), "before: the whole window");
        let zone = [InlineRedactionZone { rect: Rect::new(0.0, 0.245, 1.0, 0.05).unwrap(), kind: RedactionKind::InlineSecretLine }];
        let shield = evaluate_privacy_inline(&plain, &zone);
        assert!(shield.record(), "the rest of the terminal keeps recording");
        assert_eq!(shield, PrivacyShield::PartialRedaction { zones: vec![zone[0].rect] });
        let kept = shield.redact(lines.clone());
        assert_eq!(kept.len(), 3, "only the secret line is dropped");
        assert!(kept.iter().all(|r| !r.sensitive));
        // 창 단위 신호는 여전히 전체를 잠그고, 영역이 없으면 예전 판정과 같다
        let protected = PrivacyProbeInput { os_capture_protected: true, ..plain };
        assert_eq!(evaluate_privacy_inline(&protected, &zone).trigger(), Some(PrivacyTrigger::DisplayAffinityProtected));
        assert_eq!(evaluate_privacy_inline(&plain, &[]), evaluate_privacy(&plain));
        assert_eq!(serde_json::to_value(RedactionKind::SensitiveDocumentCell).unwrap(), "sensitive_document_cell");
    }

    #[test]
    fn ps01_a_tree_marked_password_field_locks_the_whole_screen() {
        let mut password = region("form>input#pw", Role::Input, "비밀번호", (0.3, 0.4, 0.4, 0.05), Source::Tree);
        password.sensitive = true;
        let screen = vec![
            region("form>input#id", Role::Input, "아이디", (0.3, 0.3, 0.4, 0.05), Source::Tree),
            password,
            region("form>button", Role::Button, "로그인", (0.3, 0.5, 0.2, 0.05), Source::Tree),
            region("nav>a", Role::Link, "회원가입", (0.0, 0.0, 0.1, 0.05), Source::Tree),
            region("cell:8,8", Role::Image, "", (0.7, 0.7, 0.2, 0.2), Source::Vision),
            region("roi:0", Role::Other, "", (0.8, 0.1, 0.1, 0.1), Source::Event),
        ];
        let shield = evaluate_privacy(&regions_only(&screen));
        assert_eq!(shield, PrivacyShield::FullLockdown(PrivacyTrigger::TreeSensitiveField));
        assert!(sensitive_screen(&screen), "sensitive == true (지금의 판정 그대로)");
        assert!(!shield.record(), "record == false");
        let kept = shield.redact(screen.clone());
        assert_eq!(kept.len(), 4);
        assert!(kept.iter().all(|r| r.source == Source::Tree), "비전 · 이벤트 후보 0건");
        assert_eq!(merge(screen, MERGE_IOU), kept, "merge 의 개인정보 처리도 그대로");
    }

    #[test]
    fn ps02_unmarked_keypads_and_otp_rows_lock_but_inventories_and_plain_text_do_not() {
        let keypad_lock = PrivacyShield::FullLockdown(PrivacyTrigger::VirtualSecurityKeypad);
        // A: 속성(sensitive) 없는 3×4 숫자 키패드 (섞인 배열 + 기능 키)
        let keypad = grid("main>div.keypad", &["7", "2", "9", "0", "4", "1", "8", "5", "3", "6", "재배열", "전체삭제"], Role::Button, Source::Tree);
        assert!(!sensitive_screen(&keypad));
        assert_eq!(evaluate_privacy(&regions_only(&keypad)), keypad_lock);
        // A: 좁은 입력칸 6개가 한 줄 (OTP)
        let otp = row_of_inputs(&[""; 6], 0.5);
        assert!(!sensitive_screen(&otp));
        assert_eq!(evaluate_privacy(&regions_only(&otp)), keypad_lock);

        // B: 3×4 RPG 가방 · 평범한 15낱말 영어 문장은 그대로 둔다
        let bag = grid("main>div.bag", &BAG, Role::Button, Source::Tree);
        assert_eq!(evaluate_privacy(&regions_only(&bag)), PrivacyShield::Clear);
        let sentence = "the quick brown fox jumps over the lazy dog and then runs into the forest";
        assert_eq!(sentence.split(' ').count(), 15);
        let words: Vec<Region> = sentence
            .split(' ')
            .enumerate()
            .map(|(i, w)| {
                let (x, y) = (0.05 + (i % 8) as f32 * 0.11, 0.2 + (i / 8) as f32 * 0.05);
                region(&format!("p>span:{i}"), Role::Text, w, (x, y, 0.03 + w.len() as f32 * 0.01, 0.04), Source::Tree)
            })
            .collect();
        assert_eq!(evaluate_privacy(&regions_only(&words)), PrivacyShield::Clear);
        // 낱말 버튼으로 된 3×4 격자도 키패드가 아니다
        let menu = grid("main>div.words", &["Sword", "Shield", "Potion", "Bow", "Arrow", "Armor", "Helm", "Ring", "Amulet", "Cape", "Boots", "Key"], Role::Button, Source::Tree);
        assert_eq!(evaluate_privacy(&regions_only(&menu)), PrivacyShield::Clear);

        // C: BIP-39 12낱말 시드 구문 (라벨은 identity 의 단어표로 직접 보고, 창 제목 · 주소에서 잡힌 것은 값으로 받는다)
        let seed_lock = PrivacyShield::FullLockdown(PrivacyTrigger::SeedPhraseExposure);
        let phrase = [region("main>p.phrase", Role::Text, SEED, (0.1, 0.4, 0.8, 0.1), Source::Tree)];
        assert_eq!(evaluate_privacy(&regions_only(&phrase)), seed_lock);
        let cells = grid("main>ol.words", &SEED.split(' ').collect::<Vec<_>>(), Role::Text, Source::Tree);
        let numbered: Vec<Region> = cells
            .into_iter()
            .enumerate()
            .map(|(i, mut r)| {
                r.name = format!("{}. {}", i + 1, r.name);
                r
            })
            .collect();
        assert_eq!(evaluate_privacy(&regions_only(&numbered)), seed_lock, "지갑 앱의 번호 붙은 낱말 칸");
        let from_title = PrivacyProbeInput {
            regions: &words,
            seed_phrase_detected: true,
            ..Default::default()
        };
        assert_eq!(evaluate_privacy(&from_title), seed_lock);
        // 단어표 낱말이라도 번호 없는 메뉴 버튼 12개는 시드 구문이 아니다
        let nav = grid("nav>ul", &["home", "news", "music", "video", "game", "sport", "travel", "forest", "brown", "lazy", "dog", "over"], Role::Link, Source::Tree);
        assert_eq!(evaluate_privacy(&regions_only(&nav)), PrivacyShield::Clear);
    }

    #[test]
    fn ps03_window_level_signals_lock_regardless_of_the_game_tree() {
        let game = grid("game>hud.bag", &BAG, Role::Button, Source::Tree);
        assert_eq!(evaluate_privacy(&regions_only(&game)), PrivacyShield::Clear);
        // 게임 위에 결제 오버레이 (GameOverlayUI.exe 등)
        let overlay = PrivacyProbeInput {
            regions: &game,
            os_overlay_payment_detected: true,
            ..Default::default()
        };
        assert_eq!(evaluate_privacy(&overlay), PrivacyShield::FullLockdown(PrivacyTrigger::OverlayPaymentWindow));
        // 모니터 옆쪽의 캡처 차단(WDA_EXCLUDEFROMCAPTURE) 은행 창
        let protected = PrivacyProbeInput {
            regions: &game,
            os_capture_protected: true,
            ..Default::default()
        };
        assert_eq!(evaluate_privacy(&protected), PrivacyShield::FullLockdown(PrivacyTrigger::DisplayAffinityProtected));
        let both = PrivacyProbeInput {
            os_overlay_payment_detected: true,
            vision_privacy_hit: true,
            ..protected
        };
        assert_eq!(
            privacy_triggers(&both),
            [PrivacyTrigger::DisplayAffinityProtected, PrivacyTrigger::OverlayPaymentWindow, PrivacyTrigger::VisionPrivacyDomain]
        );
        assert_eq!(evaluate_privacy(&both), PrivacyShield::FullLockdown(PrivacyTrigger::DisplayAffinityProtected));
        let vision = PrivacyProbeInput {
            regions: &game,
            vision_privacy_hit: true,
            ..Default::default()
        };
        let shield = evaluate_privacy(&vision);
        assert_eq!(shield.trigger(), Some(PrivacyTrigger::VisionPrivacyDomain));
        assert!(shield.trigger().unwrap().pixel_only() && !PrivacyTrigger::OverlayPaymentWindow.pixel_only());
        assert_eq!(shield.redact(game.clone()).len(), game.len(), "전체 잠금이어도 트리 후보로는 번호를 매긴다");
    }

    #[test]
    fn a_notification_toast_is_redacted_alone() {
        let mut screen = grid("game>hud.bag", &BAG, Role::Button, Source::Tree);
        screen.push(region("toast>text", Role::Text, "새 메시지", (0.76, 0.86, 0.2, 0.08), Source::Tree));
        screen.push(region("cell:9,9", Role::Image, "", (0.9, 0.9, 0.08, 0.08), Source::Vision));
        let toast = [Rect::new(0.75, 0.85, 0.24, 0.12).unwrap()];
        let input = PrivacyProbeInput {
            regions: &screen,
            os_notification_zones: &toast,
            ..Default::default()
        };
        let shield = evaluate_privacy(&input);
        assert_eq!(shield, PrivacyShield::PartialRedaction { zones: toast.to_vec() });
        assert!(shield.record() && shield.trigger().is_none());
        let kept = shield.redact(screen.clone());
        assert_eq!(kept.len(), screen.len() - 2, "토스트와 겹친 후보만 뺀다");
        assert!(kept.iter().all(|r| !r.anchor.starts_with("toast") && r.anchor != "cell:9,9"));
        // 잠금 사유가 있으면 토스트보다 전체 잠금이 앞선다
        let locked = PrivacyProbeInput {
            vision_privacy_hit: true,
            ..input
        };
        assert_eq!(evaluate_privacy(&locked), PrivacyShield::FullLockdown(PrivacyTrigger::VisionPrivacyDomain));
        assert_eq!(serde_json::to_value(&shield).unwrap()["partial_redaction"]["zones"].as_array().unwrap().len(), 1);
        assert_eq!(serde_json::to_value(PrivacyShield::FullLockdown(PrivacyTrigger::TreeSensitiveField)).unwrap(), serde_json::json!({"full_lockdown": "tree_sensitive_field"}));
    }

    #[test]
    fn blank_keypads_need_an_auth_context_or_a_pin_field_beside_them() {
        // 숫자를 그림으로 그려 트리 라벨이 빈 키패드
        let blank = grid("main>div.securekeys", &[""; 12], Role::Button, Source::Tree);
        assert!(!virtual_keypad(&blank, false), "빈 칸만으로는 키패드가 아니다 (게임 가방일 수 있다)");
        assert!(virtual_keypad(&blank, true), "인증 · 결제 문맥이면 키패드다");
        // 키패드 바로 위의 PIN 칸
        let mut with_pin = blank.clone();
        with_pin.push(region("main>input#pin", Role::Input, "비밀번호", (0.40, 0.22, 0.26, 0.05), Source::Tree));
        assert!(virtual_keypad(&with_pin, false));
        // 화면 반대쪽 구석의 채팅 입력창은 근거가 아니다
        let mut with_chat = blank.clone();
        with_chat.push(region("chat>input", Role::Input, "채팅", (0.0, 0.92, 0.3, 0.05), Source::Tree));
        assert!(!virtual_keypad(&with_chat, false));
        // 비전 후보는 라벨이 늘 비어 있어 근거가 되지 않는다
        let vision = grid("ignored", &[""; 12], Role::Image, Source::Vision);
        assert!(!virtual_keypad(&vision, true));
        // 가림 글자 · "숫자 N" 라벨
        let masked = grid("pad", &["*", "*", "*", "숫자 1", "숫자 2", "숫자 3", "", "", "", "", "", ""], Role::Button, Source::Tree);
        assert!(virtual_keypad(&masked, false));
    }

    #[test]
    fn keypad_geometry_rejects_lists_big_grids_and_scattered_cells() {
        let digits = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
        // 한 줄 목록 (1 × 10)
        let list: Vec<Region> = digits
            .iter()
            .enumerate()
            .map(|(i, d)| region(&format!("ol>li:{i}"), Role::Button, d, (0.1, 0.05 + i as f32 * 0.09, 0.08, 0.08), Source::Tree))
            .collect();
        assert!(!virtual_keypad(&list, true));
        // 4 × 4 = 16칸은 키패드 칸 수(10 ~ 14)를 넘는다
        let big: Vec<Region> = (0..16)
            .map(|i| region(&format!("g>c:{i}"), Role::Button, &(i % 10).to_string(), (0.1 + (i % 4) as f32 * 0.09, 0.1 + (i / 4) as f32 * 0.09, 0.08, 0.08), Source::Tree))
            .collect();
        assert!(!virtual_keypad(&big, true));
        // 같은 부모라도 화면 곳곳에 흩어진 칸은 격자가 아니다
        let scattered: Vec<Region> = digits
            .iter()
            .enumerate()
            .map(|(i, d)| region(&format!("main>b:{i}"), Role::Button, d, (0.02 + (i % 5) as f32 * 0.2, 0.05 + (i / 5) as f32 * 0.6, 0.05, 0.05), Source::Tree))
            .collect();
        assert!(!virtual_keypad(&scattered, true));
        // 위치 단서가 없는 칸(UIA AutomationId)도 붙어 있으면 한 묶음이다
        let mut ids = grid("x", &["7", "2", "9", "0", "4", "1", "8", "5", "3", "6", "", ""], Role::Button, Source::Tree);
        for (i, r) in ids.iter_mut().enumerate() {
            r.anchor = format!("btnKey{i}");
        }
        assert!(virtual_keypad(&ids, false));
        // 트리와 비전이 같은 칸을 함께 내도 한 번만 센다
        let mut doubled = ids.clone();
        doubled.extend(grid("v", &[""; 12], Role::Button, Source::Vision));
        assert!(virtual_keypad(&doubled, false));
    }

    #[test]
    fn otp_rows_are_not_word_boards_or_named_forms() {
        assert!(split_pin_fields(&row_of_inputs(&["인증번호 1번째 자리", "인증번호 2번째 자리", "인증번호 3번째 자리", "인증번호 4번째 자리", "인증번호 5번째 자리", "인증번호 6번째 자리"], 0.5)));
        assert!(split_pin_fields(&row_of_inputs(&["PIN", "PIN", "PIN", "PIN"], 0.5)), "4자리 PIN");
        assert!(!split_pin_fields(&row_of_inputs(&["", "", ""], 0.5)), "세 칸(전화번호 · 날짜)은 아니다");
        assert!(!split_pin_fields(&row_of_inputs(&["Year", "Month", "Day", "Hour"], 0.5)), "서로 다른 설명 이름은 보통 form");
        // 낱말 맞히기 판 (5칸 × 6줄)
        let board: Vec<Region> = (0..6).flat_map(|row| row_of_inputs(&[""; 5], 0.1 + row as f32 * 0.09)).collect();
        assert!(!split_pin_fields(&board));
        // 넓은 입력칸 넷은 OTP 가 아니다
        let wide: Vec<Region> = (0..4)
            .map(|i| region(&format!("form>input:{i}"), Role::Input, "", (0.05 + i as f32 * 0.24, 0.5, 0.22, 0.05), Source::Tree))
            .collect();
        assert!(!split_pin_fields(&wide));
        // 사이가 벌어진 칸은 이어지지 않는다
        let spread: Vec<Region> = (0..6)
            .map(|i| region(&format!("form>input:{i}"), Role::Input, "", (0.02 + i as f32 * 0.16, 0.5, 0.045, 0.07), Source::Tree))
            .collect();
        assert!(!split_pin_fields(&spread));
    }
}
