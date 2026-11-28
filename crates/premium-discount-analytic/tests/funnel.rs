//! 웹 분석 Track 1 시험: 선택 대조 규칙, 마찰 에피소드, 퍼널 (기획서 4.2 의 쇼핑몰 시나리오)

use premium_discount_analytic::event::{Element, EventKind, WebEvent};
use premium_discount_analytic::funnel::{choice_key, trace_session, ChoiceBook, ChoiceParams, SiteAnalysis};
use premium_discount_nav::{page_hash, DomNode, MarkType, Signal};
use premium_discount_verdict::Tier;

/// 결정론적 의사 난수 (xorshift)
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn node(tag: &str, id: Option<&str>, classes: &[&str], nth: u32) -> DomNode {
    DomNode { tag: tag.into(), id: id.map(Into::into), classes: classes.iter().map(|c| c.to_string()).collect(), attrs: vec![], nth_of_type: nth }
}

/// 상품 목록의 i 번째 카드 (같은 틀, 글만 다름)
fn card(i: usize) -> Element {
    Element {
        chain: vec![node("li", None, &["card"], i as u32 + 1), node("ul", Some("products"), &[], 1)],
        pug: format!("li\n  a\n    span | 상품 {i}\n  span | 무료배송"),
    }
}

fn button(id: &str, text: &str) -> Element {
    Element { chain: vec![node("button", Some(id), &["btn"], 1), node("div", None, &["pay"], 1)], pug: format!("button | {text}") }
}

fn ev(kind: EventKind, href: &str, sid: &str, seq: u32, t_ms: i64) -> WebEvent {
    WebEvent {
        kind,
        href: href.into(),
        sid: sid.into(),
        seq,
        t_ms,
        surface: None,
        tab_group: None,
        target: None,
        item: None,
        related: vec![],
        rage: false,
        dwell_ms: None,
        flips: None,
        field: None,
        dirty: None,
        at_ms: 0,
    }
}

const LIST: &str = "https://shop.example.com/list";

fn pick_from_list(sid: &str, chosen: usize, n: usize) -> WebEvent {
    let mut e = ev(EventKind::Click, LIST, sid, 1, 3_000);
    e.target = Some(card(chosen));
    e.related = (0..n).filter(|i| *i != chosen).map(card).collect();
    e
}

#[test]
fn list_cards_share_a_selector_so_the_choice_key_includes_the_text() {
    let page = page_hash(LIST).unwrap();
    let (a, b) = (card(0), card(1));
    // 셀렉터 틀은 같은 틀의 카드를 하나로 본다
    assert_eq!(premium_discount_analytic::funnel::element_of(&page, &a), premium_discount_analytic::funnel::element_of(&page, &b));
    assert_ne!(choice_key(&page, &a), choice_key(&page, &b));
}

#[test]
fn the_plan_rule_marks_a_random_list_discount() {
    // 10개 목록에서 아무 정보 없이(무작위로) 고른 2,000번
    let mut book = ChoiceBook::default();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..2000 {
        let e = pick_from_list(&format!("s{i}"), rng.below(10) as usize, 10);
        assert!(book.observe_event(&e));
    }
    assert_eq!(book.items.len(), 10);
    let p = ChoiceParams::default();
    let naive: Vec<Tier> = book.items.values().map(|t| t.naive_tier()).collect();
    let decided: Vec<Option<MarkType>> = book.items.values().map(|t| t.decide(&p)).collect();
    // 기획서 규칙: 정보가 없는데도 10개 모두 discount
    assert!(naive.iter().all(|t| *t == Tier::Discount), "{naive:?}");
    // 기댓값과 견주면 결정하지 않는다 (띠 2σ: 10개 중 하나쯤은 우연히 넘을 수 있다)
    let n_decided = decided.iter().filter(|d| d.is_some()).count();
    assert!(n_decided <= 1, "{decided:?}");
}

