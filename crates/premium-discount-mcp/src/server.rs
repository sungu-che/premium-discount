//! The MCP tools: premium-discount-app's 16 line commands regrouped for Raon Zena.
//!
//! | tool | premium-discount-app command(s) |
//! |---|---|
//! | `pd_enqueue_node` | `cmd_submit_evaluation` (local filter → embedding → LanceDB row → send) |
//! | `pd_flush_and_sync` | `cmd_flush_outbox` (evaluations and votes) + `cmd_sync_cache` |
//! | `pd_search_merged` | `cmd_search_merged` (with the server's verdict and my vote on each match) |
//! | `pd_vote` | `cmd_vote` |
//! | `pd_verdicts` | `cmd_verdicts` |
//! | `pd_graph_inspect` | none: `GET /graph`, `/graph/local`, `/graph/similar` on the server |
//! | `pd_auth_session` | `cmd_session`, `cmd_login_start`, `cmd_login_status`, `cmd_logout` |
//! | `pd_model_manage` | `cmd_model_status`, `cmd_model_download`, `cmd_model_download_cancel` |
//! | `pd_nav_number` | none: premium-discount's `pd-nav` in process (candidate merge, 1 ~ 9 numbering, sensitive screens) |
//! | `pd_analytic` | `cmd_analytic_site`, `cmd_analytic_sync`, `cmd_analytic_report` (a short sync first when stale), `cmd_analytic_forget` |
//!
//! The schema is premium-discount's: argument and result field names are its own, and recommendations
//! (`premium` / `discount`) are the server's verdicts (rule: premium-discount `contracts/verdict_v1.json`).
//! This layer only adds what Raon Zena's places need: `cc` from `app://exe/scene` and URLs
//! ([`crate::envelope`]), `to` / `bcc` from keys with the web client's `hashId` ([`crate::address`]), and the graph.
//! Tool errors read `<kind>: <message>`, with premium-discount-app's error kinds (`offline`, `not_ready`,
//! `invalid_input`, `rejected`, `queue_full` …) and `unavailable` when the app cannot be started or reached.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use premium_discount_mcp_protocol::ToolHandler;

use premium_discount_schema::address::{content_address, hash_id, normalize_address};
use crate::bridge::Backend;
use premium_discount_schema::envelope::{normalize_cc, Cc};
use crate::graph::{query_of, summarize, GraphClient};
use crate::lines::CallError;

pub const SERVER_NAME: &str = "premium-discount-mcp";

pub const TOOLS: [&str; 11] = [
    "pd_enqueue_node",
    "pd_flush_and_sync",
    "pd_search_merged",
    "pd_vote",
    "pd_supersede",
    "pd_verdicts",
    "pd_graph_inspect",
    "pd_auth_session",
    "pd_nav_number",
    "pd_model_manage",
    "pd_analytic",
];

/// `pd_analytic report` first runs a short sync (at most this many pages) when the host's last sync is older than
/// [`ANALYTIC_STALE`], so the report covers the latest events.
pub const ANALYTIC_AUTO_SYNC_PAGES: u32 = 2;
pub const ANALYTIC_STALE: std::time::Duration = std::time::Duration::from_secs(300);

/// Error kinds a tool error starts with: premium-discount-app's `CommandError.kind` plus `unavailable`
/// (premium-discount-schema's contract).
pub use premium_discount_schema::contract::ERROR_KINDS;

/// The kind of a tool error message (`<kind>: <message>`).
pub fn error_kind(message: &str) -> Option<&str> {
    let (kind, _) = message.split_once(": ")?;
    ERROR_KINDS.contains(&kind).then_some(kind)
}

fn has_kind(message: &str) -> bool {
    error_kind(message).is_some()
}

/// Search tier filters (premium-discount `TIER_FILTERS`).
pub const TIER_FILTERS: [&str; 3] = ["premium", "discount", "rated"];

pub struct PdSyncServer<B: Backend> {
    backend: B,
    graph: GraphClient,
    /// premium-discount-app's dictionary filter, run before the app is asked (or started)
    dictionary: premium_discount_filter::LocalFilter,
    /// When each analytics host was last synced through this server (`pd_analytic`).
    analytic_synced: std::collections::HashMap<String, std::time::Instant>,
    /// `pd_nav_number` with a frame context: one sticky numberer per session (window), see [`StickySession`].
    sticky: std::collections::HashMap<String, StickySession>,
}

/// How many `pd_nav_number` sessions are kept (a host normally has one per watched window).
pub const MAX_NAV_SESSIONS: usize = 16;

/// The sticky numbering state of one `pd_nav_number` session (Scenario 1 Part 2).
#[derive(Default)]
struct StickySession {
    numberer: premium_discount_nav::StickyNumberer,
    /// The last call was a full privacy lockdown: entering one resets the numbers (no vision / event slot survives).
    locked: bool,
}

/// The analytics host of a `host` / `cc` argument: web domains and URLs only (desktop programs have no web events).
fn analytic_host(args: &Value) -> Result<String> {
    let raw = opt_str(args, "host").or_else(|| opt_str(args, "cc")).context("missing `host` (shop.example.com or a URL)")?;
    let cc = normalize_cc(raw)?;
    if cc.kind == premium_discount_schema::envelope::CcKind::App {
        bail!("pd_analytic needs a web site (got the desktop program {})", cc.cc);
    }
    Ok(cc.cc)
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(args, key).with_context(|| format!("missing `{key}`"))
}

