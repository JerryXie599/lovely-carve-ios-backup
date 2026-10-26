//! powerlog（.PLSQL）引擎：时间基准自动判别、电池快照与充满会话、解锁事件与
//! 触发应用关联、锁定/亮屏区间、应用清单与前后台时长。

use crate::util::{day_plus8, fmt_plus8, ts_plus8, Epoch};
use lovelycarve_plugin::{FileRef, Run, Value};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;

pub struct PowerlogOut {
    pub epoch: Option<Epoch>,
    pub unlock_first: Option<(i64, String)>, // (unix, trigger_app)
    pub charge_last_reach: Option<(f64, i64)>, // (raw_ts, unix)
    pub cycle_count: Option<f64>,
    pub health_pct: Option<f64>,
    pub level_last: Option<f64>,
    pub apps: usize,
}

/// 把检材文件复制到临时目录再打开（连同同名 -wal/-shm）。
pub fn open(f: &FileRef, sidecars: &[FileRef]) -> rusqlite::Result<(tempfile::TempDir, Connection)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("powerlog.sqlite");
    let mut buf = Vec::with_capacity(f.size() as usize);
    let mut off = 0u64;
    while off < f.size() {
        let n = ((f.size() - off).min(1 << 20)) as usize;
        let chunk = f.read_at(off, n).expect("read");
        buf.extend_from_slice(&chunk);
        off += n as u64;
    }
    std::fs::write(&path, &buf).expect("write db");
    for suffix in ["-wal", "-shm"] {
        let want = format!("{}{suffix}", f.name());
        if let Some(s) = sidecars.iter().find(|s| s.name() == want) {
            let mut b = Vec::with_capacity(s.size() as usize);
            let mut o = 0u64;
            while o < s.size() {
                let n = ((s.size() - o).min(1 << 20)) as usize;
                b.extend_from_slice(&s.read_at(o, n).expect("read side"));
                o += n as u64;
            }
            std::fs::write(tmp.path().join(format!("powerlog.sqlite{suffix}")), b).ok();
        }
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok((tmp, conn))
}

fn table_exists(conn: &Connection, t: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |_| Ok(()),
    )
    .is_ok()
}

/// 主入口：一个 PLSQL → 全部记录。
pub fn analyze(
    run: &Run,
    f: &FileRef,
    sidecars: &[FileRef],
    anchor_unix: Option<i64>,
) -> lovelycarve_plugin::Result<PowerlogOut> {
    analyze_opts(run, f, sidecars, anchor_unix, true)
}

/// with_apps=false 时跳过 ios.app / ios.app_usage 的输出（分组分析器只取电源组记录）。
pub fn analyze_opts(
    run: &Run,
    f: &FileRef,
    sidecars: &[FileRef],
    anchor_unix: Option<i64>,
    with_apps: bool,
) -> lovelycarve_plugin::Result<PowerlogOut> {
    let (_tmp, conn) = open(f, sidecars)
        .map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", format!("{}: {e}", f.name())))?;
    let mut out = PowerlogOut {
        epoch: None,
        unlock_first: None,
        charge_last_reach: None,
        cycle_count: None,
        health_pct: None,
        level_last: None,
        apps: 0,
    };

    // ---- 时间基准判别 ----
    let mut samples: Vec<f64> = Vec::new();
    if table_exists(&conn, "PLBatteryAgent_EventBackward_Battery") {
        samples = conn
            .prepare("SELECT timestamp FROM PLBatteryAgent_EventBackward_Battery ORDER BY ID DESC LIMIT 500")
            .and_then(|mut s| {
                let mut v = Vec::new();
                let mut rows = s.query([])?;
                while let Some(r) = rows.next()? {
                    if let Some(t) = r.get::<_, Option<f64>>(0)? {
                        v.push(t);
                    }
                }
                Ok(v)
            })
            .unwrap_or_default();
    }
    let anchor = anchor_unix.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let epoch = Epoch::detect(&samples, anchor);
    out.epoch = epoch;
    if epoch.is_none() && !samples.is_empty() {
        run.warn(
            "EPOCH_UNKOWN",
            format!("{}：无法判别时间基准（中值两种换算都不在合理区间），时间字段降级", f.name()),
        )?;
    }
    let cv = |raw: f64| -> Option<i64> { epoch.map(|e| e.to_unix(raw)) };

    if table_exists(&conn, "PLBatteryAgent_EventBackward_Battery") {
        battery(run, &conn, f, &cv, &mut out)?;
    }
    if table_exists(&conn, "PLSpringBoardAgent_EventForward_SBLock") {
        locks(run, &conn, f, &cv, &mut out)?;
    }
    if table_exists(&conn, "PLSpringBoardAgent_EventForward_SBBlank") {
        screens(run, &conn, f, &cv)?;
    }
    if with_apps && table_exists(&conn, "PLApplicationAgent_EventNone_AppVersions") {
        apps(run, &conn, f, &cv, &mut out)?;
    }
    if with_apps && table_exists(&conn, "PLAppTimeService_Aggregate_AppRunTime") {
        usage(run, &conn, f, &cv)?;
    }
    Ok(out)
}

