//! The tools against a scripted premium-discount-app, and the real binary over MCP.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use premium_discount_mcp::address::{content_address, hash_id};
use premium_discount_mcp::{error_kind, Backend, CallError, PdSyncServer, TOOLS};
use serde_json::{json, Value};
use premium_discount_mcp_protocol::{result_json, McpClient, ToolHandler};

const ID: &str = "0190f0a0-0000-7000-8000-0000000000aa";
const OTHER_ID: &str = "0190f0a0-0000-7000-8000-0000000000bb";

/// premium-discount-app stand-in: records `(cmd, args)` and answers from a script.
#[derive(Clone, Default)]
struct Scripted {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    replies: Arc<Mutex<VecDeque<Result<Value, CallError>>>>,
}

impl Scripted {
    fn reply(&self, r: Result<Value, CallError>) {
        self.replies.lock().unwrap().push_back(r);
    }
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Backend for Scripted {
    fn call(&mut self, cmd: &str, args: Value) -> Result<Value, CallError> {
        self.calls.lock().unwrap().push((cmd.to_string(), args));
        self.replies.lock().unwrap().pop_front().unwrap_or(Ok(Value::Null))
    }
}

fn server() -> (PdSyncServer<Scripted>, Scripted) {
    let app = Scripted::default();
    (PdSyncServer::new(app.clone(), "http://127.0.0.1:9"), app)
}

#[test]
fn enqueue_maps_the_drop_envelope_onto_submit() {
    let (mut s, app) = server();
    app.reply(Ok(json!({"status": "queued", "local_id": "local-7"})));
    let out = s
        .call(
            "pd_enqueue_node",
            &json!({
                "text": "  보스 패턴: 빙결 회피가 우세  ",
                "cc": "app://EldenRing.exe/boss_fight",
                "to_key": "ELDEN RING™",
                "bcc_key": "Frost  breath\n— dodge left",
                "parent_local": "local-6",
            }),
        )
        .unwrap();
    let (cmd, args) = &app.calls()[0];
    assert_eq!(cmd, "cmd_submit_evaluation");
    assert_eq!(args["cc"], "eldenring.exe");
    assert_eq!(args["to"], hash_id("ELDEN RING™").unwrap());
    assert_eq!(args["bcc"], content_address("Frost breath — dodge left").unwrap());
    assert_eq!(args["local_parent"], "local-6");
    assert_eq!(args["ref"], Value::Null);
    assert_eq!(out["status"], "queued");
    assert_eq!(out["local_id"], "local-7");
    assert_eq!((out["cc_kind"].as_str(), out["scene"].as_str()), (Some("app"), Some("boss-fight")));

    // Rows come back as premium-discount-app wrote them; pending votes keep the timer running.
    app.reply(Ok(
        json!({"sent": 1, "remaining": 0, "still_pending": 0, "votes_remaining": 1, "stopped": null}),
    ));
    app.reply(Ok(json!([
        {"request_id": "r1", "id": ID, "state": "stored", "updated_at": 42, "data": {"local_id": "local-7"}},
        {"request_id": "r2", "id": "", "state": "queued", "updated_at": 40, "data": {"local_id": "local-8"}},
    ])));
    let synced = s.call("pd_flush_and_sync", &json!({"since_ms": 10})).unwrap();
    let calls = app.calls();
    assert_eq!(calls[1].0, "cmd_flush_outbox");
    assert_eq!(calls[2], ("cmd_sync_cache".to_string(), json!({"since": 10})));
    assert_eq!(synced["next_since_ms"], 42);
    assert_eq!(synced["has_work"], true, "a queued vote is work too");
    assert_eq!(synced["counts"], json!({"queued": 1, "stored": 1}));
}

#[test]
fn search_passes_the_tier_filter_and_returns_server_verdicts() {
    let (mut s, app) = server();
    app.reply(Ok(json!({"matches": [
        {"id": ID, "score": 0.9, "source": "remote", "verdict": {"tier": "premium", "grade": 2, "rank": 0.97, "evidence": 3.0}, "my_vote": 1},
    ], "remote_error": null})));
    let found = s
        .call(
            "pd_search_merged",
            &json!({"query": "빙결", "cc": "https://www.eldenring.exe/", "tier": "premium"}),
        )
        .unwrap();
    let args = &app.calls()[0].1;
    assert_eq!(
        (args["cc"].as_str(), args["tier"].as_str(), args["top_k"].as_u64()),
        (Some("eldenring.exe"), Some("premium"), Some(10))
    );
    assert_eq!(found["matches"][0]["verdict"]["tier"], "premium");
    assert_eq!(found["matches"][0]["my_vote"], 1);
    assert_eq!(found["cc_kind"], "web");

    let e = format!(
        "{:#}",
        s.call("pd_search_merged", &json!({"query": "q", "tier": "gold"})).unwrap_err()
    );
    assert_eq!(error_kind(&e), Some("invalid_input"));
}

#[test]
fn votes_and_verdicts_map_onto_the_app_commands() {
    let (mut s, app) = server();
    app.reply(Ok(json!({"status": "sent", "request_id": "r", "counted": true})));
    let out = s.call("pd_vote", &json!({"id": ID, "value": -1})).unwrap();
    assert_eq!(
        app.calls()[0],
        ("cmd_vote".to_string(), json!({"id": ID, "value": -1, "kind": "vote"}))
    );
    assert_eq!(out["status"], "sent");

    app.reply(Ok(
        json!({"verdicts": [{"id": ID, "tier": "discount", "grade": -1}], "missing": [], "stale": false, "remote_error": null}),
    ));
    let out = s.call("pd_verdicts", &json!({"ids": [ID], "refresh": true})).unwrap();
    assert_eq!(app.calls()[1], ("cmd_verdicts".to_string(), json!({"ids": [ID], "refresh": true})));
    assert_eq!(out["verdicts"][0]["tier"], "discount");

    app.reply(Ok(json!({"old": ID, "new": OTHER_ID, "request_id": "sup-0"})));
    let out = s.call("pd_supersede", &json!({"old": ID, "new": OTHER_ID})).unwrap();
    assert_eq!(app.calls()[2], ("cmd_supersede".to_string(), json!({"old": ID, "new": OTHER_ID})));
    assert_eq!(out["new"], OTHER_ID);

    let n = app.calls().len();
    for (tool, args) in [
        ("pd_supersede", json!({"old": ID})),
        ("pd_supersede", json!({"old": ID, "new": ID})),
        ("pd_vote", json!({"id": ID})),
        ("pd_vote", json!({"id": ID, "value": 2})),
        ("pd_vote", json!({"id": ID, "value": 1, "kind": "like"})),
        ("pd_vote", json!({"value": 1})),
        ("pd_verdicts", json!({})),
        ("pd_verdicts", json!({"ids": []})),
        ("pd_verdicts", json!({"ids": [1, 2]})),
    ] {
        let e = format!("{:#}", s.call(tool, &args).unwrap_err());
        assert_eq!(error_kind(&e), Some("invalid_input"), "{tool} {args}: {e}");
    }
    assert_eq!(app.calls().len(), n, "nothing refused here reaches the app");

    // The app's own refusal keeps its kind (an evaluation the server does not have).
    app.reply(Err(CallError::Command {
        kind: "rejected".into(),
        message: "server returned 404: evaluation not found".into(),
    }));
    let e = format!("{:#}", s.call("pd_vote", &json!({"id": ID, "value": 1})).unwrap_err());
    assert_eq!(error_kind(&e), Some("rejected"));
}

#[test]
fn defaults_and_refusals() {
    let (mut s, app) = server();
    app.reply(Ok(
        json!({"status": "saved", "id": ID, "root": ID, "ref": null, "from": "0x00000000000000000000000000000000000000ab"}),
    ));
    s.call(
        "pd_enqueue_node",
        &json!({"text": "SurfaceError: Outdated", "cc": "store.steampowered.com"}),
    )
    .unwrap();
    let args = &app.calls()[0].1;
    assert_eq!(
        args["bcc"],
        content_address("SurfaceError: Outdated").unwrap(),
        "bcc defaults to the text's address"
    );
    assert_eq!(args["to"], Value::Null);

    app.reply(Ok(json!({"status": "blocked"})));
    s.call("pd_enqueue_node", &json!({"text": "x", "cc": "a.com", "bcc": "none"}))
        .unwrap();
    assert_eq!(app.calls()[1].1["bcc"], Value::Null);

    // Obvious profanity is blocked by the dictionary in process: the app is not asked (nor started).
    let before = app.calls().len();
    let blocked = s.call("pd_enqueue_node", &json!({"text": "이 시발 패치 뭐야", "cc": "a.com"})).unwrap();
    assert_eq!((blocked["status"].as_str(), blocked["filter"].as_str()), (Some("blocked"), Some("dictionary")));
    assert_eq!(blocked["cc"], "a.com");
    assert_eq!(app.calls().len(), before);

    let n = app.calls().len();
    for (tool, args) in [
        ("pd_enqueue_node", json!({"text": "x", "cc": "app://"})),
        ("pd_enqueue_node", json!({"text": "x", "cc": "localhost"})),
        (
            "pd_enqueue_node",
            json!({"text": "x", "cc": "a.com", "ref": ID, "parent_local": "local-1"}),
        ),
        ("pd_enqueue_node", json!({"text": "x", "cc": "a.com", "to": "0x12"})),
        ("pd_enqueue_node", json!({"cc": "a.com"})),
        ("pd_graph_inspect", json!({"node_id": "nope"})),
        ("pd_auth_session", json!({"action": "hack"})),
        ("pd_model_manage", json!({"action": "download", "variant": "huge"})),
    ] {
        let e = format!("{:#}", s.call(tool, &args).unwrap_err());
        assert_eq!(error_kind(&e), Some("invalid_input"), "{tool} {args}: {e}");
    }
    assert_eq!(app.calls().len(), n, "nothing refused here reaches the app");

    app.reply(Err(CallError::Command {
        kind: "not_ready".into(),
        message: "model missing".into(),
    }));
    let e = format!("{:#}", s.call("pd_search_merged", &json!({"query": "q"})).unwrap_err());
    assert_eq!(e, "not_ready: model missing");
    app.reply(Err(CallError::Transport("spawning premium-discount-app: not found".into())));
    let e = format!("{:#}", s.call("pd_flush_and_sync", &json!({})).unwrap_err());
    assert_eq!(error_kind(&e), Some("unavailable"));
}

#[test]
fn session_and_model_actions() {
    let (mut s, app) = server();
    for (tool, action, cmd) in [
        ("pd_auth_session", "status", "cmd_session"),
        ("pd_auth_session", "start", "cmd_login_start"),
        ("pd_auth_session", "poll", "cmd_login_status"),
        ("pd_auth_session", "logout", "cmd_logout"),
        ("pd_model_manage", "download", "cmd_model_download"),
        ("pd_model_manage", "cancel", "cmd_model_download_cancel"),
        ("pd_model_manage", "status", "cmd_model_status"),
    ] {
        app.reply(Ok(json!({"state": "x"})));
        s.call(tool, &json!({"action": action, "verify": true})).unwrap();
        assert_eq!(app.calls().last().unwrap().0, cmd);
    }
    let calls = app.calls();
    assert_eq!(calls[0].1, json!({"verify": true}));
    assert_eq!(calls[4].1, json!({"variant": "text"}));
    let (mut s, app) = server();
    app.reply(Ok(json!({"installed": "none"})));
    let st = s.call("pd_model_manage", &json!({"action": "status"})).unwrap();
    assert_eq!(st["installed"], "none");
    assert_eq!(st["app"], Value::Null, "the scripted backend has no process");
}

// Scenario 2 Part 3 (PF-07, PF-09): prewarm and evict go to the backend without an app command; under 650 MB of free
// memory the pre-warm is deferred and nothing is spawned.
#[test]
fn prewarm_and_evict_actions() {
    let (mut s, app) = server();
    let out = s.call("pd_model_manage", &json!({"action": "prewarm", "avail_ram_mb": 2048})).unwrap();
    assert_eq!(out["outcome"], "already_warm", "the scripted backend has nothing to warm");
    let low = s.call("pd_model_manage", &json!({"action": "prewarm", "avail_ram_mb": 480})).unwrap();
    assert_eq!(low["outcome"], "deferred");
    assert!(low["reason"].as_str().unwrap().contains("480 MB"));
    let ev = s.call("pd_model_manage", &json!({"action": "evict"})).unwrap();
    assert_eq!(ev["stopped"], false);
    assert!(app.calls().is_empty(), "no app command for prewarm / evict");
    let e = format!("{:#}", s.call("pd_model_manage", &json!({"action": "boil"})).unwrap_err());
    assert_eq!(error_kind(&e), Some("invalid_input"));
}

// FC-12: a sync without flush still reports the queued votes, so a vote queued offline keeps the host's timer on;
// 401 turns has_work off (the host pauses instead of hammering the server).
#[test]
fn sync_reports_queued_votes_and_auth_holds() {
    let (mut s, app) = server();
    app.reply(Ok(json!([])));
    app.reply(Ok(json!({"queued": 0, "awaiting_result": 0, "queued_votes": 2})));
    let out = s.call("pd_flush_and_sync", &json!({"since_ms": 0, "flush": false})).unwrap();
    assert_eq!(out["queued_votes"], 2);
    assert_eq!(app.calls()[1].0, "cmd_queue_counts");

    let (mut s, app) = server();
    app.reply(Ok(json!({"sent": 0, "remaining": 1, "still_pending": 0, "votes_remaining": 1,
        "stopped": "auth_required: server returned 401", "auth_required": true})));
    app.reply(Ok(json!([])));
    let out = s.call("pd_flush_and_sync", &json!({"since_ms": 0})).unwrap();
    assert_eq!(out["has_work"], false, "auth hold: no timer runs");
    assert_eq!(out["queued_votes"], 1);
    assert_eq!(app.calls().len(), 2, "with flush the summary already has the vote count");
}

// WA-11 (MCP side): pd_analytic is the 11th tool; report runs a short sync first when the host is stale and still
// answers from local events when that sync fails; forget takes ids and/or a time cut-off.
#[test]
fn wa11_pd_analytic_is_the_eleventh_tool_and_maps_its_actions() {
    assert_eq!(TOOLS.len(), 11);
    assert_eq!(TOOLS[10], "pd_analytic");
    let (mut s, app) = server();
    // First report: stale → cmd_analytic_sync (2 pages) then cmd_analytic_report.
    app.reply(Ok(json!({"host": "shop.example.com", "pages": 1})));
    app.reply(Ok(json!({"host": "shop.example.com", "confidence": {"kind": "insufficient_sample", "sessions": 4, "min_required": 15}})));
    let out = s
        .call("pd_analytic", &json!({"action": "report", "host": "https://Shop.Example.com/cart", "query": "이번 주 vs 지난주 전환율"}))
        .unwrap();
    let calls = app.calls();
    assert_eq!(calls[0], ("cmd_analytic_sync".to_string(), json!({"host": "shop.example.com", "max_pages": 2})));
    assert_eq!(calls[1].0, "cmd_analytic_report");
    assert_eq!(calls[1].1["query"], "이번 주 vs 지난주 전환율");
    assert_eq!(out["confidence"]["kind"], "insufficient_sample");
    assert!(out.get("sync_error").is_none());
    // Within 5 minutes: no second sync.
    app.reply(Ok(json!({"host": "shop.example.com"})));
    s.call("pd_analytic", &json!({"action": "report", "host": "shop.example.com", "query": "q"})).unwrap();
    assert_eq!(app.calls()[2].0, "cmd_analytic_report");

    // A failed sync does not fail the report.
    let (mut s, app) = server();
    app.reply(Err(CallError::Command { kind: "offline".into(), message: "no network".into() }));
    app.reply(Ok(json!({"host": "shop.example.com"})));
    let out = s.call("pd_analytic", &json!({"action": "report", "host": "shop.example.com", "query": "q"})).unwrap();
    assert!(out["sync_error"].as_str().unwrap().contains("offline"));

    // forget: ids and the time cut-off travel together; neither is an argument error.
    let (mut s, app) = server();
    app.reply(Ok(json!({"forgotten": 3})));
    s.call("pd_analytic", &json!({"action": "forget", "host": "shop.example.com", "before_ms": 1_700_000_000_000i64, "ids": ["e1"]})).unwrap();
    assert_eq!(app.calls()[0].1, json!({"host": "shop.example.com", "ids": ["e1"], "before_ms": 1_700_000_000_000i64}));
    for bad in [json!({"action": "forget", "host": "shop.example.com"}), json!({"action": "report", "host": "app://Game.exe", "query": "q"})] {
        let e = format!("{:#}", s.call("pd_analytic", &bad).unwrap_err());
        assert_eq!(error_kind(&e), Some("invalid_input"), "{bad}");
    }
}

// Scenario 3 Part 2 (CR-04): the scope boost travels to cmd_search_merged; keys are hashed like `to_key`.
#[test]
fn search_passes_the_scope_boost() {
    let (mut s, app) = server();
    app.reply(Ok(json!({"matches": [], "remote_error": null})));
    s.call(
        "pd_search_merged",
        &json!({"query": "출혈 쌍곡도", "cc": "app://EldenRing.exe", "l1_checkpoint_key": "boss: 말레니아", "l2_entity_keys": ["출혈 쌍곡도"]}),
    )
    .unwrap();
    let args = &app.calls()[0].1;
    assert_eq!(args["scope"]["l1_checkpoint_to"], hash_id("boss: 말레니아").unwrap());
    assert_eq!(args["scope"]["l2_entity_tos"], json!([hash_id("출혈 쌍곡도").unwrap()]));
    // No scope arguments: the request is exactly as before.
    let (mut s, app) = server();
    app.reply(Ok(json!({"matches": [], "remote_error": null})));
    s.call("pd_search_merged", &json!({"query": "q"})).unwrap();
    assert!(app.calls()[0].1.get("scope").is_none());
}

/// One-shot HTTP server answering `GET /graph…`.
fn graph_server(body: Value) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let body = body.to_string();
    let h = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut r = BufReader::new(sock.try_clone().unwrap());
        let mut first = String::new();
        r.read_line(&mut first).unwrap();
        loop {
            let mut l = String::new();
            if r.read_line(&mut l).unwrap() == 0 || l == "\r\n" {
                break;
            }
        }
        write!(
            sock,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        first
    });
    (base, h)
}