fn call(backend: &mut impl Backend, cmd: &str, args: Value) -> Result<Value> {
    backend.call(cmd, args).map_err(|e: CallError| anyhow!("{e}"))
}

/// `address` given directly, else `hash_id(key)`.
fn address_or_key(args: &Value, field: &str, key_field: &str) -> Result<Option<String>> {
    match (opt_str(args, field), opt_str(args, key_field)) {
        (Some(_), Some(_)) => bail!("give `{field}` or `{key_field}`, not both"),
        (Some(a), None) => Ok(Some(normalize_address(field, a)?)),
        (None, Some(k)) => Ok(Some(hash_id(k)?)),
        (None, None) => Ok(None),
    }
}

fn cc_fields(cc: &Cc) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("cc".into(), Value::from(cc.cc.clone()));
    m.insert("cc_kind".into(), Value::from(cc.kind.name()));
    if let Some(s) = &cc.scene {
        m.insert("scene".into(), Value::from(s.clone()));
    }
    m
}

fn merge(mut base: Value, extra: Map<String, Value>) -> Value {
    if let Some(obj) = base.as_object_mut() {
        obj.extend(extra);
    }
    base
}

impl<B: Backend> PdSyncServer<B> {
    pub fn new(backend: B, server_url: &str) -> PdSyncServer<B> {
        PdSyncServer {
            backend,
            graph: GraphClient::new(server_url),
            dictionary: premium_discount_filter::LocalFilter::default(),
            analytic_synced: std::collections::HashMap::new(),
            sticky: std::collections::HashMap::new(),
        }
    }

