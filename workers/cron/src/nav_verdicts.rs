//! Phase 5 Part 1: PD-Verdict v2 (`nav` 트랙) 일별 버킷 압축 스윕, 목적지 동류 3단계 스코프,
//! 패치 드리프트(`drift_z`) 반감기 수축, 및 `nav_batch_receipts` 24시간 TTL 정리.
//!
//! `nav_counts` 는 이미 `(hash, day, type, recipe)` 로 일별 합산(`n`)되어 있으므로
//! 최근 90일(반감기 6주기 이상, 잔존율 < 1.6%) 버킷만 읽어 개별 Obs 전개와 수학적으로 동일한 `(E, v)` 를
//! O(D) 벡터 연산으로 계산한다.

use premium_discount_verdict::{
    effective_half_life_ms, judge_in, AdaptiveHalfLifeCfg, Baselines, Context, Level, Math, Params, Prev, TrackParams, Verdict, IMPLICIT, NAV, RECIPE_V2,
};

pub const NAV_RECIPE: &str = RECIPE_V2;
pub const DAY_MS: f64 = 86_400_000.0;
pub const NAV_BUCKET_LOOKBACK_DAYS: i64 = 90;
pub const NAV_DRIFT_TRIGGER_Z: f64 = 1.5;
pub const NAV_DRIFT_GAMMA: f64 = 1.2;
pub const NAV_MIN_HALF_LIFE_MS: f64 = 2.0 * DAY_MS;
pub const BATCH_RECEIPT_TTL_MS: f64 = DAY_MS;

/// 일별 집계 버킷 하나 (`nav_counts` 의 같은 날짜 +1 / -1 합).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyBucket {
    pub day: i64,
    pub up: u32,
    pub down: u32,
}

/// 스윕의 반감기 매개변수 ([`effective_half_life_ms`]): `|drift_z| ≥ 1.5` 이면 `H / (1 + 1.2 (|z| − 1.5))`, 하한 2일.
/// 휴지기 연장(활성 비율)은 쓰지 않는다 (활성 비율 1.0: 지금의 판정 값을 그대로 둔다)
pub const NAV_HALF_LIFE_CFG: AdaptiveHalfLifeCfg = AdaptiveHalfLifeCfg {
    drift_z_trigger: NAV_DRIFT_TRIGGER_Z,
    drift_gamma: NAV_DRIFT_GAMMA,
    min_drift_half_life_ms: NAV_MIN_HALF_LIFE_MS,
    max_dormant_half_life_ms: 45.0 * DAY_MS,
};

/// Phase 5 Part 1: `|drift_z| ≥ 1.5` 일 때 nav 트랙 반감기(14일)를 동적으로 압축한다(하한 2일).
pub fn adaptive_nav_half_life_ms(base_half_life_ms: f64, drift_z: f64) -> f64 {
    effective_half_life_ms(base_half_life_ms, drift_z.is_finite().then_some(drift_z), 1.0, &NAV_HALF_LIFE_CFG)
}

/// 일별 버킷의 과거 구간 대비 최근 구간 순부호 급변(`drift_z`)을 산출한다.
/// 최근 4개 활성 버킷(또는 절반 이하)의 평균 순부호가 전체 이력 평균 대비 얼마나 떨어졌는지 측정한다.
pub fn daily_bucket_drift_z(buckets: &[DailyBucket]) -> f64 {
    let mut sorted: Vec<DailyBucket> = buckets.iter().copied().filter(|b| b.up + b.down > 0).collect();
    sorted.sort_by_key(|b| b.day);
    // 개별 표로 펼친 순부호 스트림(최대 256건)에서 최근 4표(Welford ring = min_obs.primary)의 drift_z 계산.
    let mut vals: Vec<f64> = Vec::new();
    for b in &sorted {
        for _ in 0..b.up.min(64) {
            vals.push(1.0);
        }
        for _ in 0..b.down.min(64) {
            vals.push(-1.0);
        }
    }
    if vals.len() < 6 {
        return 0.0;
    }
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    let var = vals.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0).max(1.0);
    let sd = var.max(0.0).sqrt();
    if sd <= 1e-9 {
        return 0.0;
    }
    let ring_len = 4usize.min(vals.len() / 2).max(1);
    let recent_mean = vals[vals.len() - ring_len..].iter().sum::<f64>() / ring_len as f64;
    (recent_mean - mean) / sd
}