#[test]
fn a_dominant_choice_is_premium_and_the_ignored_one_discount() {
    // 상품 0 을 40 %, 상품 9 는 한 번도 고르지 않는다 (나머지는 고르게)
    let mut book = ChoiceBook::default();
    let mut rng = Rng(42);
    for i in 0..600 {
        let chosen = if rng.below(10) < 4 { 0 } else { 1 + rng.below(8) as usize };
        book.observe_event(&pick_from_list(&format!("s{i}"), chosen, 10));
    }
    let page = page_hash(LIST).unwrap();
    let tally = |i: usize| book.items[&choice_key(&page, &card(i)).unwrap()];
    let p = ChoiceParams::default();
    assert_eq!(tally(0).decide(&p), Some(MarkType::Premium), "{:?}", tally(0));
    assert_eq!(tally(9).decide(&p), Some(MarkType::Discount), "{:?}", tally(9));
    // 기획서 규칙으로는 가장 많이 골라진 상품도 premium 이 아니다 (40 % < 2/3)
    assert_eq!(tally(0).naive_tier(), Tier::Neutral);
    assert_eq!(book.labels[&choice_key(&page, &card(0)).unwrap()], "상품 0 무료배송");
    // 노출이 모자라면 결정하지 않는다
    let mut few = ChoiceBook::default();
    for i in 0..3 {
        few.observe_event(&pick_from_list(&format!("f{i}"), 0, 10));
    }
    assert_eq!(few.items[&choice_key(&page, &card(0)).unwrap()].decide(&p), None);
}

#[test]
fn a_link_pressed_inside_a_card_is_counted_as_the_card() {
    // 카드 안의 링크를 누르면 target 은 링크, item 은 카드다. 고른 것은 형제(카드)와 같은 단위로 센다
    let page = page_hash(LIST).unwrap();
    let mut book = ChoiceBook::default();
    let mut e = pick_from_list("s", 1, 4);
    e.item = Some(card(1));
    e.target = Some(Element { chain: vec![node("a", None, &[], 1), node("li", None, &["card"], 2)], pug: "a | 상품 1".into() });
    assert!(book.observe_event(&e));
    let chosen = &book.items[&choice_key(&page, &card(1)).unwrap()];
    assert_eq!((chosen.picks, chosen.exposures), (1, 1));
    assert_eq!(book.items.len(), 4, "the link itself is not a separate candidate");
}

#[test]
fn single_candidates_and_rage_clicks_are_not_choices() {
    let mut book = ChoiceBook::default();
    let mut lone = pick_from_list("s", 0, 1);
    assert!(lone.related.is_empty());
    assert!(!book.observe_event(&lone));
    lone = pick_from_list("s", 0, 3);
    lone.rage = true;
    assert!(!book.observe_event(&lone));
    assert!(book.items.is_empty());
}

const DETAIL: &str = "https://shop.example.com/p/1234";
const CART: &str = "https://shop.example.com/cart";
const CHECKOUT: &str = "https://shop.example.com/checkout?step=coupon";
const DONE: &str = "https://shop.example.com/done";

/// 순조로운 결제: 상품 → 장바구니 → 결제(간편결제 바로) → 완료
fn smooth_session(sid: &str) -> Vec<WebEvent> {
    let mut v = vec![ev(EventKind::Enter, DETAIL, sid, 0, 0)];
    let mut add = ev(EventKind::Click, DETAIL, sid, 1, 2_000);
    add.target = Some(button("add-to-cart", "장바구니 담기"));
    v.push(add);
    v.push(ev(EventKind::Enter, CART, sid, 2, 4_000));
    let mut order = ev(EventKind::Click, CART, sid, 3, 6_000);
    order.target = Some(button("order", "주문하기"));
    v.push(order);
    v.push(ev(EventKind::Enter, CHECKOUT, sid, 4, 8_000));
    let mut pay = ev(EventKind::Click, CHECKOUT, sid, 5, 9_500);
    pay.target = Some(button("easy-pay", "간편결제"));
    pay.related = vec![button("coupon", "쿠폰 적용")];
    v.push(pay);
    let mut submit = ev(EventKind::Submit, CHECKOUT, sid, 6, 10_000);
    submit.target = Some(button("easy-pay", "간편결제"));
    v.push(submit);
    v.push(ev(EventKind::Enter, DONE, sid, 7, 12_000));
    v
}