    /// `pd_analytic` (Scenario 5 Part 4): the site owner's web analytics. `report` runs a short sync first when the
    /// host was not synced in the last 5 minutes; when that sync fails (offline, not signed in) the report still comes
    /// from the local events, with `sync_error`.
    fn analytic(&mut self, args: &Value) -> Result<Value> {
        let action = req_str(args, "action")?;
        let host = analytic_host(args)?;
        match action {
            "site" => call(&mut self.backend, "cmd_analytic_site", json!({"host": host})),
            "sync" => {
                let mut a = json!({"host": host});
                if let Some(p) = args.get("max_pages").and_then(Value::as_u64) {
                    a["max_pages"] = json!(p);
                }
                let out = call(&mut self.backend, "cmd_analytic_sync", a)?;
                self.analytic_synced.insert(host, std::time::Instant::now());
                Ok(out)
            }
            "report" => {
                let query = req_str(args, "query")?;
                let stale = self.analytic_synced.get(&host).is_none_or(|t| t.elapsed() >= ANALYTIC_STALE);
                let mut sync_error = None;
                if stale && args.get("sync").and_then(Value::as_bool) != Some(false) {
                    match call(&mut self.backend, "cmd_analytic_sync", json!({"host": host, "max_pages": ANALYTIC_AUTO_SYNC_PAGES})) {
                        Ok(_) => {
                            self.analytic_synced.insert(host.clone(), std::time::Instant::now());
                        }
                        Err(e) => sync_error = Some(format!("{e:#}")),
                    }
                }
                let mut a = json!({"host": host, "query": query});
                if let Some(lang) = opt_str(args, "lang") {
                    a["lang"] = json!(lang);
                }
                if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
                    a["top_k"] = json!(k);
                }
                let mut out = call(&mut self.backend, "cmd_analytic_report", a)?;
                if let (Some(obj), Some(e)) = (out.as_object_mut(), sync_error) {
                    obj.insert("sync_error".into(), json!(e));
                }
                Ok(out)
            }
            "forget" => {
                // Two forms, together or apart: event ids (the desktop UI) and everything of the host up to before_ms.
                let ids: Vec<String> = args
                    .get("ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).context("`ids` must hold strings"))
                    .collect::<Result<_>>()?;
                let before_ms = args.get("before_ms").and_then(Value::as_i64);
                if ids.is_empty() && before_ms.is_none() {
                    bail!("forget needs `ids` or `before_ms`");
                }
                let mut a = json!({"host": host, "ids": ids});
                if let Some(b) = before_ms {
                    a["before_ms"] = json!(b);
                }
                if let Some(f) = args.get("flush_remote").and_then(Value::as_bool) {
                    a["flush_remote"] = json!(f);
                }
                call(&mut self.backend, "cmd_analytic_forget", a)
            }
            other => bail!("action must be site, sync, report or forget (got {other})"),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn enqueue(&mut self, args: &Value) -> Result<Value> {
        let text = req_str(args, "text")?;
        let cc = normalize_cc(req_str(args, "cc")?)?;
        let to = address_or_key(args, "to", "to_key")?;
        let bcc = match (opt_str(args, "bcc"), opt_str(args, "bcc_key")) {
            (Some("none"), None) => None,
            (Some(_), Some(_)) => bail!("give `bcc` or `bcc_key`, not both"),
            (Some(a), None) => Some(normalize_address("bcc", a)?),
            (None, Some(k)) => Some(content_address(k)?),
            (None, None) => Some(content_address(text)?),
        };
        let from = opt_str(args, "from").map(|a| normalize_address("from", a)).transpose()?;
        let (ref_, parent) = (opt_str(args, "ref"), opt_str(args, "parent_local"));
        if ref_.is_some() && parent.is_some() {
            bail!("invalid_input: give `ref` (parent on the server) or `parent_local` (parent in the outbox), not both");
        }
        // The app's own dictionary (premium-discount-filter), in process: obvious profanity is answered here without
        // starting premium-discount-app (its model filter, laya, may still block what this lets through).
        if self.dictionary.judge(text) == premium_discount_filter::Verdict::Profane {
            let mut out = cc_fields(&cc);
            out.insert("status".into(), json!("blocked"));
            out.insert("filter".into(), json!("dictionary"));
            return Ok(Value::Object(out));
        }
        let result = call(
            &mut self.backend,
            "cmd_submit_evaluation",
            json!({"text": text, "cc": cc.cc, "from": from, "to": to, "bcc": bcc, "ref": ref_, "local_parent": parent}),
        )?;
        let mut extra = cc_fields(&cc);
        extra.insert("to".into(), json!(to));
        extra.insert("bcc".into(), json!(bcc));
        Ok(merge(result, extra))
    }

    fn flush_and_sync(&mut self, args: &Value) -> Result<Value> {
        let since = args.get("since_ms").and_then(Value::as_i64).unwrap_or(0).max(0);
        let flush = args.get("flush").and_then(Value::as_bool).unwrap_or(true);
        let summary = if flush {
            Some(call(&mut self.backend, "cmd_flush_outbox", Value::Null)?)
        } else {
            None
        };
        let rows = match call(&mut self.backend, "cmd_sync_cache", json!({"since": since}))? {
            Value::Array(r) => r,
            other => bail!("bad_response: cmd_sync_cache returned {other}"),
        };
        let next = rows
            .iter()
            .filter_map(|r| r.get("updated_at").and_then(Value::as_i64))
            .max()
            .unwrap_or(since)
            .max(since);
        let mut counts: Map<String, Value> = Map::new();
        for r in &rows {
            let state = r.get("state").and_then(Value::as_str).unwrap_or("unknown").to_string();
            let n = counts.get(&state).and_then(Value::as_u64).unwrap_or(0) + 1;
            counts.insert(state, Value::from(n));
        }
        let has_work = summary.as_ref().map(|s| {
            let n = |k: &str| s.get(k).and_then(Value::as_u64).unwrap_or(0);
            let auth_required = s.get("auth_required").and_then(Value::as_bool).unwrap_or(false);
            n("remaining") + n("still_pending") + n("votes_remaining") > 0 && !auth_required
        });
        // Votes waiting to go out, also on a run without flush (FC-12): a vote queued offline while no evaluation
        // waits must still keep the host's SyncLoop timer on. An older app without the command just omits it.
        let queued_votes = match &summary {
            Some(s) => s.get("votes_remaining").and_then(Value::as_u64),
            None => call(&mut self.backend, "cmd_queue_counts", Value::Null)
                .ok()
                .and_then(|c| c.get("queued_votes").and_then(Value::as_u64)),
        };
        Ok(json!({"flush": summary, "rows": rows, "next_since_ms": next, "counts": counts, "has_work": has_work, "queued_votes": queued_votes}))
    }

    fn search(&mut self, args: &Value) -> Result<Value> {
        let query = req_str(args, "query")?;
        let cc = opt_str(args, "cc").map(normalize_cc).transpose()?;
        let to = address_or_key(args, "to", "to_key")?;
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(10);
        let tier = opt_str(args, "tier");
        if let Some(t) = tier {
            if !TIER_FILTERS.contains(&t) {
                bail!("tier must be premium, discount or rated (got {t})");
            }
        }
        // Scenario 3 Part 2 scope boost: rank (not filter) matches on the current checkpoint's target (L1, +0.12) and
        // the entities named in the question (L2, +0.06) after one Role::Query embedding.
        let l1 = address_or_key(args, "l1_checkpoint_to", "l1_checkpoint_key")?;
        let mut l2: Vec<String> = Vec::new();
        for (field, hashed) in [("l2_entity_tos", false), ("l2_entity_keys", true)] {
            for v in args.get(field).and_then(Value::as_array).into_iter().flatten() {
                let s = v.as_str().map(str::trim).filter(|s| !s.is_empty()).with_context(|| format!("`{field}` must hold strings"))?;
                l2.push(if hashed { hash_id(s)? } else { normalize_address(field, s)? });
            }
        }
        let mut request = json!({
            "query": query,
            "top_k": top_k,
            "cc": cc.as_ref().map(|c| c.cc.clone()),
            "to": to,
            "root": opt_str(args, "root"),
            "ref": opt_str(args, "ref"),
            "tier": tier,
        });
        if l1.is_some() || !l2.is_empty() {
            request["scope"] = json!({"l1_checkpoint_to": l1, "l2_entity_tos": l2});
        }
        let found = call(&mut self.backend, "cmd_search_merged", request)?;
        Ok(merge(found, cc.as_ref().map(cc_fields).unwrap_or_default()))
    }

    fn vote(&mut self, args: &Value) -> Result<Value> {
        let id = req_str(args, "id")?;
        let value = args.get("value").and_then(Value::as_i64).context("missing `value` (1, -1 or 0)")?;
        if !matches!(value, -1..=1) {
            bail!("value must be 1, -1 or 0 (got {value})");
        }
        let kind = opt_str(args, "kind").unwrap_or("vote");
        if !matches!(kind, "vote" | "vote_outcome") {
            bail!("kind must be vote or vote_outcome (got {kind})");
        }
        call(&mut self.backend, "cmd_vote", json!({"id": id, "value": value, "kind": kind}))
    }

    /// 신구 대체 보고: premium-discount-relay 가 `new` 가 `old` 를 대체한다고 판정했다 (Raon Zena 의 판정 보조)
    fn supersede(&mut self, args: &Value) -> Result<Value> {
        let old = req_str(args, "old")?;
        let new = req_str(args, "new")?;
        if old == new {
            bail!("old and new must be different evaluations");
        }
        call(&mut self.backend, "cmd_supersede", json!({"old": old, "new": new}))
    }

    fn verdicts(&mut self, args: &Value) -> Result<Value> {
        let ids: Vec<&str> = args
            .get("ids")
            .and_then(Value::as_array)
            .context("missing `ids` (an array of evaluation ids)")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .context("`ids` must hold strings")
            })
            .collect::<Result<_>>()?;
        if ids.is_empty() {
            bail!("`ids` is empty");
        }
        let refresh = args.get("refresh").and_then(Value::as_bool).unwrap_or(false);
        call(&mut self.backend, "cmd_verdicts", json!({"ids": ids, "refresh": refresh}))
    }