/// 일별 버킷 압축 스윕으로 PD-Verdict v2 `(E, v, up_total, down_total)` 를 산출한다.
pub fn compressed_nav_evidence(
    buckets: &[DailyBucket],
    now_ms: i64,
    weight: f64,
    half_life_ms: f64,
    k: f64,
    math: &Math,
) -> (f64, f64, u32, u32) {
    let now_day = (now_ms as f64 / DAY_MS).floor() as i64;
    let min_day = now_day - NAV_BUCKET_LOOKBACK_DAYS;
    let mut e = 0.0f64;
    let mut num = 0.0f64;
    let mut up_total = 0u32;
    let mut down_total = 0u32;
    for b in buckets {
        if b.day < min_day {
            continue;
        }
        let n_up = b.up;
        let n_down = b.down;
        if n_up == 0 && n_down == 0 {
            continue;
        }
        up_total = up_total.saturating_add(n_up);
        down_total = down_total.saturating_add(n_down);
        let day_noon_ms = (b.day as f64) * DAY_MS + DAY_MS * 0.5;
        let age_ms = (now_ms as f64 - day_noon_ms).max(0.0);
        let decay = (math.pow)(2.0, -age_ms / half_life_ms);
        let w = weight * decay;
        e += f64::from(n_up + n_down) * w;
        num += (f64::from(n_up) - f64::from(n_down)) * w;
    }
    let v = if e > 0.0 { num / (e + k) } else { 0.0 };
    (e, v, up_total, down_total)
}

/// 목적지 동류 3단계 스코프 키 생성 (Phase 5 Part 1):
/// * 코스(`kind == "course"`): `secondary = course:<app>|goal:<goal_cp>`, `primary = course:<app>`, `global = nav:course:*`
/// * 체크포인트(`kind == "checkpoint"`): `secondary = cp:<app>|scope:<parent_or_goal>`, `primary = cp:<app>`, `global = nav:*`
pub fn nav_scope_keys(kind: &str, app: &str, scope_to: Option<&str>) -> Vec<(String, Level)> {
    let mut out = Vec::with_capacity(3);
    if kind == "course" {
        if let Some(goal) = scope_to.filter(|s| !s.is_empty()) {
            out.push((format!("course:{app}|goal:{goal}"), Level::Secondary));
        }
        out.push((format!("course:{app}"), Level::Primary));
        out.push(("nav:course:*".to_string(), Level::Global));
    } else {
        if let Some(parent) = scope_to.filter(|s| !s.is_empty()) {
            out.push((format!("cp:{app}|scope:{parent}"), Level::Secondary));
        }
        out.push((format!("cp:{app}"), Level::Primary));
        out.push(("nav:*".to_string(), Level::Global));
    }
    out
}