/// 쿠폰 칸에서 헤맨 세션: 스크롤을 뒤집고 광클하고 다시 고치다 떠난다 (완료 없음)
fn coupon_struggle(sid: &str) -> Vec<WebEvent> {
    let mut v: Vec<WebEvent> = smooth_session(sid).into_iter().take(5).collect();
    let mut s = ev(EventKind::Scroll, CHECKOUT, sid, 5, 9_000);
    s.flips = Some(4);
    v.push(s);
    let mut coupon = ev(EventKind::Click, CHECKOUT, sid, 6, 10_000);
    coupon.target = Some(button("coupon", "쿠폰 적용"));
    coupon.related = vec![button("easy-pay", "간편결제")];
    v.push(coupon);
    let mut rage = ev(EventKind::Click, CHECKOUT, sid, 7, 10_500);
    rage.target = Some(button("coupon", "쿠폰 적용"));
    rage.rage = true;
    v.push(rage);
    for (i, t) in [11_000, 13_000, 15_000].iter().enumerate() {
        let mut c = ev(EventKind::Change, CHECKOUT, sid, 8 + i as u32, *t);
        c.target = Some(button("coupon-code", ""));
        c.field = Some("text".into());
        v.push(c);
    }
    let mut leave = ev(EventKind::Leave, CHECKOUT, sid, 11, 20_000);
    leave.dirty = Some(1);
    v.push(leave);
    v
}

#[test]
fn a_struggle_becomes_signals_and_checkpoints() {
    let t = trace_session(&coupon_struggle("x"));
    let signals: Vec<Signal> = t.events.iter().map(|e| e.signal).collect();
    assert!(signals.contains(&Signal::Rescrolled { times: 4 }));
    assert!(signals.contains(&Signal::RageClick));
    assert_eq!(signals.iter().filter(|s| matches!(s, Signal::Engaged)).count(), 1);
    assert_eq!(signals.iter().filter(|s| matches!(s, Signal::Retried)).count(), 2);
    assert_eq!(signals.last(), Some(&Signal::Abandoned));
    // 들어와서 첫 동작까지 2초 (상품 페이지)
    assert_eq!(signals.first(), Some(&Signal::Searched { ms: 2_000 }));
    // 체크포인트: 페이지 3개 + 누른 요소 (광클은 체크포인트가 아니다)
    assert_eq!(t.path.len(), 3);
    assert_eq!(t.checkpoints.iter().filter(|c| c.kind == premium_discount_nav::StepKind::Page).count(), 3);
    // 쿼리가 다른 결제 페이지도 같은 페이지 해시다
    assert_eq!(page_hash(CHECKOUT), page_hash("https://shop.example.com/checkout?step=pay"));
}

#[test]
fn the_coupon_box_is_the_bottleneck_of_the_funnel() {
    let mut site = SiteAnalysis::new("shop.example.com");
    // 다른 쇼핑 흐름의 평범한 세션들로 기준선을 만든다
    for i in 0..40 {
        if i % 4 == 3 {
            site.add_session(&coupon_struggle(&format!("c{i}")));
        } else {
            site.add_session(&smooth_session(&format!("s{i}")));
        }
    }
    let steps: Vec<String> = [DETAIL, CART, CHECKOUT, DONE].iter().map(|u| page_hash(u).unwrap()).collect();
    let funnel = site.funnel(&steps);
    assert_eq!(funnel.iter().map(|s| s.reached).collect::<Vec<_>>(), [40, 40, 40, 30]);
    assert_eq!(funnel[2].dropped, 10, "결제 단계에서 10 세션이 빠진다");
    assert_eq!(funnel[2].label, "shop.example.com/checkout");

    // 병목: 쿠폰 칸 (discount), 간편결제는 premium 쪽
    let bottlenecks = site.bottlenecks();
    assert!(!bottlenecks.is_empty());
    let (top, tally) = bottlenecks[0];
    assert_eq!(tally.label, "쿠폰 적용", "{bottlenecks:?}");
    let checkout = page_hash(CHECKOUT).unwrap();
    let easy = premium_discount_analytic::funnel::element_of(&checkout, &button("easy-pay", "간편결제")).unwrap();
    assert_ne!(top, easy);
    let easy_tally = &site.elements[&easy];
    assert!(easy_tally.premium > 0 && easy_tally.discount == 0, "{easy_tally:?}");
    assert!(tally.mean_friction() > easy_tally.mean_friction());

    // 선택 대조: 결제 페이지의 두 버튼은 30 대 10 으로 갈리지만, 두 후보 묶음이라 기댓값(반반)과 견줘 결정한다
    let pay_key = choice_key(&checkout, &button("easy-pay", "간편결제")).unwrap();
    let coupon_key = choice_key(&checkout, &button("coupon", "쿠폰 적용")).unwrap();
    let p = ChoiceParams::default();
    assert_eq!(site.choices.items[&pay_key].decide(&p), Some(MarkType::Premium), "{:?}", site.choices.items[&pay_key]);
    assert_eq!(site.choices.items[&coupon_key].decide(&p), Some(MarkType::Discount));
}