    fn graph(&mut self, args: &Value) -> Result<Value> {
        let cc = opt_str(args, "cc").map(normalize_cc).transpose()?;
        let q = query_of(
            cc.as_ref().map(|c| c.cc.clone()),
            opt_str(args, "node_id").map(str::to_string),
            opt_str(args, "mode"),
            args.get("top_k").and_then(Value::as_u64),
        )?;
        let graph = match args.get("timeout_ms").and_then(Value::as_u64) {
            Some(ms) => self.graph.get_within(&q, std::time::Duration::from_millis(ms)),
            None => self.graph.get(&q),
        }
        .map_err(|e| anyhow!("{e}"))?;
        let summary = summarize(&graph);
        Ok(json!({"path": q.path(), "summary": summary, "graph": graph}))
    }

    fn auth(&mut self, args: &Value) -> Result<Value> {
        match req_str(args, "action")? {
            "status" => {
                let verify = args.get("verify").and_then(Value::as_bool).unwrap_or(false);
                call(&mut self.backend, "cmd_session", json!({"verify": verify}))
            }
            "start" => call(&mut self.backend, "cmd_login_start", Value::Null),
            "poll" => call(&mut self.backend, "cmd_login_status", Value::Null),
            "logout" => call(&mut self.backend, "cmd_logout", Value::Null),
            other => bail!("action must be status, start, poll or logout (got {other})"),
        }
    }

    fn model(&mut self, args: &Value) -> Result<Value> {
        match req_str(args, "action")? {
            "status" => {
                let status = call(&mut self.backend, "cmd_model_status", Value::Null)?;
                Ok(merge(status, Map::from_iter([("app".to_string(), self.backend.state())])))
            }
            "download" => {
                let variant = opt_str(args, "variant").unwrap_or("text");
                if !matches!(variant, "text" | "full") {
                    bail!("variant must be text or full (got {variant})");
                }
                // A download is the user fixing what tripped the pre-warm breaker.
                self.backend.clear_suppression();
                call(&mut self.backend, "cmd_model_download", json!({"variant": variant}))
            }
            "cancel" => call(&mut self.backend, "cmd_model_download_cancel", Value::Null),
            // Scenario 2 Part 3: start the app before a question, without waiting (in-process tools stay instant).
            "prewarm" => {
                let low_memory = args
                    .get("avail_ram_mb")
                    .and_then(Value::as_u64)
                    .filter(|mb| *mb < crate::bridge::MIN_PREWARM_AVAIL_MB);
                let outcome = match low_memory {
                    Some(mb) => crate::bridge::PrewarmOutcome::Deferred {
                        reason: format!("only {mb} MB available (needs {} MB)", crate::bridge::MIN_PREWARM_AVAIL_MB),
                    },
                    None => self.backend.prewarm(),
                };
                let mut out = serde_json::to_value(&outcome)?;
                out["app"] = self.backend.state();
                Ok(out)
            }
            "evict" => {
                let stopped = self.backend.evict_if_idle();
                Ok(json!({"stopped": stopped, "app": self.backend.state()}))
            }
            other => bail!("action must be status, download, cancel, prewarm or evict (got {other})"),
        }
    }

    /// `pd_nav_number`. Without a frame context it is the stateless [`nav_number`]. With one (`screen_key`, `now_ms`,
    /// `scrolled`, `locked_numbers`, `priority_anchor`) the numbers stay put across calls of the same `session`
    /// (premium-discount-nav's `StickyNumberer`): a tier change or a jittering box does not renumber, numbers Zena is
    /// saying are locked, a box hidden for a moment keeps its number for 600 ms unless the screen scrolled.
    fn nav_number(&mut self, args: &Value) -> Result<Value> {
        const FRAME_KEYS: [&str; 5] = ["screen_key", "now_ms", "scrolled", "locked_numbers", "priority_anchor"];
        if !FRAME_KEYS.iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) {
            return nav_number(args);
        }
        let input = nav_input(args)?;
        let cx: premium_discount_nav::FrameContext =
            serde_json::from_value(args.clone()).context("screen_key / now_ms / scrolled / locked_numbers / priority_anchor have the wrong type")?;
        let session = opt_str(args, "session").unwrap_or("default").to_string();
        if !self.sticky.contains_key(&session) && self.sticky.len() >= MAX_NAV_SESSIONS {
            self.sticky.clear();
        }
        let state = self.sticky.entry(session.clone()).or_default();
        if args.get("reset").and_then(Value::as_bool) == Some(true) {
            state.numberer.reset();
        }
        let lockdown = matches!(input.shield, premium_discount_nav::PrivacyShield::FullLockdown(_));
        if lockdown && !state.locked {
            state.numberer.reset();
        }
        state.locked = lockdown;
        let regions = input.shield.redact(input.regions);
        let (numbered, sensitive) = state.numberer.update(regions, &input.tiers, input.iou, &cx);
        let mut out = nav_output(numbered, sensitive || lockdown, input.dropped, &input.shield);
        out["session"] = json!(session);
        Ok(out)
    }
}