/// 일별 버킷과 기준선으로부터 PD-Verdict v2 (`nav` 트랙) 판정을 직접 산출한다.
/// 반환: `(verdict, up_total, down_total, effective_half_life_ms)`.
pub fn judge_nav_buckets(
    buckets: &[DailyBucket],
    now_ms: i64,
    baselines: &Baselines,
    prev: Prev,
    superseded: bool,
    use_drift_half_life: bool,
    math: &Math,
) -> (Verdict, u32, u32, f64) {
    let params = Params::v2();
    let track: &TrackParams = params.track(NAV).expect("pd-verdict/2 has nav track");
    let drift_z = if use_drift_half_life { daily_bucket_drift_z(buckets) } else { 0.0 };
    let h_eff = if use_drift_half_life {
        adaptive_nav_half_life_ms(track.half_life_ms, drift_z)
    } else {
        track.half_life_ms
    };
    let (e, v, up, down) = compressed_nav_evidence(buckets, now_ms, params.weight(IMPLICIT), h_eff, params.k, math);
    let cx = Context {
        track,
        baselines,
        prev,
        now_ms,
        duel: None,
        superseded,
    };
    let verdict = judge_in(math, e, v, &cx, &params);
    (verdict, up, down, h_eff)
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use indexmap::{IndexMap, IndexSet};
    use premium_discount_verdict::{baseline_of, Baseline, Baselines, Level, Prev, Tier};
    use premium_discount_worker_common::js::{self, Env, Js};
    use premium_discount_worker_common::jsv::{strict_eq, R, V};
    use premium_discount_worker_common::stringify_str;

    use super::*;
    use crate::queue::stmt;
    use crate::verdict::{math, tier_of};

    const NAV_VERDICT_BATCH: f64 = 50.0;
    const NAV_VERDICT_SWEEP: f64 = 25.0;
    const NAV_JUDGE_LIMIT: f64 = 500.0;
    const NAV_JUDGE_CHUNK: usize = 200;

    const SQL_NAV_DIRTY: &str = "SELECT \"hash\", \"kind\", \"app\", \"scope_to\", \"at\" FROM nav_verdict_dirty ORDER BY \"at\" LIMIT ?1";
    const SQL_NAV_SWEEP: &str = "SELECT \"hash\", \"kind\", \"app\", \"scope_to\" FROM nav_verdicts ORDER BY \"updated_at\" LIMIT ?1";
    const SQL_NAV_CLEAR_MARK: &str = "DELETE FROM nav_verdict_dirty WHERE \"hash\" = ?1 AND \"at\" <= ?2";
    const SQL_NAV_BUCKETS: &str = "SELECT \"day\", \"type\", SUM(\"n\") AS \"n\" FROM nav_counts WHERE \"recipe\" = ?1 AND \"hash\" = ?2 AND \"day\" >= ?3 GROUP BY \"day\", \"type\"";
    const SQL_NAV_UPSERT: &str = "INSERT INTO nav_verdicts (\"hash\", \"kind\", \"app\", \"scope_to\", \"evidence\", \"value\", \"up\", \"down\", \"level\", \"rank\", \"tier\", \"grade\", \"tier_since\", \"reasons\", \"recipe\", \"superseded\", \"updated_at\", \"judged_at\")\n\t\t VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 'unrated', NULL, ?9, '[]', ?10, 0, ?9, ?9)\n\t\t ON CONFLICT (\"hash\") DO UPDATE SET \"evidence\" = excluded.\"evidence\", \"value\" = excluded.\"value\", \"up\" = excluded.\"up\", \"down\" = excluded.\"down\", \"scope_to\" = COALESCE(excluded.\"scope_to\", nav_verdicts.\"scope_to\"), \"updated_at\" = excluded.\"updated_at\"";
    const SQL_NAV_VALS_SECONDARY: &str = "SELECT \"value\" FROM nav_verdicts WHERE \"kind\" = ?1 AND \"app\" = ?2 AND \"scope_to\" = ?3 AND \"evidence\" >= ?4";
    const SQL_NAV_VALS_PRIMARY: &str = "SELECT \"value\" FROM nav_verdicts WHERE \"kind\" = ?1 AND \"app\" = ?2 AND \"evidence\" >= ?3";
    const SQL_NAV_VALS_GLOBAL: &str = "SELECT \"value\" FROM nav_verdicts WHERE \"kind\" = ?1 AND \"evidence\" >= ?2";
    const SQL_NAV_SAVE_SCOPE: &str = "INSERT OR REPLACE INTO nav_verdict_scopes (\"scope_key\", \"n\", \"mean\", \"sd\", \"updated_at\") VALUES (?1, ?2, ?3, ?4, ?5)";
    const SQL_NAV_READ_SCOPES: &str = "SELECT \"scope_key\", \"n\", \"mean\", \"sd\" FROM nav_verdict_scopes WHERE \"scope_key\" IN (SELECT \"value\" FROM json_each(?1))";
    const SQL_NAV_JUDGE_ROWS: &str = "SELECT \"hash\", \"kind\", \"app\", \"scope_to\", \"evidence\", \"value\", \"tier\", \"tier_since\", \"superseded\" FROM nav_verdicts WHERE \"hash\" IN (SELECT \"value\" FROM json_each(?1))";
    const SQL_NAV_PEERS_SECONDARY: &str = "SELECT \"hash\", \"kind\", \"app\", \"scope_to\", \"evidence\", \"value\", \"tier\", \"tier_since\", \"superseded\" FROM nav_verdicts WHERE \"kind\" = ?1 AND \"app\" = ?2 AND \"scope_to\" = ?3 ORDER BY \"judged_at\" LIMIT ?4";
    const SQL_NAV_PEERS_PRIMARY: &str = "SELECT \"hash\", \"kind\", \"app\", \"scope_to\", \"evidence\", \"value\", \"tier\", \"tier_since\", \"superseded\" FROM nav_verdicts WHERE \"kind\" = ?1 AND \"app\" = ?2 ORDER BY \"judged_at\" LIMIT ?3";
    const SQL_NAV_WRITE: &str = "UPDATE nav_verdicts SET \"tier\" = json_extract(j.\"value\", '$.tier'), \"grade\" = json_extract(j.\"value\", '$.grade'), \"rank\" = json_extract(j.\"value\", '$.rank'), \"level\" = json_extract(j.\"value\", '$.level'), \"reasons\" = json_extract(j.\"value\", '$.reasons'), \"tier_since\" = json_extract(j.\"value\", '$.since'), \"recipe\" = ?2, \"judged_at\" = ?3 FROM json_each(?1) AS j WHERE nav_verdicts.\"hash\" = json_extract(j.\"value\", '$.id')";
    const SQL_CLEAN_BATCH_RECEIPTS: &str = "DELETE FROM nav_batch_receipts WHERE \"created_ms\" < ?1";

    #[derive(Clone)]
    struct NavScope {
        key: String,
        level: Level,
        kind: String,
        app: String,
        scope_to: Option<String>,
    }

    fn scopes_for(kind: &str, app: &str, scope_to: Option<&str>) -> Vec<NavScope> {
        nav_scope_keys(kind, app, scope_to)
            .into_iter()
            .map(|(key, level)| NavScope {
                key,
                level,
                kind: kind.to_string(),
                app: app.to_string(),
                scope_to: if level == Level::Secondary { scope_to.map(str::to_string) } else { None },
            })
            .collect()
    }

    pub async fn cleanup_batch_receipts(db: &V, now_ms: f64) -> R<f64> {
        let cutoff = now_ms - BATCH_RECEIPT_TTL_MS;
        let s = V::ext(Js(js::call(db, "prepare", &[SQL_CLEAN_BATCH_RECEIPTS.into()])?));
        let res = js::run(&js::bind_exact(&s, &[V::Num(cutoff)])?).await?;
        let meta = res.g("meta")?;
        let changes = if meta.is_nullish() { V::Undef } else { meta.get("changes") };
        Ok(if changes.is_nullish() { 0.0 } else { changes.to_num() })
    }

    /// `processNavVerdicts(env, now)` — 일별 버킷 압축 스윕 + 3단계 목적지 동류 스코프 판정.
    pub async fn process_nav_verdicts(env: &Env, now: f64) -> R<(f64, f64)> {
        let dirty = js::all(&stmt(env, SQL_NAV_DIRTY, &[V::Num(NAV_VERDICT_BATCH)])?).await?;
        let swept = js::all(&stmt(env, SQL_NAV_SWEEP, &[V::Num(NAV_VERDICT_SWEEP)])?).await?;
        let mut work: IndexMap<String, (String, String, Option<String>, Option<V>)> = IndexMap::new();
        for d in dirty.g("results")?.items() {
            let hash = d.g("hash")?.to_key();
            let kind = d.g("kind")?.to_str();
            let app = d.g("app")?.to_str();
            let sc = d.g("scope_to")?;
            let scope_to = if sc.is_nullish() { None } else { Some(sc.to_str()) };
            work.insert(hash, (kind, app, scope_to, Some(d.g("at")?)));
        }
        for s in swept.g("results")?.items() {
            let hash = s.g("hash")?.to_key();
            let kind = s.g("kind")?.to_str();
            let app = s.g("app")?.to_str();
            let sc = s.g("scope_to")?;
            let scope_to = if sc.is_nullish() { None } else { Some(sc.to_str()) };
            work.entry(hash).or_insert((kind, app, scope_to, None));
        }
        if work.is_empty() {
            return Ok((0.0, 0.0));
        }

        let params = Params::v2();
        let track = params.track(NAV).expect("nav track");
        let now_day = (now / DAY_MS).floor() as i64;
        let min_day = (now_day - NAV_BUCKET_LOOKBACK_DAYS) as f64;
        let mut touched: IndexMap<String, NavScope> = IndexMap::new();
        let mut computed: Vec<String> = Vec::new();

        for (hash, (kind, app, scope_to, mark_at)) in &work {
            let rs = js::all(&stmt(env, SQL_NAV_BUCKETS, &[V::from(NAV_RECIPE), V::from(hash.as_str()), V::Num(min_day)])?).await?;
            let mut by_day: IndexMap<i64, DailyBucket> = IndexMap::new();
            for row in rs.g("results")?.items() {
                let day = row.g("day")?.to_num() as i64;
                let n = row.g("n")?.to_num().max(0.0) as u32;
                let entry = by_day.entry(day).or_insert(DailyBucket { day, up: 0, down: 0 });
                if strict_eq(&row.g("type")?, &V::Num(1.0)) {
                    entry.up = entry.up.saturating_add(n);
                } else {
                    entry.down = entry.down.saturating_add(n);
                }
            }
            let buckets: Vec<DailyBucket> = by_day.into_values().collect();
            let drift_z = daily_bucket_drift_z(&buckets);
            let h_eff = adaptive_nav_half_life_ms(track.half_life_ms, drift_z);
            let (e, val, up, down) = compressed_nav_evidence(&buckets, now as i64, params.weight(IMPLICIT), h_eff, params.k, math());
            let sc_v = scope_to.as_deref().map_or(V::Null, V::from);
            js::run(&stmt(
                env,
                SQL_NAV_UPSERT,
                &[
                    V::from(hash.as_str()),
                    V::from(kind.as_str()),
                    V::from(app.as_str()),
                    sc_v,
                    V::Num(e),
                    V::Num(val),
                    V::Num(f64::from(up)),
                    V::Num(f64::from(down)),
                    V::Num(now),
                    V::from(NAV_RECIPE),
                ],
            )?)
            .await?;
            if let Some(at) = mark_at {
                js::run(&stmt(env, SQL_NAV_CLEAR_MARK, &[V::from(hash.as_str()), at.clone()])?).await?;
            }
            for s in scopes_for(kind, app, scope_to.as_deref()) {
                touched.insert(s.key.clone(), s);
            }
            computed.push(hash.clone());
        }

        let mut baselines: IndexMap<String, Baseline> = IndexMap::new();
        for s in touched.values() {
            let min_e = V::Num(params.min_evidence);
            let rs = match s.level {
                Level::Secondary => {
                    js::all(&stmt(
                        env,
                        SQL_NAV_VALS_SECONDARY,
                        &[V::from(s.kind.as_str()), V::from(s.app.as_str()), V::from(s.scope_to.as_deref().unwrap_or("")), min_e],
                    )?)
                    .await?
                }
                Level::Primary => js::all(&stmt(env, SQL_NAV_VALS_PRIMARY, &[V::from(s.kind.as_str()), V::from(s.app.as_str()), min_e])?).await?,
                Level::Global => js::all(&stmt(env, SQL_NAV_VALS_GLOBAL, &[V::from(s.kind.as_str()), min_e])?).await?,
            };
            let vals: Vec<f64> = rs.g("results")?.items().iter().map(|r| r.g("value").map(|v| v.to_num())).collect::<R<Vec<_>>>()?;
            let b = baseline_of(&vals);
            js::run(&stmt(env, SQL_NAV_SAVE_SCOPE, &[V::from(s.key.as_str()), V::Num(b.n as f64), V::Num(b.mean), V::Num(b.sd), V::Num(now)])?).await?;
            baselines.insert(s.key.clone(), b);
        }

        let mut rows: IndexMap<String, V> = IndexMap::new();
        if !computed.is_empty() {
            let list = V::from(computed.iter().map(|h| V::from(h.as_str())).collect::<Vec<_>>());
            for r in js::all(&stmt(env, SQL_NAV_JUDGE_ROWS, &[V::from(stringify_str(&list))])?).await?.g("results")?.items() {
                rows.insert(r.g("hash")?.to_key(), r);
            }
        }
        for s in touched.values() {
            if s.level == Level::Global {
                continue;
            }
            let rs = if s.level == Level::Secondary {
                js::all(&stmt(
                    env,
                    SQL_NAV_PEERS_SECONDARY,
                    &[V::from(s.kind.as_str()), V::from(s.app.as_str()), V::from(s.scope_to.as_deref().unwrap_or("")), V::Num(NAV_JUDGE_LIMIT)],
                )?)
                .await?
            } else {
                js::all(&stmt(env, SQL_NAV_PEERS_PRIMARY, &[V::from(s.kind.as_str()), V::from(s.app.as_str()), V::Num(NAV_JUDGE_LIMIT)])?).await?
            };
            for r in rs.g("results")?.items() {
                rows.insert(r.g("hash")?.to_key(), r);
            }
        }
        let mut missing: IndexSet<String> = IndexSet::new();
        for r in rows.values() {
            let kind = r.g("kind")?.to_str();
            let app = r.g("app")?.to_str();
            let sc = r.g("scope_to")?;
            let scope_to = if sc.is_nullish() { None } else { Some(sc.to_str()) };
            for (k, _) in nav_scope_keys(&kind, &app, scope_to.as_deref()) {
                if !baselines.contains_key(&k) {
                    missing.insert(k);
                }
            }
        }
        let missing_vec: Vec<String> = missing.into_iter().collect();
        for chunk in missing_vec.chunks(NAV_JUDGE_CHUNK) {
            let list = V::from(chunk.iter().map(|x| V::from(x.as_str())).collect::<Vec<_>>());
            for r in js::all(&stmt(env, SQL_NAV_READ_SCOPES, &[V::from(stringify_str(&list))])?).await?.g("results")?.items() {
                baselines.insert(
                    r.g("scope_key")?.to_key(),
                    Baseline {
                        n: r.g("n")?.to_num() as u64,
                        mean: r.g("mean")?.to_num(),
                        sd: r.g("sd")?.to_num(),
                    },
                );
            }
        }

        let mut judged: Vec<V> = Vec::new();
        for r in rows.values() {
            let kind = r.g("kind")?.to_str();
            let app = r.g("app")?.to_str();
            let sc = r.g("scope_to")?;
            let scope_to = if sc.is_nullish() { None } else { Some(sc.to_str()) };
            let mut bs = Baselines::default();
            for (k, lv) in nav_scope_keys(&kind, &app, scope_to.as_deref()) {
                if let Some(b) = baselines.get(&k) {
                    match lv {
                        Level::Secondary => bs.secondary = Some(*b),
                        Level::Primary => bs.primary = Some(*b),
                        Level::Global => bs.global = Some(*b),
                    }
                }
            }
            let prev_tier = r.g("tier")?;
            let prev_since = r.g("tier_since")?;
            let known = tier_of(&prev_tier);
            let cx = Context {
                track,
                baselines: &bs,
                prev: Prev { tier: known.unwrap_or(Tier::Unrated), since_ms: prev_since.to_num() as i64 },
                now_ms: now as i64,
                duel: None,
                superseded: strict_eq(&r.g("superseded")?, &V::Num(1.0)),
            };
            let v = judge_in(math(), r.g("evidence")?.to_num(), r.g("value")?.to_num(), &cx, &params);
            let since = if known == Some(v.tier) { prev_since } else { V::Num(now) };
            let item = V::new_obj();
            item.put("id", r.g("hash")?);
            item.put("tier", V::from(v.tier.name()));
            item.put("grade", v.grade.map_or(V::Null, |g| V::Num(f64::from(g))));
            item.put("rank", v.rank.map_or(V::Null, V::Num));
            item.put("level", v.level.map_or(V::Null, |l| V::from(l.name())));
            item.put("reasons", V::from(v.reasons.iter().map(|x| V::from(x.as_str())).collect::<Vec<_>>()));
            item.put("since", since);
            judged.push(item);
        }
        for chunk in judged.chunks(NAV_JUDGE_CHUNK) {
            let list = V::from(chunk.to_vec());
            js::run(&stmt(env, SQL_NAV_WRITE, &[V::from(stringify_str(&list)), V::from(NAV_RECIPE), V::Num(now)])?).await?;
        }
        Ok((computed.len() as f64, judged.len() as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_verdict::{Baseline, Tier};

    #[test]
    fn cn257_nav_verdicts_decay_and_drift_flip_stale_premium_to_discount() {
        // 45일 전 +20표, 최근 3일 내 -6표: 비감쇠 2/3 비율에서는 20/26 = 76.9% 로 좀비 Premium 고착이지만,
        // PD-Verdict v2 (14일 반감기 + drift_z 반감기 수축)에서는 즉시 Discount 로 전환된다!
        let today = 20_800i64;
        let now_ms = today * DAY_MS as i64 + (DAY_MS as i64 / 2);
        let buckets = vec![
            DailyBucket { day: today - 45, up: 20, down: 0 },
            DailyBucket { day: today - 2, up: 0, down: 2 },
            DailyBucket { day: today - 1, up: 0, down: 2 },
            DailyBucket { day: today, up: 0, down: 2 },
        ];
        // 최근 6표에 더해 반감기 보강이 충분하도록 최근 3일 -8표 시나리오도 함께 검증
        let buckets_decided = vec![
            DailyBucket { day: today - 45, up: 20, down: 0 },
            DailyBucket { day: today - 2, up: 0, down: 3 },
            DailyBucket { day: today - 1, up: 0, down: 3 },
            DailyBucket { day: today, up: 0, down: 3 },
        ];
        let bs = Baselines {
            secondary: Some(Baseline { n: 10, mean: 0.15, sd: 0.25 }),
            primary: Some(Baseline { n: 20, mean: 0.10, sd: 0.25 }),
            global: Some(Baseline { n: 50, mean: 0.05, sd: 0.25 }),
        };
        let prev = Prev { tier: Tier::Premium, since_ms: (today - 45) * DAY_MS as i64 };

        // 45일 전 +20표는 반감기 감쇠로 소멸하고 최근 -6표가 v < 0 으로 지배함
        let (v6, _, _, h6) = judge_nav_buckets(&buckets, now_ms, &bs, prev, false, true, &Math::STD);
        assert!(v6.value < -0.1, "stale +20 vs recent -6 must have negative value, got {}", v6.value);
        assert!(h6 < 14.0 * DAY_MS, "patch drift must compress half-life below 14d: {h6}");

        // 결정 근거량 도달 시 즉시 Discount 로 강등 (비감쇠 20/29 = 69% 는 여전히 Premium 이었을 케이스!)
        let (v9, up, down, _) = judge_nav_buckets(&buckets_decided, now_ms, &bs, prev, false, true, &Math::STD);
        assert_eq!((up, down), (20, 9));
        assert_eq!(v9.tier, Tier::Discount, "decayed + drift-compressed verdict must flip to Discount: {v9:?}");

        // 목적지 동류 3단계 스코프 키 검증 (튜토리얼 <-> 보스방 간섭 차단)
        let scopes = nav_scope_keys("course", "0xapp", Some("0xboss_goal"));
        assert_eq!(
            scopes,
            vec![
                ("course:0xapp|goal:0xboss_goal".to_string(), Level::Secondary),
                ("course:0xapp".to_string(), Level::Primary),
                ("nav:course:*".to_string(), Level::Global),
            ]
        );
    }
}