#[test]
fn the_binary_speaks_mcp_without_premium_discount_app() {
    let home = std::env::temp_dir().join(format!("premium-discount-mcp-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let graph = json!({
        "nodes": [
            {"id": ID, "type": "evaluation", "label": "평가", "cc": "code.exe", "root": ID, "tier": "premium", "grade": 1},
            {"id": "author:0123456789ab", "type": "author", "label": "작성자 012345"},
        ],
        "edges": [{"source": "author:0123456789ab", "target": ID, "type": "authored"}],
        "summary": {"premium": 1, "neutral": 0, "discount": 0, "unrated": 0},
    });
    let (base, http) = graph_server(graph);
    let args: Vec<String> = [
        "--pd-app",
        "definitely-not-premium-discount-app-xyz",
        "--home",
        home.to_str().unwrap(),
        "--server-url",
        &base,
        "--idle-secs",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut c = McpClient::spawn(Path::new(env!("CARGO_BIN_EXE_premium-discount-mcp")), &args).unwrap();
    assert_eq!(c.server_name(), "premium-discount-mcp");
    assert_eq!(c.list_tools().unwrap(), TOOLS.to_vec());

    // No premium-discount-app: a tool error with kind `unavailable`, and the server keeps running.
    let e = format!(
        "{:#}",
        c.call_tool("pd_vote", json!({"id": ID, "value": 1}), Duration::from_secs(30))
            .unwrap_err()
    );
    assert!(e.starts_with("pd_vote: unavailable: spawning"), "{e}");
    let e = format!(
        "{:#}",
        c.call_tool("pd_enqueue_node", json!({"text": "x", "cc": "app://"}), Duration::from_secs(30))
            .unwrap_err()
    );
    assert!(e.contains("invalid_input"), "{e}");

    // The graph does not need the app.
    let g = result_json(
        &c.call_tool("pd_graph_inspect", json!({"cc": "app://Code.exe/coding"}), Duration::from_secs(30))
            .unwrap(),
    );
    assert_eq!(g["path"], "/graph?cc=code.exe");
    assert_eq!(g["summary"]["authors"], 1);
    assert_eq!(g["summary"]["tiers"]["premium"], 1);
    assert_eq!(g["summary"]["threads"][0]["root"], ID);
    assert!(http.join().unwrap().starts_with("GET /graph?cc=code.exe "));

    // Neither does numbering (pd-nav in process).
    let n = result_json(&c.call_tool("pd_nav_number", nav_screen(false), Duration::from_secs(30)).unwrap());
    assert_eq!(n["regions"][0]["number"], 1);
    drop(c);
    let _ = std::fs::remove_dir_all(&home);
}

fn region(anchor: &str, role: &str, rect: [f32; 4], source: &str, sensitive: bool) -> Value {
    json!({"anchor": anchor, "role": role, "name": anchor, "rect": {"x": rect[0], "y": rect[1], "w": rect[2], "h": rect[3]},
           "source": source, "score": 0.9, "sensitive": sensitive})
}

/// A login form: a tree button, a vision box over the same button, a link, a vision-only icon, an event over the
/// link, and a box entirely outside the window.
fn nav_screen(sensitive: bool) -> Value {
    json!({
        "regions": [
            region("text:intro", "text", [0.1, 0.05, 0.8, 0.1], "tree", false),
            region("btn:login", "button", [0.6, 0.5, 0.2, 0.08], "tree", false),
            region("vis:btn", "button", [0.61, 0.5, 0.2, 0.08], "vision", false),
            region("link:help", "link", [0.1, 0.8, 0.2, 0.05], "tree", false),
            region("evt:help", "other", [0.1, 0.8, 0.2, 0.05], "event", false),
            region("vis:icon", "image", [0.9, 0.9, 0.05, 0.05], "vision", false),
            region("pw", "input", [0.1, 0.5, 0.3, 0.08], "tree", sensitive),
            region("gone", "button", [1.5, 1.5, 0.1, 0.1], "tree", false),
        ],
        "tiers": {"link:help": "premium", "btn:login": "discount"},
    })
}

#[test]
fn nav_number_uses_premium_discounts_rules() {
    let n = premium_discount_mcp::server::nav_number(&nav_screen(false)).unwrap();
    assert_eq!(n["sensitive"], false);
    assert_eq!(n["record"], true);
    assert_eq!(n["dropped"], 1, "the box outside the window");
    let order: Vec<(&str, i64)> = n["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["region"]["anchor"].as_str().unwrap(), r["number"].as_i64().unwrap()))
        .collect();
    // The vision box over the button and the event over the link merge into the tree regions; the link is
    // premium (first, marked changed), the button discount (last); in between: input, text, image by role.
    assert_eq!(order, vec![("link:help", 1), ("pw", 2), ("text:intro", 3), ("vis:icon", 4), ("btn:login", 5)]);
    assert_eq!(n["regions"][0]["tier"], "premium");
    assert_eq!(n["regions"][0]["region"]["changed"], true);
    assert_eq!(n["regions"][4]["tier"], "discount");

    // A password field makes the screen sensitive: tree candidates only, and nothing is recorded.
    let n = premium_discount_mcp::server::nav_number(&nav_screen(true)).unwrap();
    assert_eq!((n["sensitive"].as_bool(), n["record"].as_bool()), (Some(true), Some(false)));
    let anchors: Vec<&str> = n["regions"].as_array().unwrap().iter().map(|r| r["region"]["anchor"].as_str().unwrap()).collect();
    assert!(!anchors.contains(&"vis:icon"), "{anchors:?}");
    assert_eq!(anchors.len(), 4);

    let bad = premium_discount_mcp::server::nav_number(&json!({"regions": [], "tiers": {"a": "great"}}));
    assert!(bad.is_err());
    assert!(premium_discount_mcp::server::nav_number(&json!({"regions": [], "iou": 2})).is_err());
}

// Scenario 1 Part 2 through the tool (CN-04 / CN-12): with a frame context the numbers stay put across calls even when
// a tier change would reorder them, and a locked number is not given away.
#[test]
fn nav_number_with_a_frame_context_keeps_numbers_across_calls() {
    let (mut s, _app) = server();
    let mut args = nav_screen(false);
    args["screen_key"] = json!("0xscreen");
    args["now_ms"] = json!(1_000);
    let first = s.call("pd_nav_number", &args).unwrap();
    let numbers = |v: &Value| -> Vec<(String, i64)> {
        v["regions"].as_array().unwrap().iter().map(|r| (r["region"]["anchor"].as_str().unwrap().to_string(), r["number"].as_i64().unwrap())).collect()
    };
    let before = numbers(&first);
    assert_eq!(first["session"], "default");
    assert!(first["regions"][0].get("origin").is_some(), "sticky numbers say where they came from");
    // The button turns premium: the stateless order would move it to 1; the sticky numbers stay.
    args["tiers"]["btn:login"] = json!("premium");
    args["now_ms"] = json!(1_200);
    args["locked_numbers"] = json!(1 << 2);
    let second = s.call("pd_nav_number", &args).unwrap();
    let mut a = before.clone();
    let mut b = numbers(&second);
    a.sort();
    b.sort();
    assert_eq!(a, b, "no renumbering on a tier change");
    let stateless = premium_discount_mcp::server::nav_number(&args).unwrap();
    assert_ne!(numbers(&stateless), numbers(&second), "the stateless call would have reordered");
    // Another session is independent.
    args["session"] = json!("second-window");
    assert_eq!(s.call("pd_nav_number", &args).unwrap()["session"], "second-window");
}

// Scenario 6 Part 1 through the tool: window-level signals lock the screen with no sensitive region, a toast only
// drops the candidates under it.
#[test]
fn nav_number_applies_the_privacy_shield() {
    let mut args = nav_screen(false);
    args["os_capture_protected"] = json!(true);
    let n = premium_discount_mcp::server::nav_number(&args).unwrap();
    assert_eq!((n["sensitive"].as_bool(), n["record"].as_bool()), (Some(true), Some(false)));
    assert_eq!(n["shield"]["full_lockdown"], "display_affinity_protected");
    let anchors: Vec<&str> = n["regions"].as_array().unwrap().iter().map(|r| r["region"]["anchor"].as_str().unwrap()).collect();
    assert!(!anchors.contains(&"vis:icon"), "tree candidates only: {anchors:?}");

    let mut args = nav_screen(false);
    args["os_notification_zones"] = json!([{"x": 0.85, "y": 0.85, "w": 0.15, "h": 0.15}]);
    let n = premium_discount_mcp::server::nav_number(&args).unwrap();
    assert_eq!((n["sensitive"].as_bool(), n["record"].as_bool()), (Some(false), Some(true)));
    let anchors: Vec<&str> = n["regions"].as_array().unwrap().iter().map(|r| r["region"]["anchor"].as_str().unwrap()).collect();
    assert!(!anchors.contains(&"vis:icon"), "the icon under the toast is dropped: {anchors:?}");
    assert!(anchors.contains(&"link:help"));
    assert!(n["shield"]["partial_redaction"]["zones"].is_array());
    // No privacy arguments: clear, exactly as before.
    assert_eq!(premium_discount_mcp::server::nav_number(&nav_screen(false)).unwrap()["shield"], "clear");
}

#[test]
fn nav_number_hides_only_an_inline_sensitive_cell() {
    // a sensitive field locks the screen …
    let n = premium_discount_mcp::server::nav_number(&nav_screen(true)).unwrap();
    assert_eq!((n["sensitive"].as_bool(), n["record"].as_bool()), (Some(true), Some(false)));
    // … unless the host marks it as one sensitive cell: only that spot is hidden and the rest is recorded
    let mut args = nav_screen(true);
    args["inline_redaction_zones"] = json!([{"x": 0.09, "y": 0.49, "w": 0.32, "h": 0.10, "kind": "sensitive_document_cell"}]);
    let n = premium_discount_mcp::server::nav_number(&args).unwrap();
    assert_eq!((n["sensitive"].as_bool(), n["record"].as_bool()), (Some(false), Some(true)), "{n}");
    let anchors: Vec<&str> = n["regions"].as_array().unwrap().iter().map(|r| r["region"]["anchor"].as_str().unwrap()).collect();
    assert!(!anchors.contains(&"pw"), "{anchors:?}");
    assert!(anchors.contains(&"link:help") && anchors.contains(&"btn:login"), "{anchors:?}");
    assert!(n["shield"]["partial_redaction"]["zones"].is_array());
    // a window-level signal still locks everything
    args["os_capture_protected"] = json!(true);
    assert_eq!(premium_discount_mcp::server::nav_number(&args).unwrap()["shield"]["full_lockdown"], "display_affinity_protected");
}