/// Parsed `pd_nav_number` arguments.
struct NavInput {
    regions: Vec<premium_discount_nav::Region>,
    dropped: usize,
    tiers: std::collections::HashMap<String, premium_discount_verdict::Tier>,
    iou: f32,
    shield: premium_discount_nav::PrivacyShield,
    /// The host's inline redaction zones (a secret line, a sensitive cell): only those spots are hidden
    inline: Vec<premium_discount_nav::InlineRedactionZone>,
}

/// One `inline_redaction_zones` item: `{x, y, w, h, kind}` (kind `inline_secret_line` by default)
fn nav_inline_zone(v: &Value) -> Option<premium_discount_nav::InlineRedactionZone> {
    use premium_discount_nav::RedactionKind;
    let kind = match v.get("kind").and_then(Value::as_str).unwrap_or("inline_secret_line") {
        "os_notification_toast" => RedactionKind::OsNotificationToast,
        "sensitive_document_cell" => RedactionKind::SensitiveDocumentCell,
        _ => RedactionKind::InlineSecretLine,
    };
    Some(premium_discount_nav::InlineRedactionZone { rect: nav_rect(v)?, kind })
}

fn nav_rect(v: &Value) -> Option<premium_discount_nav::Rect> {
    let f = |k: &str| v.get(k).and_then(Value::as_f64).map(|x| x as f32);
    premium_discount_nav::Rect::new(f("x")?, f("y")?, f("w")?, f("h")?)
}

fn nav_input(args: &Value) -> Result<NavInput> {
    use std::collections::HashMap;
    let raw = args.get("regions").and_then(Value::as_array).context("regions must be an array")?;
    let mut regions = Vec::with_capacity(raw.len());
    let mut dropped = 0usize;
    for (i, r) in raw.iter().enumerate() {
        let mut region: premium_discount_nav::Region =
            serde_json::from_value(r.clone()).with_context(|| format!("regions[{i}] is not a pd-nav region"))?;
        match premium_discount_nav::Rect::new(region.rect.x, region.rect.y, region.rect.w, region.rect.h) {
            Some(rect) => {
                region.rect = rect;
                regions.push(region);
            }
            None => dropped += 1,
        }
    }
    let mut tiers: HashMap<String, premium_discount_verdict::Tier> = HashMap::new();
    if let Some(map) = args.get("tiers").and_then(Value::as_object) {
        for (anchor, tier) in map {
            let tier: premium_discount_verdict::Tier = serde_json::from_value(tier.clone())
                .with_context(|| format!("tiers.{anchor} must be premium, neutral, discount or unrated"))?;
            tiers.insert(anchor.clone(), tier);
        }
    }
    let iou = match args.get("iou") {
        None | Some(Value::Null) => premium_discount_nav::MERGE_IOU,
        Some(v) => {
            let iou = v.as_f64().context("iou must be a number")? as f32;
            if !(0.0..=1.0).contains(&iou) {
                bail!("iou must be between 0 and 1 (got {iou})");
            }
            iou
        }
    };
    // Scenario 6 Part 1: window- and vision-level signals the host measured, judged with the regions (keypad / OTP).
    let zones: Vec<premium_discount_nav::Rect> =
        args.get("os_notification_zones").and_then(Value::as_array).into_iter().flatten().filter_map(nav_rect).collect();
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let inline: Vec<premium_discount_nav::InlineRedactionZone> =
        args.get("inline_redaction_zones").and_then(Value::as_array).into_iter().flatten().filter_map(nav_inline_zone).collect();
    let probe = premium_discount_nav::PrivacyProbeInput {
        regions: &regions,
        os_capture_protected: flag("os_capture_protected"),
        os_overlay_payment_detected: flag("os_overlay_payment_detected"),
        os_notification_zones: &zones,
        vision_privacy_hit: flag("vision_privacy_hit"),
        seed_phrase_detected: flag("seed_phrase_detected"),
        auth_or_payment_context: flag("auth_or_payment_context"),
    };
    let shield = premium_discount_nav::evaluate_privacy_inline(&probe, &inline);
    Ok(NavInput { regions, dropped, tiers, iou, shield, inline })
}

fn nav_output(numbered: Vec<premium_discount_nav::Numbered>, sensitive: bool, dropped: usize, shield: &premium_discount_nav::PrivacyShield) -> Value {
    json!({
        "sensitive": sensitive,
        "record": !sensitive && shield.record(),
        "dropped": dropped,
        "max_numbered": premium_discount_nav::MAX_NUMBERED,
        "regions": numbered,
        "shield": shield,
    })
}