/// 쿠폰을 적용하느라 늦었지만 끝까지 간 세션 (간편결제를 비켜 간 길)
fn coupon_then_done(sid: &str) -> Vec<WebEvent> {
    let mut v: Vec<WebEvent> = smooth_session(sid).into_iter().take(5).collect();
    let mut coupon = ev(EventKind::Click, CHECKOUT, sid, 5, 12_000);
    coupon.target = Some(button("coupon", "쿠폰 적용"));
    coupon.related = vec![button("easy-pay", "간편결제")];
    v.push(coupon);
    let mut submit = ev(EventKind::Submit, CHECKOUT, sid, 6, 20_000);
    submit.target = Some(button("coupon", "쿠폰 적용"));
    v.push(submit);
    v.push(ev(EventKind::Enter, DONE, sid, 7, 22_000));
    v
}

/// 사건 → 접근성 내비게이션 웹 코스까지: 고른 요소(TARGET)와 지나친 형제(RELATED)가 nav 의 요소 체크포인트(page_hash +
/// selector_template)로 이어지고, 완주한 세션들의 코스를 nav 의 서버 규칙(route_marks)으로 견주면 결제 페이지에 막 들어온
/// 방문자에게 다음 단계로 간편결제를 premium 으로 권한다(추천 경로). 쿠폰 칸은 같은 세션들의 마찰 에피소드에서 병목(discount)이다.
#[test]
fn sessions_become_nav_checkpoints_a_bottleneck_and_a_premium_next_step() {
    use premium_discount_analytic::funnel::element_of;
    use premium_discount_nav::{app_hash, element_hash, route_marks, selector_template, CheckpointTally, PublicCourse, Run, StepRef};
    use std::collections::BTreeMap;

    let checkout = page_hash(CHECKOUT).unwrap();
    let done = page_hash(DONE).unwrap();
    let easy = button("easy-pay", "간편결제");
    let coupon = button("coupon", "쿠폰 적용");
    // TARGET · RELATED 의 체크포인트는 nav 웹 코스의 요소 해시 (같은 DOM 조상 → 같은 셀렉터 틀)
    let easy_hash = element_hash(&checkout, &selector_template(&easy.chain).unwrap()).unwrap();
    assert_eq!(element_of(&checkout, &easy).as_deref(), Some(easy_hash.as_str()));
    let smooth = trace_session(&smooth_session("probe"));
    assert!(smooth.checkpoints.iter().any(|c| c.hash == easy_hash), "the pressed TARGET is a course checkpoint");
    assert_ne!(element_of(&checkout, &coupon).unwrap(), easy_hash);

    // 30 세션은 간편결제로 끝내고, 10 세션은 쿠폰으로 늦게 끝내고, 10 세션은 쿠폰 칸에서 헤매다 떠난다
    let mut site = SiteAnalysis::new("shop.example.com");
    let app = app_hash("shop.example.com").unwrap();
    let mut runs: Vec<Run> = Vec::new();
    for i in 0..50 {
        let session = match i % 5 {
            0..=2 => smooth_session(&format!("s{i}")),
            3 => coupon_then_done(&format!("d{i}")),
            _ => coupon_struggle(&format!("c{i}")),
        };
        site.add_session(&session);
        let trace = trace_session(&session);
        if trace.path.last() == Some(&done) {
            runs.push(Run::from_checkpoints(&app, &trace.checkpoints, None).expect("a completed session is a run"));
        }
    }
    assert_eq!(runs.len(), 40);

    // 병목 (마찰 에피소드 · FrictionBaselines): 쿠폰 칸
    let bottlenecks = site.bottlenecks();
    assert_eq!(bottlenecks.first().map(|(_, t)| t.label.as_str()), Some("쿠폰 적용"), "{bottlenecks:?}");
    assert_eq!(site.funnel(&[checkout.clone(), done.clone()])[0].dropped, 10);

    // 고스트 = 가장 많이 달린 코스, 체크포인트 판정은 서버와 같은 route_marks
    let mut by_course: BTreeMap<&str, Vec<&Run>> = BTreeMap::new();
    for r in &runs {
        by_course.entry(r.course.as_str()).or_default().push(r);
    }
    let (ghost_course, ghost_runs) = by_course.iter().max_by_key(|(_, v)| v.len()).unwrap();
    assert_eq!(ghost_runs.len(), 30);
    let ghost: Vec<StepRef> = ghost_runs[0].refs();
    let mut tallies = vec![CheckpointTally::default(); ghost.len()];
    for r in &runs {
        for (step, mark) in route_marks(&r.refs(), &ghost).unwrap_or_default() {
            let at = ghost.iter().position(|g| *g == step).unwrap();
            match mark {
                MarkType::Premium => tallies[at].premium += 1,
                MarkType::Discount => tallies[at].discount += 1,
            }
        }
    }
    let easy_at = ghost.iter().position(|s| s.hash == easy_hash).expect("the ghost passes the easy-pay button");
    assert_eq!((tallies[easy_at].premium, tallies[easy_at].discount), (30, 10), "the coupon finishers went around it");
    let public = PublicCourse {
        course: ghost_course.to_string(),
        steps: ghost.clone(),
        runs: ghost_runs.len() as u32,
        premium: 0,
        discount: 0,
        record: ghost_runs[0].splits(),
        average: ghost_runs[0].splits(),
        checkpoints: tallies,
        verdict: None,
        interaction_traits: 0,
    };

    // 결제 페이지에 막 들어온 새 방문자: 다음 단계는 간편결제, premium
    let progress = trace_session(&smooth_session("visitor").into_iter().take(5).collect::<Vec<_>>()).checkpoints;
    let guide = premium_discount_nav::Collection::default().guide(&app, &progress, &[public]).expect("a course fits");
    assert_eq!(guide.course, *ghost_course);
    let next = guide.next.expect("not at the finish yet");
    assert_eq!(next.hash, easy_hash);
    assert_eq!(guide.next_tier, Tier::Premium);
}

