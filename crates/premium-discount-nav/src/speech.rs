//! 끊어 들어오는 발화 조각을 하나로 잇는다 (구음장애 · 말더듬 · 스크린리더 사용자의 끊어 말하기, AC-05).
//!
//! raon-zena `zena-core::reference` 의 `SpeechCoalescer` 를 옮겼다(raon-zena 는 이것을 다시 내보낸다). 더한 것: 아직 잇는 중인지
//! ([`SpeechCoalescer::in_flight`]): 잇는 동안에는 먼저 말을 걸지 않는다(조각 사이 침묵에 끼어들지 않게).
//!
//! - 조각이 끝나지 않았으면(말줄임 · 쉼표 · 간투사 · 종결 없는 두 낱말 이하) 기다리고, 끝났으면 모은 것을 낸다.
//! - 앞 조각에서 [`SPEECH_COALESCE_WINDOW_MS`] 가 지나면 모은 것을 버리고 새로 시작한다 ([`SpeechCoalescer::flush_if_due`] 는 낸다).
//! - 명시적 정정 · 취소("아니", "취소", "말고", "cancel", "stop" …)는 모은 것을 버리고 그 발화를 바로 낸다.

/// 조각을 잇는 창 (ms)
pub const SPEECH_COALESCE_WINDOW_MS: u64 = 2_500;

/// 끊어 들어오는 발화 조각을 문장이 끝날 때까지 잇는다
#[derive(Debug, Clone, Default)]
pub struct SpeechCoalescer {
    fragments: Vec<String>,
    last_at_ms: Option<u64>,
    pub window_ms: u64,
}

/// 기획서의 이름
pub type SpeechChunkMerger = SpeechCoalescer;