/// `pd_nav_number`: premium-discount's accessibility-navigation rules, so Raon Zena numbers a screen exactly as
/// premium-discount-app does. Candidates come from the accessibility tree, vision or events; rectangles are
/// clipped to the window (`Rect::new`, 0 ~ 1) and empty ones are dropped. On a sensitive screen (any candidate
/// marked `sensitive`: password, payment) only tree candidates survive the merge and the screen must not be
/// recorded (`record: false`). No app process, no model: pure functions.
///
/// Scenario 6 Part 1: the screen is also judged with premium-discount-nav's `evaluate_privacy` — an unmarked virtual
/// keypad or OTP row in the regions, or the host's window / vision signals (`os_capture_protected`,
/// `os_overlay_payment_detected`, `vision_privacy_hit`, `seed_phrase_detected`, `auth_or_payment_context`) lock it the
/// same way (`shield: {full_lockdown: <trigger>}`); `os_notification_zones` only drop the candidates under a toast
/// (`shield: {partial_redaction: …}`, still recorded). `inline_redaction_zones` (`{x, y, w, h, kind}`, kind
/// `inline_secret_line` | `sensitive_document_cell`) hide only that line or cell: a sensitive candidate under one does
/// not make the screen sensitive. Without those arguments the result is what it always was.
pub fn nav_number(args: &Value) -> Result<Value> {
    let input = nav_input(args)?;
    let under_inline = |r: &premium_discount_nav::Region| input.inline.iter().any(|z| r.rect.containment(&z.rect) > 0.0);
    let sensitive = input.regions.iter().any(|r| r.sensitive && !under_inline(r)) || matches!(input.shield, premium_discount_nav::PrivacyShield::FullLockdown(_));
    let regions = input.shield.redact(input.regions);
    let merged = premium_discount_nav::merge(regions, input.iou);
    let numbered = premium_discount_nav::number(merged, &input.tiers);
    Ok(nav_output(numbered, sensitive, input.dropped, &input.shield))
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

impl<B: Backend> ToolHandler for PdSyncServer<B> {
    fn tools(&self) -> Vec<Value> {
        let cc = s("Domain (store.steampowered.com), web URL (host is used) or desktop program app://<exe>[/<scene>] (→ <exe>.exe; the scene is returned, not sent)");
        let address = |what: &str| s(&format!("{what}: 0x + 40 hex"));
        vec![
            json!({
                "name": "pd_enqueue_node",
                "description": "Record one evaluation in the local premium-discount outbox (LanceDB, original text kept) and send it when online. Profanity is blocked locally (status blocked: nothing stored). Returns status saved | accepted (server checks it) | queued (offline or behind earlier items) with id or local_id.",
                "inputSchema": {"type": "object", "properties": {
                    "text": s("Text to store (at most 8192 bytes): the OCR / translation summary"),
                    "cc": cc,
                    "to": address("Target address"),
                    "to_key": s("Target key (window title, Relay Key slug); hashed to `to` with hashId"),
                    "bcc": address("Content address, or \"none\" to send none"),
                    "bcc_key": s("Content to address (the region text read at the drop); default: `text`. Normalized (NFC, whitespace) then hashId"),
                    "ref": s("Parent evaluation UUID on the server"),
                    "parent_local": s("Parent's outbox id (local-N) while it has no server id; resolved to `ref` when it is sent"),
                    "from": address("Author address (the server prefers the signed-in session)"),
                }, "required": ["text", "cc"]},
            }),
            json!({
                "name": "pd_flush_and_sync",
                "description": "Send queued evaluations and votes, check pending evaluations (stored / rejected / missing), then return local rows changed after since_ms (original text and state). Offline: flush.stopped / votes_stopped say why, rows still come back.",
                "inputSchema": {"type": "object", "properties": {
                    "since_ms": {"type": "integer", "minimum": 0, "default": 0, "description": "Last next_since_ms (0: every row)"},
                    "flush": {"type": "boolean", "default": true, "description": "Send and check before reading rows"},
                }},
            }),
            json!({
                "name": "pd_search_merged",
                "description": "One embedding, two searches: my local outbox (original text) and the server's index (other users' threads), merged by server id (source local | remote | both). Server matches carry the global verdict (tier premium | neutral | discount | unrated, grade -2..2) and my_vote. Offline: local matches only, with remote_error. A root / ref / to / tier filter returns server matches only.",
                "inputSchema": {"type": "object", "properties": {
                    "query": s("Search text (a question, or the text read at a drop)"),
                    "cc": cc,
                    "to": address("Target filter"),
                    "to_key": s("Target key, hashed to `to`"),
                    "root": s("Thread root UUID filter"),
                    "ref": s("Parent UUID filter"),
                    "tier": {"type": "string", "enum": TIER_FILTERS, "description": "Only recommended (premium), not recommended (discount) or any judged (rated) evaluations"},
                    "top_k": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10},
                    "l1_checkpoint_to": address("Scope boost L1: the current checkpoint's target (+0.12 to matches on it; ranks, does not filter)"),
                    "l1_checkpoint_key": s("Scope boost L1 as a key, hashed to l1_checkpoint_to"),
                    "l2_entity_tos": {"type": "array", "items": {"type": "string"}, "description": "Scope boost L2: targets of entities named in the question (+0.06 each match)"},
                    "l2_entity_keys": {"type": "array", "items": {"type": "string"}, "description": "Scope boost L2 as keys, hashed with hashId"},
                }, "required": ["query"]},
            }),
            json!({
                "name": "pd_vote",
                "description": "Recommend (1) or not recommend (-1) another user's evaluation, or withdraw (0). Written to the local vote outbox first and sent at once (status sent with counted, or queued offline). Only signed-in votes on other people's evaluations count; guest and own votes are stored but carry no weight.",
                "inputSchema": {"type": "object", "properties": {
                    "id": s("Evaluation UUID on the server"),
                    "value": {"type": "integer", "enum": [1, -1, 0]},
                    "kind": {"type": "string", "enum": ["vote", "vote_outcome"], "default": "vote", "description": "vote_outcome: a vote confirmed by an outcome (send only with the user's consent)"},
                }, "required": ["id", "value"]},
            }),
            json!({
                "name": "pd_supersede",
                "description": "Report that a newer evaluation supersedes an older one in the same place and target (premium-discount-relay: the same claim with a changed number, unit or adjective, e.g. damage 20% -> 15%, shipping 3,000 won -> free). The server checks that both share cc and to, that new is later and that the signed-in user wrote one of them; once accepted the old evaluation's verdict is discount (grade -2, reason superseded), in the local cache at once. Offline: an offline error, send again later (the same report is stored once).",
                "inputSchema": {"type": "object", "properties": {
                    "old": s("UUID of the superseded (earlier) evaluation"),
                    "new": s("UUID of the superseding (later) evaluation"),
                }, "required": ["old", "new"]},
            }),
            json!({
                "name": "pd_verdicts",
                "description": "Global verdicts of evaluations (PD-Verdict: tier, grade -2..2, rank, value, evidence, reason codes). Served from the local cache when it is under 10 minutes old, else asked from the server; offline: the cache with stale=true and never-seen ids in missing.",
                "inputSchema": {"type": "object", "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 200},
                    "refresh": {"type": "boolean", "default": false},
                }, "required": ["ids"]},
            }),
            json!({
                "name": "pd_graph_inspect",
                "description": "Read the server graph: a domain's evaluations, anonymous authors (author:<12 hex>) and reply threads (mode domain), the thread around one evaluation (local) or its nearest evaluations (similar). Evaluation nodes carry the server's tier and grade; the summary counts them.",
                "inputSchema": {"type": "object", "properties": {
                    "cc": cc,
                    "node_id": s("Evaluation UUID (modes local, similar)"),
                    "mode": {"type": "string", "enum": ["domain", "local", "similar"], "description": "Default: local with node_id, else domain"},
                    "top_k": {"type": "integer", "minimum": 1, "maximum": 49, "default": 10, "description": "similar only"},
                    "timeout_ms": {"type": "integer", "minimum": 50, "maximum": 15000, "description": "A short deadline for this read (e.g. 200 inside a 600 ms answer); default 15 s. A timeout is an offline error"},
                }},
            }),
            json!({
                "name": "pd_auth_session",
                "description": "QR e-mail login: status (verify asks the server), start (returns the address and mailto: link to show as a QR), poll (call every 3 s until signed_in or expired, 15 min), logout. The session is saved by premium-discount-app (session.json, owner-only), never returned.",
                "inputSchema": {"type": "object", "properties": {
                    "action": {"type": "string", "enum": ["status", "start", "poll", "logout"]},
                    "verify": {"type": "boolean", "default": false},
                }, "required": ["action"]},
            }),
            json!({
                "name": "pd_nav_number",
                "description": "Number a screen for keyboard / voice navigation with premium-discount's rules (pd-nav): merge candidate regions (IoU, tree > vision > event; events mark a region changed), then order premium regions first and discount last, then by role (input, button, link, menu, text, image, other) and reading order; the first 9 get numbers 1-9. A screen with any sensitive candidate keeps tree candidates only and must not be recorded (record false). Runs in process: no app, no model.",
                "inputSchema": {"type": "object", "properties": {
                    "regions": {"type": "array", "items": {"type": "object", "properties": {
                        "anchor": s("Position key of the region (stable across visits; tiers are keyed by it)"),
                        "role": {"type": "string", "enum": ["input", "button", "link", "menu", "text", "image", "other"]},
                        "name": s("Accessible name or label (may be empty)"),
                        "rect": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "w": {"type": "number"}, "h": {"type": "number"}}, "required": ["x", "y", "w", "h"], "description": "Window-relative 0..1"},
                        "source": {"type": "string", "enum": ["tree", "vision", "event"]},
                        "score": {"type": "number"},
                        "changed": {"type": "boolean", "default": false},
                        "sensitive": {"type": "boolean", "default": false, "description": "Password, payment or other personal field"},
                    }, "required": ["anchor", "role", "name", "rect", "source", "score"]}},
                    "tiers": {"type": "object", "additionalProperties": {"type": "string", "enum": ["premium", "neutral", "discount", "unrated"]}, "description": "anchor -> verdict tier (local or the server's)"},
                    "iou": {"type": "number", "minimum": 0, "maximum": 1, "description": "Merge threshold (default 0.4)"},
                    "session": s("Sticky numbering: which watched window this is (default \"default\")"),
                    "screen_key": s("Sticky numbering: screen_hash / page_hash; a new key renumbers from scratch. Any frame field turns sticky numbering on: numbers stay put across calls (tier changes, jitter, a box hidden < 600 ms)"),
                    "now_ms": {"type": "integer", "description": "Sticky numbering: the device clock"},
                    "scrolled": {"type": "boolean", "description": "Sticky numbering: the screen scrolled since the last call (numbers of boxes that left are reclaimed at once)"},
                    "locked_numbers": {"type": "integer", "minimum": 0, "maximum": 1022, "description": "Sticky numbering: bit n locks number n (Zena is saying it); a locked number is never reassigned"},
                    "priority_anchor": s("Sticky numbering: the anchor that must get a number 1-9 (e.g. the time-trial ghost's next checkpoint)"),
                    "reset": {"type": "boolean", "description": "Sticky numbering: forget the session's numbers first (window switch)"},
                    "os_capture_protected": {"type": "boolean", "description": "Privacy: a capture-protected window (WDA) is visible on the captured monitor → full lockdown"},
                    "os_overlay_payment_detected": {"type": "boolean", "description": "Privacy: a payment overlay window is visible → full lockdown"},
                    "os_notification_zones": {"type": "array", "items": {"type": "object"}, "description": "Privacy: notification toasts ({x,y,w,h}, window-relative): candidates under them are dropped"},
                    "inline_redaction_zones": {"type": "array", "items": {"type": "object"}, "description": "Privacy: a secret line or a sensitive cell ({x,y,w,h,kind}, kind inline_secret_line | sensitive_document_cell): only that spot is hidden, the rest is still numbered and recorded"},
                    "vision_privacy_hit": {"type": "boolean", "description": "Privacy: the screen encoder saw a privacy domain → full lockdown"},
                    "seed_phrase_detected": {"type": "boolean", "description": "Privacy: a seed phrase is in the title / URL → full lockdown"},
                    "auth_or_payment_context": {"type": "boolean", "description": "Privacy: the page is a login / payment flow (lets an unlabeled digit grid count as a security keypad)"},
                }, "required": ["regions"]},
            }),
            json!({
                "name": "pd_model_manage",
                "description": "embeddinggemma-2 for premium-discount-app: status (installed variant, download progress, whether the app process is running), download (text: 574 MB text-only; full: 1.52 GB; resumable, SHA-256 checked) or cancel. prewarm starts the app and its model load in the background and returns at once (outcome started_async | already_warming | already_warm | suppressed | deferred); a process woken only this way is stopped after 45 s unless a real command uses it. evict stops an idle app now (graceful: it releases its model and GPU pool first).",
                "inputSchema": {"type": "object", "properties": {
                    "action": {"type": "string", "enum": ["status", "download", "cancel", "prewarm", "evict"]},
                    "variant": {"type": "string", "enum": ["text", "full"], "default": "text"},
                    "avail_ram_mb": {"type": "integer", "minimum": 0, "description": "prewarm: available physical memory measured by the host; under 650 MB the pre-warm is deferred"},
                }, "required": ["action"]},
            }),
            json!({
                "name": "pd_analytic",
                "description": "Web analytics for a site the signed-in user owns (premium-discount's tracker): site (ownership check: the site's front page carries <meta name=\"premium-discount-site\" content=\"<login address>\">), sync (receive raw events, decode, structure them on this device; events the owner forgot never come back), report (a natural-language question → time window recomputed on the user's clock, funnel, bottlenecks, choice contrasts; this week vs last week compares equal elapsed time; too few sessions read as insufficient_sample, never as a drop; a short sync runs first when the last one is over 5 minutes old) or forget (event ids, and/or every event of the host up to before_ms — kept as tombstones).",
                "inputSchema": {"type": "object", "properties": {
                    "action": {"type": "string", "enum": ["site", "sync", "report", "forget"]},
                    "host": s("Site host (shop.example.com) or a URL of it"),
                    "query": s("report: the question, e.g. \"이번 주 vs 지난주 전환율 왜 떨어졌어?\""),
                    "lang": s("report: the user's language (time zone and hemisphere), default ko"),
                    "top_k": {"type": "integer", "minimum": 1, "maximum": 50, "description": "report: summary matches"},
                    "sync": {"type": "boolean", "default": true, "description": "report: false skips the automatic short sync"},
                    "max_pages": {"type": "integer", "minimum": 1, "maximum": 20, "description": "sync: pages of up to 500 events"},
                    "ids": {"type": "array", "items": {"type": "string"}, "description": "forget: event ids"},
                    "before_ms": {"type": "integer", "description": "forget: every event of the host created at or before this time (epoch ms)"},
                    "flush_remote": {"type": "boolean", "default": false, "description": "forget: also ask the server to delete them"},
                }, "required": ["action", "host"]},
            }),
        ]
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        let out = match name {
            "pd_enqueue_node" => self.enqueue(args),
            "pd_flush_and_sync" => self.flush_and_sync(args),
            "pd_search_merged" => self.search(args),
            "pd_vote" => self.vote(args),
            "pd_supersede" => self.supersede(args),
            "pd_verdicts" => self.verdicts(args),
            "pd_graph_inspect" => self.graph(args),
            "pd_auth_session" => self.auth(args),
            "pd_model_manage" => self.model(args),
            "pd_nav_number" => self.nav_number(args),
            "pd_analytic" => self.analytic(args),
            other => bail!("unknown_command: unknown tool {other}"),
        };
        // Arguments this layer refused carry no kind yet.
        out.map_err(|e| {
            if has_kind(&format!("{e}")) {
                e
            } else {
                anyhow!("invalid_input: {e:#}")
            }
        })
    }
}