#[test]
fn cn253_cross_boundary_1hop_post_pick_classifies_dark_pattern_button_as_friction_trap() {
    use premium_discount_analytic::funnel::{canonical_of, post_pick_windows_v2, AbandonPolicy, DebiasedChoiceBook, WebElementKind};
    use premium_discount_nav::BehaviorParams;
    let params = BehaviorParams::default();
    let mut book = DebiasedChoiceBook::default();
    let p1 = "https://shop.example/product/100";
    let p2_bad = "https://shop.example/dead-promo";
    let p2_ok = "https://shop.example/checkout";
    let trap_btn = button("promo-trap", "특가 쿠폰 받기");
    let safe_btn = button("buy-now", "바로 구매");
    let peer_btn = button("wish", "찜하기");
    let dead_box = button("retry", "재시도");

    // 6 clean baseline sessions pick safe_btn and submit on checkout.
    for s in 0..6 {
        let sid = format!("ok-{s}");
        let mut click = ev(EventKind::Click, p1, &sid, 1, 1_000);
        click.target = Some(safe_btn.clone());
        click.related = vec![trap_btn.clone(), peer_btn.clone()];
        let events = vec![
            ev(EventKind::Enter, p1, &sid, 0, 0),
            click.clone(),
            ev(EventKind::Enter, p2_ok, &sid, 2, 1_400),
            ev(EventKind::Submit, p2_ok, &sid, 3, 4_000),
        ];
        book.observe_event(&click);
        for pp in post_pick_windows_v2(&events, &params, AbandonPolicy::Always) {
            book.observe_post_pick(&pp.choice.stable_key, &pp.page, pp.friction, pp.completed, pp.abandoned, params.eps);
        }
    }
    // 12 sessions click attractive trap_btn on P1 (z_choice > +2.0!), navigate to P2 in 400ms, then immediately rage-click & backtrack/leave within 3s.
    for s in 0..12 {
        let sid = format!("trap-{s}");
        let mut click = ev(EventKind::Click, p1, &sid, 1, 1_000);
        click.target = Some(trap_btn.clone());
        click.related = vec![safe_btn.clone(), peer_btn.clone()];
        let mut rage_ev = ev(EventKind::Click, p2_bad, &sid, 3, 2_500);
        rage_ev.target = Some(dead_box.clone());
        rage_ev.rage = true;
        let events = vec![
            ev(EventKind::Enter, p1, &sid, 0, 0),
            click.clone(),
            ev(EventKind::Enter, p2_bad, &sid, 2, 1_400),
            rage_ev,
            ev(EventKind::Enter, p1, &sid, 4, 4_000),
            ev(EventKind::Leave, p1, &sid, 5, 4_200),
        ];
        book.observe_event(&click);
        for pp in post_pick_windows_v2(&events, &params, AbandonPolicy::Always) {
            book.observe_post_pick(&pp.choice.stable_key, &pp.page, pp.friction, pp.completed, pp.abandoned, params.eps);
        }
    }

    let page_h = page_hash(p1).unwrap();
    let trap_choice = canonical_of(&page_h, &trap_btn);
    let diag = book.diagnose(&trap_choice.stable_key).expect("trap diagnosed");
    assert_eq!(diag.kind, WebElementKind::FrictionTrap, "1-Hop bounce/rage must veto GoldenPath into FrictionTrap: {diag:?}");
    assert!(diag.abandon_rate.unwrap_or(0.0) >= 0.5);
    assert!(diag.post_friction_z.unwrap_or(0.0) >= 1.0);
}