fn f64v(v: rusqlite::types::ValueRef<'_>) -> Option<f64> {
    use rusqlite::types::ValueRef as V;
    match v {
        V::Integer(i) => Some(i as f64),
        V::Real(x) => Some(x),
        _ => None,
    }
}

/// 电池：最新快照 + 充满会话 + 每日采样。
fn battery(
    run: &Run,
    conn: &Connection,
    f: &FileRef,
    cv: &dyn Fn(f64) -> Option<i64>,
    out: &mut PowerlogOut,
) -> lovelycarve_plugin::Result<()> {
    // 最新快照
    let mut stmt = conn
        .prepare(
            "SELECT timestamp, Level, CycleCount, Temperature, DesignCapacity, NominalChargeCapacity, AppleRawMaxCapacity, FullyCharged, IsCharging, ExternalConnected FROM PLBatteryAgent_EventBackward_Battery ORDER BY timestamp DESC LIMIT 1",
        )
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let (raw, level, cycle, temp, design, nom, rawmax, full, chg, ext): (f64, Option<f64>, Option<f64>, Option<f64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = (
            r.get(0).unwrap_or_default(), r.get(1).unwrap_or_default(), r.get(2).unwrap_or_default(), r.get(3).unwrap_or_default(), r.get(4).unwrap_or_default(), r.get(5).unwrap_or_default(), r.get(6).unwrap_or_default(), r.get(7).unwrap_or_default(), r.get(8).unwrap_or_default(), r.get(9).unwrap_or_default(),
        );
        out.cycle_count = cycle;
        out.level_last = level;
        let mut item = |k: String, v: String, note: String| {
            run.record_values("ios.battery", vec![k.into(), v.into(), note.into()])
                .from(f)
                .emit()
        };
        item(
            "最新电量%".into(),
            level.map(|v| format!("{v:.0}")).unwrap_or_default(),
            format!("最新采样 {}", cv(raw).map(fmt_plus8).unwrap_or_default()),
        )?;
        item("循环次数".into(), cycle.map(|v| format!("{v:.0}")).unwrap_or_default(), "CycleCount 最新采样".into())?;
        item(
            "健康度%（NominalChargeCapacity/DesignCapacity）".into(),
            match (nom, design) {
                (Some(n), Some(d)) if d > 0 => {
                    let h = n as f64 / d as f64 * 100.0;
                    out.health_pct = Some(h);
                    format!("{h:.0}")
                }
                (None, Some(d)) if d > 0 => rawmax
                    .map(|m| {
                        let h = m as f64 / d as f64 * 100.0;
                        out.health_pct = Some(h);
                        format!("{h:.0}（按 AppleRawMaxCapacity 估算）")
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            },
            "NominalChargeCapacity / DesignCapacity".into(),
        )?;
        item("设计容量mAh".into(), design.map(|v| v.to_string()).unwrap_or_default(), String::new())?;
        item("温度°C".into(), temp.map(|v| format!("{v:.1}")).unwrap_or_default(), String::new())?;
        item(
            "充电状态".into(),
            format!(
                "FullyCharged={} IsCharging={} ExternalConnected={}",
                full.unwrap_or(0),
                chg.unwrap_or(0),
                ext.unwrap_or(0)
            ),
            "最新采样标志位".into(),
        )?;
    }

    // Level=100 会话切分（间隔>600s 断开）
    let mut stmt = conn
        .prepare("SELECT timestamp, Level, IsCharging, FullyCharged FROM PLBatteryAgent_EventBackward_Battery ORDER BY timestamp")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut sessions: Vec<Vec<(f64, Option<i64>, Option<i64>)>> = Vec::new();
    let mut prev: Option<f64> = None;
    // 会话起点回溯：记录进入 100% 前的最后一条 <100 且充电中的采样
    let mut pre_charge: Vec<Option<(f64, Option<i64>)>> = Vec::new();
    let mut last_below: Option<(f64, Option<i64>)> = None;
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let raw: f64 = r.get(0).unwrap_or_default();
        let level: Option<f64> = r.get(1).unwrap_or_default();
        let chg: Option<i64> = r.get(2).unwrap_or_default();
        let full: Option<i64> = r.get(3).unwrap_or_default();
        let is100 = level == Some(100.0);
        if is100 {
            let new_session = match (sessions.last(), prev) {
                (Some(s), Some(p)) => raw - p > 600.0 || s.is_empty(),
                (None, _) => true,
                _ => true,
            };
            if new_session {
                sessions.push(Vec::new());
                pre_charge.push(last_below.filter(|(_, c)| *c == Some(1)));
            }
            sessions.last_mut().unwrap().push((raw, chg, full));
        } else {
            last_below = Some((raw, chg));
        }
        prev = Some(raw);
    }
    for (i, s) in sessions.iter().enumerate() {
        let first = s.first().map(|(t, _, _)| *t);
        let fc = s.iter().find(|(_, _, f)| *f == Some(1)).map(|(t, _, _)| *t);
        let last = s.last().map(|(t, _, _)| *t);
        let reach_unix = first.and_then(cv);
        let pre = pre_charge.get(i).copied().flatten();
        if i + 1 == sessions.len() {
            out.charge_last_reach = first.map(|t| (t, reach_unix.unwrap_or(0)));
        }
        run.record_values(
            "ios.charge_full",
            vec![
                (i + 1).into(),
                first.map(|t| format!("{t}")).unwrap_or_default().into(),
                (reach_unix.and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                (fc.and_then(cv).and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                (last.and_then(cv).and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                s.len().into(),
                Value::from(pre.and_then(|(t, _)| cv(t)).and_then(ts_plus8).map(|t| t.as_str().to_string()).unwrap_or_default()),
                "充至100%=会话首条 Level=100 采样（原始时间戳列 raw_ts）；FullyCharged 为系统置满标志首条".into(),
            ],
        )
        .from(f)
        .note(format!("PLBatteryAgent_EventBackward_Battery Level=100 会话{}", i + 1))
        .emit()?;
    }

    // 每日采样
    let mut stmt = conn
        .prepare("SELECT timestamp, Level, Temperature, CycleCount FROM PLBatteryAgent_EventBackward_Battery ORDER BY timestamp")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    #[derive(Default)]
    struct Day {
        n: usize,
        lvl_min: Option<f64>,
        lvl_max: Option<f64>,
        t_min: Option<f64>,
        t_max: Option<f64>,
        cyc: Option<f64>,
    }
    fn fold(e: &mut Option<f64>, v: Option<f64>, min: bool) {
        *e = Some(match (*e, v) {
            (Some(a), Some(b)) => {
                if min { a.min(b) } else { a.max(b) }
            }
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return,
        });
    }
    let mut days: BTreeMap<String, Day> = BTreeMap::new();
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let raw: f64 = r.get(0).unwrap_or_default();
        let Some(u) = cv(raw) else { continue };
        let day = day_plus8(u);
        let e = days.entry(day).or_default();
        e.n += 1;
        let lvl: Option<f64> = r.get(1).unwrap_or_default();
        let temp: Option<f64> = r.get(2).unwrap_or_default();
        let cyc: Option<f64> = r.get(3).unwrap_or_default();
        fold(&mut e.lvl_min, lvl, true);
        fold(&mut e.lvl_max, lvl, false);
        fold(&mut e.t_min, temp, true);
        fold(&mut e.t_max, temp, false);
        e.cyc = cyc.or(e.cyc);
    }
    for (day, e) in days {
        run.record_values(
            "ios.battery_daily",
            vec![
                day.into(),
                (e.n as i64).into(),
                Value::from(e.lvl_min.unwrap_or(0.0)),
                Value::from(e.lvl_max.unwrap_or(0.0)),
                Value::from(e.t_min.unwrap_or(0.0)),
                Value::from(e.t_max.unwrap_or(0.0)),
                Value::from(e.cyc.unwrap_or(0.0)),
            ],
        )
        .from(f)
        .emit()?;
    }
    Ok(())
}

/// SBLock：Locked 1→0 变化=解锁动作；ScreenState 关联触发应用。
fn locks(
    run: &Run,
    conn: &Connection,
    f: &FileRef,
    cv: &dyn Fn(f64) -> Option<i64>,
    out: &mut PowerlogOut,
) -> lovelycarve_plugin::Result<()> {
    let mut stmt = conn
        .prepare("SELECT timestamp, Locked FROM PLSpringBoardAgent_EventForward_SBLock ORDER BY timestamp")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut seq: Vec<(f64, i64)> = Vec::new();
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let raw: f64 = r.get(0).unwrap_or_default();
        let locked: i64 = r.get(1).unwrap_or_default();
        seq.push((raw, locked));
    }
    // 去除同一时刻重复/冲突（保留首见）
    let mut clean: Vec<(f64, i64)> = Vec::new();
    for (t, l) in seq {
        match clean.last_mut() {
            Some((pt, pl)) if (*pt - t).abs() < 1.0 && *pl == l => {}
            Some((pt, pl)) if (*pt - t).abs() < 1.0 && *pl != l => {
                // 同秒内状态翻转：以新值为准（SBLock 同秒多次写入时最后一笔生效）
                *pt = t;
                *pl = l;
            }
            _ => clean.push((t, l)),
        }
    }
    // 锁定状态区间
    let mut i = 0usize;
    while i < clean.len() {
        let mut j = i;
        while j + 1 < clean.len() && clean[j + 1].1 == clean[i].1 {
            j += 1;
        }
        let (start, state) = clean[i];
        let end = clean[j].0;
        if let (Some(su), Some(eu)) = (cv(start), cv(end)) {
            run.record_values(
                "ios.lock_interval",
                vec![
                    (if state == 1 { "锁定" } else { "解锁" }).into(),
                    (ts_plus8(su)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                    (ts_plus8(eu)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                    Value::from((end - start) as i64),
                    (j - i + 1).into(),
                ],
            )
            .from(f)
            .emit()?;
        }
        i = j + 1;
    }
    // 解锁事件（1→0）
    let screen: Vec<(f64, i64, String)> = if table_exists(conn, "PLScreenStateAgent_EventForward_ScreenState") {
        conn.prepare("SELECT timestamp, AppRole, bundleID FROM PLScreenStateAgent_EventForward_ScreenState ORDER BY timestamp")
            .and_then(|mut s| {
                let mut v = Vec::new();
                let mut rows = s.query([])?;
                while let Some(r) = rows.next()? {
                    let t: f64 = r.get(0).unwrap_or_default();
                    let role: Option<i64> = r.get(1).unwrap_or_default();
                    let b: Option<String> = r.get(2).unwrap_or_default();
                    v.push((t, role.unwrap_or(0), b.unwrap_or_default()));
                }
                Ok(v)
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let system_ui = |b: &str| {
        b == "com.apple.springboard.home-screen" || b == "com.apple.lock-screen" || b.is_empty()
    };
    for w in clean.windows(2) {
        let ((t0, l0), (t1, l1)) = (w[0], w[1]);
        if l0 == 1 && l1 == 0 {
            let Some(u) = cv(t1) else { continue };
            // 解锁后 15s 内第一个非系统 UI 的前台应用（AppRole=1），记录锁屏层应用线索
            let mut trigger = String::new();
            let mut chain: Vec<String> = Vec::new();
            for (st, role, b) in &screen {
                if *st < t1 - 0.5 || *st > t1 + 15.0 {
                    continue;
                }
                if role == &3 || role == &6 {
                    chain.push(b.clone());
                    continue;
                }
                if role == &1 && !system_ui(b) {
                    trigger = b.clone();
                    break;
                }
                if role == &1 {
                    chain.push(b.clone());
                }
            }
            if out.unlock_first.is_none() {
                out.unlock_first = Some((u, trigger.clone()));
            }
            run.record_values(
                "ios.unlock",
                vec![
                    (ts_plus8(u)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                    format!("{t1}").into(),
                    "解锁".into(),
                    trigger.clone().into(),
                    chain.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join(" → ").into(),
                    Value::from((t1 - t0) as i64),
                ],
            )
            .from(f)
            .note(format!("SBLock Locked 1→0 @ {t1}"))
            .emit()?;
        }
    }
    Ok(())
}

/// SBBlank：亮屏（Blanked=0）/息屏（=1）区间。
fn screens(run: &Run, conn: &Connection, f: &FileRef, cv: &dyn Fn(f64) -> Option<i64>) -> lovelycarve_plugin::Result<()> {
    let mut stmt = conn
        .prepare("SELECT timestamp, Blanked FROM PLSpringBoardAgent_EventForward_SBBlank ORDER BY timestamp")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut seq: Vec<(f64, i64)> = Vec::new();
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        seq.push((r.get(0).unwrap_or_default(), r.get(1).unwrap_or_default()));
    }
    let mut i = 0usize;
    while i < seq.len() {
        let mut j = i;
        while j + 1 < seq.len() && seq[j + 1].1 == seq[i].1 {
            j += 1;
        }
        let (start, state) = seq[i];
        let end = seq[j].0;
        if let (Some(su), Some(eu)) = (cv(start), cv(end)) {
            run.record_values(
                "ios.screen_interval",
                vec![
                    (if state == 0 { "亮屏" } else { "息屏" }).into(),
                    (ts_plus8(su)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                    (ts_plus8(eu)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                    Value::from((end - start) as i64),
                    (j - i + 1).into(),
                ],
            )
            .from(f)
            .emit()?;
        }
        i = j + 1;
    }
    Ok(())
}

/// AppVersions：应用清单（名称/版本/更新时间/删除时间/第三方标志）。
fn apps(
    run: &Run,
    conn: &Connection,
    f: &FileRef,
    cv: &dyn Fn(f64) -> Option<i64>,
    out: &mut PowerlogOut,
) -> lovelycarve_plugin::Result<()> {
    let mut stmt = conn
        .prepare("SELECT AppBundleId, AppName, AppBundleVersion, AppBuildVersion, AppIs3rdParty, AppDeletedDate, AppUpdatedDate, timestamp FROM PLApplicationAgent_EventNone_AppVersions")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // bundle -> (name, version, updated, deleted, third_party, count)
    let mut agg: BTreeMap<String, (String, String, Option<f64>, Option<f64>, i64, i64)> = BTreeMap::new();
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let bundle: String = r.get(0).unwrap_or_default();
        let name: Option<String> = r.get(1).unwrap_or_default();
        let ver: Option<String> = r.get(2).unwrap_or_default();
        let build: Option<String> = r.get(3).unwrap_or_default();
        let third: Option<i64> = r.get(4).unwrap_or_default();
        let deleted: Option<f64> = r.get(5).unwrap_or_default();
        let updated: Option<f64> = r.get(6).unwrap_or_default();
        let snap: f64 = r.get(7).unwrap_or_default();
        let e = agg.entry(bundle).or_insert((String::new(), String::new(), None, None, third.unwrap_or(0), 0));
        if let Some(n) = name {
            if n.len() > e.0.len() { e.0 = n; }
        }
        if let Some(v) = ver {
            e.1 = if build.as_deref().map(|b| !b.is_empty()).unwrap_or(false) {
                format!("{v} ({})", build.unwrap_or_default())
            } else {
                v
            };
        }
        if let Some(u2) = updated {
            e.2 = Some(e.2.map_or(u2, |a: f64| a.max(u2)));
        }
        e.3 = e.3.or(deleted);
        e.4 = third.unwrap_or(e.4);
        e.5 += 1;
        let _ = snap;
    }
    out.apps = agg.len();
    for (bundle, (name, ver, updated, deleted, third, count)) in agg {
        run.record_values(
            "ios.app",
            vec![
                bundle.clone().into(),
                crate::appmap::name(&bundle).unwrap_or("").into(),
                ver.into(),
                (if third == 1 { "第三方应用" } else if third == 0 { "系统应用" } else { "" }).into(),
                Value::Boolean(false),
                Value::Nil,
                "powerlog AppVersions".into(),
                (updated.and_then(cv).and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                format!("AppVersions 快照×{count}；AppUpdatedDate 见 InstallTime；AppDeletedDate={}", deleted.map(|d| cv(d).map(fmt_plus8).unwrap_or_default()).unwrap_or_default()).into(),
            ],
        )
        .from(f)
        .emit()?;
    }
    Ok(())
}

/// AppRunTime：前台亮屏秒（ScreenOnTime）与后台秒（BackgroundTime；插电单列合并说明）。
fn usage(run: &Run, conn: &Connection, f: &FileRef, cv: &dyn Fn(f64) -> Option<i64>) -> lovelycarve_plugin::Result<()> {
    let mut stmt = conn
        .prepare("SELECT BundleID, ScreenOnTime, BackgroundTime, ScreenOnPluggedInTime, BackgroundPluggedInTime, timestamp FROM PLAppTimeService_Aggregate_AppRunTime")
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // bundle -> (fg, bg, samples, first, last)
    let mut agg: BTreeMap<String, (f64, f64, i64, Option<f64>, Option<f64>)> = BTreeMap::new();
    while let Some(r) = rows.next().map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))? {
        let bundle: String = r.get(0).unwrap_or_default();
        let fg: Option<f64> = r.get(1).unwrap_or_default();
        let bg: Option<f64> = r.get(2).unwrap_or_default();
        let fg_p: Option<f64> = r.get(3).unwrap_or_default();
        let bg_p: Option<f64> = r.get(4).unwrap_or_default();
        let snap: f64 = r.get(5).unwrap_or_default();
        let e = agg.entry(bundle.clone()).or_insert((0.0, 0.0, 0, None, None));
        e.0 += fg.unwrap_or(0.0) + fg_p.unwrap_or(0.0);
        e.1 += bg.unwrap_or(0.0) + bg_p.unwrap_or(0.0);
        e.2 += 1;
        e.3 = Some(e.3.map_or(Some(snap), |m: f64| Some(m.min(snap))).unwrap_or(snap));
        e.4 = Some(e.4.map_or(Some(snap), |m: f64| Some(m.max(snap))).unwrap_or(snap));
    }
    let mut list: Vec<_> = agg.into_iter().collect();
    list.sort_by(|a, b| b.1 .0.partial_cmp(&a.1 .0).unwrap_or(std::cmp::Ordering::Equal));
    for (bundle, (fg, bg, n, first, last)) in list {
        run.record_values(
            "ios.app_usage",
            vec![
                bundle.clone().into(),
                crate::appmap::name(&bundle).unwrap_or("").into(),
                Value::from(fg),
                Value::from(bg),
                n.into(),
                (first.and_then(cv).and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                (last.and_then(cv).and_then(ts_plus8)).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
            ],
        )
        .from(f)
        .emit()?;
    }
    Ok(())
}

/// 仅输出应用前后台时长（分组分析器用）。
pub fn usage_only(run: &Run, f: &FileRef, sidecars: &[FileRef]) -> lovelycarve_plugin::Result<()> {
    let (_tmp, conn) = open(f, sidecars)
        .map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", format!("{}: {e}", f.name())))?;
    let mut samples: Vec<f64> = Vec::new();
    if table_exists(&conn, "PLBatteryAgent_EventBackward_Battery") {
        samples = conn
            .prepare("SELECT timestamp FROM PLBatteryAgent_EventBackward_Battery ORDER BY ID DESC LIMIT 500")
            .and_then(|mut s| {
                let mut v = Vec::new();
                let mut rows = s.query([])?;
                while let Some(r) = rows.next()? {
                    if let Some(t) = r.get::<_, Option<f64>>(0)? {
                        v.push(t);
                    }
                }
                Ok(v)
            })
            .unwrap_or_default();
    }
    let anchor = chrono::Utc::now().timestamp();
    let epoch = Epoch::detect(&samples, anchor);
    let cv = |raw: f64| -> Option<i64> { epoch.map(|e| e.to_unix(raw)) };
    if table_exists(&conn, "PLAppTimeService_Aggregate_AppRunTime") {
        usage(run, &conn, f, &cv)?;
    }
    Ok(())
}
