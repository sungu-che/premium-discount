//! premium-discount-nav: 접근성 내비게이션의 코어. premium-discount 앱과 raon-zena 가 같은 규칙을 쓰도록 라이브러리로 둔다.
//!
//! 1. 화면에서 모은 후보 영역(접근성 트리 · 비전 · 이벤트 카메라)을 하나로 합치고([`merge`]), 판정 등급을 반영해
//!    번호 1 ~ 9 를 매긴다([`number`]). 같은 화면을 계속 다시 매기는 오버레이는 번호가 깜빡이지 않게 [`StickyNumberer`] 로 매긴다.
//! 2. 행동 기록(번호 선택 · 화면 변화 · form change)을 에피소드로 자르고, SDS 로 premium · discount 를 결정한다
//!    ([`split_episodes`], [`FrictionBaselines::decide`]). 늘 결정할 수는 없다: 띠 안이면 결정하지 않는다.
//!    진행 중인 에피소드는 기준선을 바꾸지 않고 읽어 실시간 경보 단계를 낸다([`FrictionBaselines::peek_live`], [`LiveEpisodeWindow`]).
//! 3. 결정이 난 것만 결정 기록([`NavMark`], `navmark/1`)이 된다: 해시 · 벡터들 · 종류 + 날짜. 계정 · IP · 원시 이벤트 ·
//!    입력값 · 화면 글자가 없고, 기기에만 쌓는다(게스트도). 해시는 서버와 같은 `hashId` 다([`hash_id`]).
//! 4. 시작부터 결승까지의 플로우를 타임트라이얼(`trial/1`)로 기록한다: 코스 = 체크포인트(화면 · 영역 해시) 10개까지,
//!    런 = 구간 시간 + SDS 결정, 앱 · 웹마다 코스 10개의 콜렉션(같은 코스는 횟수로), 진행 중에는 내 최고 기록 ·
//!    다른 사용자의 기록을 고스트로 안내한다([`Run`], [`Collection`], [`Collection::guide`]). 서버로는 런만 올린다.
//!    체크포인트는 앱이면 화면(UIA 구조) · 영역, 웹이면 페이지(링크값, [`page_hash`]) · 요소(DOM 셀렉터, [`selector_template`],
//!    [`element_hash`])다. 스와이프 · 스크롤로 지난 체크포인트에는 제스처가 붙는다([`classify_stroke`], [`scroll_bursts`]).
//!    곁길은 접고 시작 · 결승은 지켜 코스로 다듬고([`reduce_course_points`]), 곁길 · 지름길 · 제자리 헤맴까지 보는 실시간 안내도 있다([`Collection::guide_live`]).
//! 5. 같은 화면에 처음 들어온 사용자를 위한 영역 지도([`NavMap`], `navmap/1`)도 만든다. 이름은 개인정보를 거른다([`scrub_name`]).
//! 6. 개인정보 화면(비밀번호 · 결제 칸, [`sensitive_screen`])은 트리로만 번호를 매기고 기록 · 지도 · 체크포인트를 만들지 않는다.
//!    속성 없는 보안 키패드 · OTP 칸과 창 · 비전 신호까지 합쳐 판정하고([`evaluate_privacy`]), 잠금을 풀 때는 상태 머신이 정한다([`PrivacyStateMachine`]).
//!
//! 모델(SigLIP2 · granite) · 저장소(LanceDB) · HTTP 에 의존하지 않는다. 벡터는 앱이 임베딩해서 넘긴다.

mod behavior;
mod credit;
mod dom;
mod gesture;
mod hash;
mod mark;
mod navmap;
mod order;
mod portfolio;
mod privacy;
mod region;
mod resume;
mod speech;
mod trial;