#[test]
fn peek_and_return_context_is_benign_only_when_clean() {
    use premium_discount_analytic::funnel::PostPickContext;
    let benign = PostPickContext { dwell_ms: 9_000, tts_budget_ms: 3_600, rage_or_retry_count: 0, clean_dismiss_key: true, parent_progressed: true };
    assert!(benign.is_benign_peek_and_return(), "9 s is inside 8 s + 3.6 s of listening");
    assert!(!PostPickContext::default().is_benign_peek_and_return(), "the default context keeps the old verdicts");
    assert!(!PostPickContext { dwell_ms: 12_000, ..benign }.is_benign_peek_and_return());
    assert!(!PostPickContext { rage_or_retry_count: 1, ..benign }.is_benign_peek_and_return());
    assert!(!PostPickContext { clean_dismiss_key: false, ..benign }.is_benign_peek_and_return());
    assert!(!PostPickContext { parent_progressed: false, ..benign }.is_benign_peek_and_return());
    assert!(PostPickContext { dwell_ms: 7_900, tts_budget_ms: 0, ..benign }.is_benign_peek_and_return(), "sighted users get the plain 8 s");
}

/// 스크린리더 사용자가 옵션 대화상자를 열어 첫 안내만 듣고 뒤로 닫은 뒤 부모 화면에서 이어 간다 (PeekAndReturn).
/// `rage` 면 대화상자에서 광클하고, `leave` 면 돌아와서 떠난다 (둘 다 면책이 아니다)
fn options_peek_session(sid: &str, peek_btn: &Element, peers: &[Element], next_btn: &Element, rage: bool, leave: bool) -> Vec<WebEvent> {
    let p1 = "https://app.example/settings";
    let p2 = "https://app.example/settings/advanced";
    let mut pick = ev(EventKind::Click, p1, sid, 1, 1_000);
    pick.target = Some(peek_btn.clone());
    pick.related = peers.to_vec();
    let mut events = vec![ev(EventKind::Enter, p1, sid, 0, 0), pick, ev(EventKind::Enter, p2, sid, 2, 1_400)];
    if rage {
        let mut r = ev(EventKind::Click, p2, sid, 3, 2_000);
        r.target = Some(button("toggle", "켜기"));
        r.rage = true;
        events.push(r);
    }
    events.push(ev(EventKind::Back, p1, sid, 4, 4_400));
    if leave {
        events.push(ev(EventKind::Leave, p1, sid, 5, 5_000));
    } else {
        let mut next = ev(EventKind::Click, p1, sid, 5, 5_000);
        next.target = Some(next_btn.clone());
        events.push(next);
        events.push(ev(EventKind::Submit, p1, sid, 6, 7_000));
    }
    events
}