/// 조각 하나를 넣은 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoalesceOutcome {
    /// 아직 문장이 끝나지 않아 다음 조각을 기다린다
    Buffering,
    /// 이은 발화
    Ready(String),
    /// 정정 · 취소가 들어와 모은 것을 버리고 이 발화를 낸다
    CancelledAndReplaced(String),
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl SpeechCoalescer {
    pub fn new() -> Self {
        Self {
            fragments: Vec::new(),
            last_at_ms: None,
            window_ms: SPEECH_COALESCE_WINDOW_MS,
        }
    }

    fn is_cancel_or_correction(s: &str) -> bool {
        let t = s.trim().to_ascii_lowercase();
        ["아니", "취소", "말고", "그만", "cancel", "no wait", "stop"]
            .iter()
            .any(|k| t.starts_with(k) || t.contains(" 말고") || t.contains("취소"))
    }

    fn is_incomplete_fragment(s: &str) -> bool {
        let t = s.trim();
        if t.is_empty() {
            return true;
        }
        if t.ends_with("...") || t.ends_with('…') || t.ends_with(',') {
            return true;
        }
        let stripped = t.trim_end_matches(|c: char| matches!(c, '.' | '…' | ',' | ' '));
        let filler = matches!(stripped, "어" | "음" | "그" | "저" | "이" | "그거" | "이거" | "uh" | "um" | "the");
        if filler {
            return true;
        }
        // 조사 · 관형어로 끝나고 종결어미 · 물음표가 없는 짧은 조각
        let ends_complete = stripped.ends_with('?')
            || stripped.ends_with('!')
            || stripped.ends_with('요')
            || stripped.ends_with('줘')
            || stripped.ends_with('해')
            || stripped.ends_with('야')
            || stripped.ends_with('까')
            || stripped.ends_with('다')
            || stripped.ends_with("press")
            || stripped.ends_with("click");
        !ends_complete && stripped.split_whitespace().count() <= 2
    }

    fn clean_fragment(s: &str) -> String {
        let stripped = s.trim().trim_end_matches(|c: char| matches!(c, '.' | '…' | ',')).trim();
        // 말더듬 간투사 떼기 ("어... 2번" → "2번")
        let mut parts: Vec<&str> = stripped.split_whitespace().collect();
        while parts
            .first()
            .is_some_and(|w| matches!(w.trim_end_matches(|c: char| matches!(c, '.' | '…')), "어" | "음" | "uh" | "um"))
        {
            parts.remove(0);
        }
        parts.join(" ")
    }

    /// 조각 하나를 넣는다
    pub fn push(&mut self, fragment: &str, now_ms: u64) -> CoalesceOutcome {
        let raw = fragment.trim();
        if raw.is_empty() {
            return CoalesceOutcome::Buffering;
        }
        if Self::is_cancel_or_correction(raw) {
            self.fragments.clear();
            self.last_at_ms = None;
            return CoalesceOutcome::CancelledAndReplaced(collapse_ws(raw));
        }
        if let Some(prev) = self.last_at_ms {
            if now_ms.saturating_sub(prev) > self.window_ms {
                self.fragments.clear();
            }
        }
        let cleaned = Self::clean_fragment(raw);
        if !cleaned.is_empty() {
            self.fragments.push(cleaned);
        }
        self.last_at_ms = Some(now_ms);
        if Self::is_incomplete_fragment(raw) {
            CoalesceOutcome::Buffering
        } else {
            let out = collapse_ws(&self.fragments.join(" "));
            self.fragments.clear();
            self.last_at_ms = None;
            CoalesceOutcome::Ready(out)
        }
    }

    /// 창이 지나면 모은 조각을 낸다
    pub fn flush_if_due(&mut self, now_ms: u64) -> Option<String> {
        let last = self.last_at_ms?;
        if now_ms.saturating_sub(last) >= self.window_ms && !self.fragments.is_empty() {
            let out = collapse_ws(&self.fragments.join(" "));
            self.fragments.clear();
            self.last_at_ms = None;
            Some(out)
        } else {
            None
        }
    }

    /// 아직 잇는 중인가: 모은 조각이 있고 마지막 조각에서 창이 지나지 않았다. 참인 동안 먼저 말을 걸지 않는다
    /// (조각 사이의 침묵을 말이 끝난 것으로 보고 끼어들면 발화가 부딪친다)
    pub fn in_flight(&self, now_ms: u64) -> bool {
        !self.fragments.is_empty() && self.last_at_ms.is_some_and(|last| now_ms.saturating_sub(last) < self.window_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuttered_fragments_merge_and_a_correction_replaces_them() {
        let mut coal = SpeechCoalescer::new();
        assert_eq!(coal.push("그...", 1_000), CoalesceOutcome::Buffering);
        assert_eq!(coal.push("어... 2번", 1_900), CoalesceOutcome::Buffering);
        assert_eq!(coal.push("눌러줘", 2_800), CoalesceOutcome::Ready("그 2번 눌러줘".to_string()));
        assert_eq!(coal.push("어... 3번", 5_000), CoalesceOutcome::Buffering);
        assert_eq!(coal.push("아니 취소 1번으로", 5_700), CoalesceOutcome::CancelledAndReplaced("아니 취소 1번으로".to_string()));
        assert_eq!(coal.flush_if_due(10_000), None);
    }

    #[test]
    fn it_is_in_flight_through_the_silence_between_fragments() {
        let mut coal = SpeechChunkMerger::new();
        assert!(!coal.in_flight(0), "nothing buffered");
        assert_eq!(coal.push("그 표의", 1_000), CoalesceOutcome::Buffering);
        // 1.2초 쉼 (낱말 사이 침묵): 아직 잇는 중이라 먼저 말을 걸지 않는다
        assert!(coal.in_flight(2_200));
        assert!(coal.in_flight(1_000 + SPEECH_COALESCE_WINDOW_MS - 1));
        assert!(!coal.in_flight(1_000 + SPEECH_COALESCE_WINDOW_MS), "the window ran out");
        assert_eq!(coal.push("셋째 칸 읽어줘", 2_300), CoalesceOutcome::Ready("그 표의 셋째 칸 읽어줘".to_string()));
        assert!(!coal.in_flight(2_400), "emitted");
        // 창이 지난 뒤의 낼 것
        assert_eq!(coal.push("음,", 10_000), CoalesceOutcome::Buffering);
        assert_eq!(coal.flush_if_due(10_000 + SPEECH_COALESCE_WINDOW_MS), None, "a filler alone leaves nothing to emit");
    }
}