pub use behavior::{
    reading_grace_ms, split_episodes, split_episodes_with_freezes, BehaviorParams, Decision, Episode, Event, FrictionAlertStage, FrictionBaselines, Judgement, LiveEpisodeWindow, LiveFrictionReading,
    LiveParams, LiveProbeContext, Signal, UniversalChoiceBook, UniversalChoiceEntry, UniversalChoiceKind, Welford, CROSS_BOUNDARY_BOUNCE_WINDOW_MS, CROSS_BOUNDARY_HOP_DECAY, IDLE_AFK_CUTOFF_MS,
    PRIVACY_PURGE_MS, READING_MS_PER_CHAR,
};
pub use credit::{
    accept_window_ms, listening_adjusted_ms, run_credit, step_fault_mask, InputSurfaceRole, RunCredit, StepOutcome, ACCEPT_WINDOW_MS, BOTTLENECK_Z, NOOP_CLICK_FAULT,
    SCREEN_READER_ACCEPT_WINDOW_MS,
};
pub use dom::{selector_template, DomNode, SELECTOR_DEPTH};
pub use gesture::{
    classify_stroke, classify_stroke_adaptive, filter_keyboard_bounces, filter_spastic_bounces, rage_presses, rage_presses_adaptive, scroll_bursts, Gesture, GestureParams, KeyPress,
    KeyVerdict, KinematicProfile, PointerKind, PointerSample, ScrollBurst, ScrollSample, Stroke, BACKSPACE_KEY, BASE_HOLD_MS, HOLD_SHIFT_MARGIN_MS, KEY_BOUNCE_MS,
    MODIFIER_SLIP_RECOVERY_MS, SPASTIC_BOUNCE_MS,
};
pub use hash::{app_hash, element_hash, hash_id, hash_message, page_hash, region_hash, screen_hash, structure_template, url_template};
pub use mark::{
    canonical_space_dims, canonical_vision_space, dequantize_unit_i8, is_uploadable_space, quantize_unit_i8, region_label, screen_label, CanonicalSpace, MarkError, MarkType, MarkVector, NavMark,
    CANONICAL_SPACES, HASH_NGRAM_SPACE, LABEL_SPACE, MARK_SCHEMA, VISION_SPACE_RAON_VE, VISION_SPACE_SIGLIP2_HF,
};
pub use navmap::{MapRegion, NavMap, NAVMAP_SCHEMA};
pub use order::{grid_subrect, number, FrameContext, Numbered, SlotOrigin, StickyConfig, StickyNumberer, MAX_NUMBERED};
pub use portfolio::{
    is_course_superseded_preserving_traits, scrub_workspace_identity, select_stratified_portfolio, PaceScale, PortfolioCourse, BRANCH_TOKEN, PATH_TOKEN, PER_GOAL_CAP,
    PROTECTED_ACCESSIBLE_TRAITS, QUOTA_ACCESSIBLE, QUOTA_ELITE, QUOTA_ENTRY, QUOTA_POPULAR, TIME_TOKEN, VERSION_TOKEN,
};
pub use privacy::{
    evaluate_privacy, evaluate_privacy_inline, privacy_triggers, scrub_anchor, scrub_name, split_pin_fields, virtual_keypad, InlineRedactionZone, PrivacyProbeInput, PrivacyShield,
    PrivacyTrigger, RedactionKind, NAME_MAX_CHARS,
};
pub use region::{merge, merge_absorbing, sensitive_screen, Rect, Region, Role, Source, CONTAIN_ABSORB, MERGE_IOU};
pub use resume::{
    frozen_overlap_ms, merge_frozen_spans, payment_route, receipt_finish_page, receipt_route, PrivacyPhase, PrivacyStateMachine, PrivacyTransitionCue, ResumeStepOutcome, StepContext,
    SurfaceSignature, ZeroPixelProbe, BRIDGE_TTL_MS, GRACE_MS, GRACE_TICKS, POST_PAYMENT_ARM_MS,
};
pub use speech::{CoalesceOutcome, SpeechChunkMerger, SpeechCoalescer, SPEECH_COALESCE_WINDOW_MS};
pub use trial::{
    checkpoint_id, compute_interaction_traits, contextual_ghost_utility, course_chain, course_hash, dead_end_mark_if_eligible, ghost_utility, ghost_utility_v3, is_course_superseded,
    reduce_course_points, route_marks, route_marks_v2, selection_gesture, shortcut_wins, shortcut_wins_v2, tier, AbandonContext, AccessibilityRoutePrefs, Added, Checkpoint, CheckpointTally,
    Collection, CourseEntry, DeadEndMark, GhostSyncState, Guide, LiveGuide, MarkOutcome, PersonalPaceProfile, PublicCourse, ReduceOptions, RouteEvalContext, Run, ShortcutKind, ShortcutWin,
    ShortcutWinV2, Step, StepKind, StepRef, TrialError, VerdictSummary, DEAD_END_MIN_FRICTION_Z, MAX_COURSES, MAX_DETOUR_LOOP_MS, MAX_RUN_MS, MAX_STEPS, MIN_SHORTCUT_MARGIN,
    MIN_SHORTCUT_SAVED_MS, SCROLL_REWIND_MS, SELECTION_SWIPE_MIN_PX, SLRU_MAX_PINNED, SLRU_PROTECTED_MIN_RUNS, SPLIT_UNIT_MS, TIER_MIN_DECIDED, TRAIT_LOW_DENSITY_CALM, TRAIT_NO_FINE_GESTURE,
    TRAIT_TREE_ACCESSIBLE, TRIAL_SCHEMA,
};

#[cfg(test)]
pub(crate) mod testing {
    use crate::{Rect, Region, Role, Source};

    pub fn region(anchor: &str, role: Role, name: &str, rect: (f32, f32, f32, f32), source: Source) -> Region {
        Region {
            anchor: anchor.into(),
            role,
            name: name.into(),
            rect: Rect::new(rect.0, rect.1, rect.2, rect.3).unwrap(),
            source,
            score: 1.0,
            changed: false,
            sensitive: false,
        }
    }
}