#[test]
fn a_screen_reader_peek_and_return_is_not_a_friction_trap() {
    use premium_discount_analytic::funnel::{canonical_of, post_pick_windows_v2, post_pick_windows_v2_with, AbandonPolicy, DebiasedChoiceBook, WebElementKind};
    use premium_discount_nav::{reading_grace_ms, BehaviorParams};
    let params = BehaviorParams::default();
    let peek_btn = button("advanced", "고급 설정");
    let other = button("basic", "기본 설정");
    let peer = button("reset", "초기화");
    let next_btn = button("save", "저장");
    let tts = reading_grace_ms(30);
    assert_eq!(tts, 3_600);
    let run = |peek: Option<u32>, rage: bool, leave: bool| {
        let mut book = DebiasedChoiceBook::default();
        for s in 0..6 {
            // the baseline: another option, finished on the page
            let sid = format!("base-{s}");
            let mut click = ev(EventKind::Click, "https://app.example/settings", &sid, 1, 1_000);
            click.target = Some(other.clone());
            click.related = vec![peek_btn.clone(), peer.clone()];
            let events = vec![ev(EventKind::Enter, "https://app.example/settings", &sid, 0, 0), click.clone(), ev(EventKind::Submit, "https://app.example/settings", &sid, 2, 3_000)];
            book.observe_event(&click);
            for w in post_pick_windows_v2_with(&events, &params, AbandonPolicy::Always, peek) {
                book.observe_window(&w, params.eps);
            }
        }
        let mut peeks = 0;
        for s in 0..12 {
            let events = options_peek_session(&format!("sr-{s}"), &peek_btn, &[other.clone(), peer.clone()], &next_btn, rage, leave);
            book.observe_event(&events[1]);
            for w in post_pick_windows_v2_with(&events, &params, AbandonPolicy::Always, peek) {
                peeks += usize::from(w.peek);
                book.observe_window(&w, params.eps);
            }
        }
        let key = canonical_of(&page_hash("https://app.example/settings").unwrap(), &peek_btn).stable_key;
        (book.diagnose(&key).expect("diagnosed").kind, peeks)
    };
    // without the exemption (v2 as before): every look-and-close is a 1-hop bounce → friction trap
    assert_eq!(run(None, false, false), (WebElementKind::FrictionTrap, 0));
    let plain: Vec<_> = post_pick_windows_v2(&options_peek_session("x", &peek_btn, &[other.clone()], &next_btn, false, false), &params, AbandonPolicy::Always);
    assert!(plain.iter().any(|w| w.abandoned && !w.peek), "v2 is unchanged: {plain:?}");
    // with it: the clean peeks stay out of friction and of the abandon denominator
    let (kind, peeks) = run(Some(tts), false, false);
    assert_eq!(peeks, 12);
    assert_ne!(kind, WebElementKind::FrictionTrap);
    // a rage click in the dialog, or leaving after coming back, is still friction
    assert_eq!(run(Some(tts), true, false), (WebElementKind::FrictionTrap, 0));
    assert_eq!(run(Some(tts), false, true), (WebElementKind::FrictionTrap, 0));
}
