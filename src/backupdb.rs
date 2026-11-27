//! iOS 备份业务数据库解析：短信/通话/通讯录/蓝牙配对/账户/WiFi（+相册/笔记防御式）。
//! 输入文件名与表列口径依据标准 iOS 备份结构；时间列为 Apple epoch（2001-01-01），
//! iOS 16 起部分库改用纳秒整数，按量级自动判别。

use crate::util::{civil, ts_plus8, APPLE_EPOCH, BEIJING};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use rusqlite::{Connection, OpenFlags};
use std::time::{SystemTime, UNIX_EPOCH};

/// Apple epoch 秒/纳秒 → UTC+8 Timestamp（整秒）。
pub fn apple(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 {
        (v / 1e9) as i64
    } else {
        v as i64
    };
    ts_plus8(APPLE_EPOCH + secs)
}

/// Apple epoch 秒/纳秒 → 保留毫秒精度的 UTC+8 Timestamp（适合短时间间隔取证对比）。
pub fn apple_precise(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let secs_f = if v.abs() > 1e14 { v / 1e9 } else { v };
    let unix_ms = ((APPLE_EPOCH as f64 + secs_f) * 1000.0).round() as i64;
    let unix_sec = unix_ms.div_euclid(1000);
    let ms = unix_ms.rem_euclid(1000);
    let (y, mo, d, h, mi, s) = civil(unix_sec + 8 * 3600)?;
    if ms == 0 {
        Timestamp::with_offset(y, mo, d, h, mi, s, BEIJING)
    } else {
        Timestamp::parse(&format!(
            "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}+08:00"
        ))
    }
}

/// 完整读取一个检材文件字节。
pub fn read_file_bytes(f: &FileRef) -> Vec<u8> {
    let mut buf = Vec::with_capacity(f.size() as usize);
    let mut off = 0u64;
    while off < f.size() {
        let n = ((f.size() - off).min(1 << 20)) as usize;
        let chunk = f.read_at(off, n).unwrap_or_default();
        if chunk.is_empty() {
            break;
        }
        buf.extend_from_slice(&chunk);
        off += chunk.len() as u64;
    }
    buf
}

/// 在伴随文件列表里定位同名 sidecar（例如 `-wal` / `-shm`）。
pub fn find_sidecar<'a>(f: &FileRef, sidecars: &'a [FileRef], suffix: &str) -> Option<&'a FileRef> {
    let want_name = format!("{}{suffix}", f.name());
    sidecars
        .iter()
        .find(|s| s.name().eq_ignore_ascii_case(&want_name))
}

/// plist::Date（绝对时间）→ UTC+8 Timestamp。
fn plist_date(d: &plist::Date) -> Option<Timestamp> {
    let unix = SystemTime::from(*d)
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    ts_plus8(unix)
}

pub fn ts_or_nil(t: Option<Timestamp>) -> Value {
    t.map(|t| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil)
}

/// 拷贝检材数据库及同名 `-wal` 到临时目录并用 SQLite 打开。
///
/// 刻意不拷入异构设备（如 ARM64 iOS）残留的 `-shm` WAL-index 文件：跨平台直接复
/// 用源 `-shm` 可能导致宿主 SQLite 拒绝挂载或跳过 `-wal` 重放；以可写模式打开临
/// 时副本让 SQLite 自行重建本地 `-shm` 并完整重放已提交事务，失败时回退只读。
/// （原始 `-wal` 与 `-shm` 的未提交/旧盐值/底层帧由 `sqlite_wal` 独立解析。）
pub fn open_db(
    f: &FileRef,
    sidecars: &[FileRef],
) -> Result<(tempfile::TempDir, Connection), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = tmp.path().join("db.sqlite");
    std::fs::write(&path, read_file_bytes(f)).map_err(|e| e.to_string())?;
    if let Some(wal) = find_sidecar(f, sidecars, "-wal") {
        std::fs::write(tmp.path().join("db.sqlite-wal"), read_file_bytes(wal)).ok();
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| {
        Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    })
    .map_err(|e| e.to_string())?;
    Ok((tmp, conn))
}

/// 从内存字节打开临时只读 SQLite 数据库（供解密后的 iOS 备份数据库复用解析逻辑）。
pub fn open_db_from_bytes(bytes: &[u8]) -> Result<(tempfile::TempDir, Connection), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = tmp.path().join("decrypted.sqlite");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    Ok((tmp, conn))
}

fn err(e: impl std::fmt::Display) -> lovelycarve_plugin::Error {
    lovelycarve_plugin::Error::new("SQL", e.to_string())
}

pub fn table_exists(conn: &Connection, t: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |_| Ok(()),
    )
    .is_ok()
}

pub fn cols(conn: &Connection, t: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info([{t}])"))
        .and_then(|mut s| {
            let mut v = Vec::new();
            let mut rows = s.query([])?;
            while let Some(r) = rows.next()? {
                if let Ok(Some(n)) = r.get::<_, Option<String>>(1) {
                    v.push(n);
                }
            }
            Ok(v)
        })
        .unwrap_or_default()
}

pub fn pick<'a>(available: &'a [String], want: &[&str]) -> Option<&'a str> {
    want.iter().find_map(|w| {
        available
            .iter()
            .find(|c| c.eq_ignore_ascii_case(w))
            .map(|c| c.as_str())
    })
}

// ---------- 短信 ----------
pub fn sms(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    sms_conn(run, f, &conn)
}

pub fn sms_conn(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "message") {
        run.warn_file(f, "SCHEMA", "无 message 表")?;
        return Ok(0);
    }
    let c = cols(conn, "message");
    let (Some(text), Some(date)) = (pick(&c, &["text"]), pick(&c, &["date"])) else {
        run.warn_file(f, "SCHEMA", "message 表缺 text/date 列")?;
        return Ok(0);
    };
    let handle_id = pick(&c, &["handle_id"]);
    let from_me = pick(&c, &["is_from_me"]);
    let is_read = pick(&c, &["is_read"]);
    let service = pick(&c, &["service"]);
    let subject = pick(&c, &["subject"]);
    let mut sel = vec![
        "m.ROWID".to_string(),
        format!("m.{text}"),
        format!("m.{date}"),
    ];
    if let Some(x) = from_me {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = is_read {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = service {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = subject {
        sel.push(format!("m.{x}"));
    }
    let join = handle_id
        .map(|h| format!(" LEFT JOIN handle h ON h.ROWID = m.{h}"))
        .unwrap_or_default();
    let hsel = if handle_id.is_some() {
        ", h.id"
    } else {
        ", NULL"
    };
    let sql = format!(
        "SELECT {}{} FROM message m{join} ORDER BY m.{date}",
        sel.join(", "),
        hsel
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("sms 查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // 附件
    let mut att: std::collections::HashMap<i64, String> = Default::default();
    if table_exists(conn, "message_attachment_join") && table_exists(conn, "attachment") {
        if let Ok(mut s) = conn.prepare(
            "SELECT j.message_id, a.filename FROM message_attachment_join j LEFT JOIN attachment a ON a.ROWID=j.attachment_id",
        ) {
            if let Ok(mut rs) = s.query([]) {
                while let Ok(Some(r)) = rs.next() {
                    if let (Ok(mid), Ok(Some(fname))) =
                        (r.get::<_, i64>(0), r.get::<_, Option<String>>(1))
                    {
                        att.entry(mid).or_insert(fname);
                    }
                }
            }
        }
    }
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let rowid: i64 = r.get(0).unwrap_or_default();
        let body: Option<String> = r.get(1).unwrap_or(None);
        let date_v: Option<f64> = r.get(2).unwrap_or(None);
        let from_me_v: Option<i64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        let read_v: Option<i64> = if cnt > 4 {
            r.get(4).unwrap_or(None)
        } else {
            None
        };
        let service_v: Option<String> = if cnt > 5 {
            r.get(5).unwrap_or(None)
        } else {
            None
        };
        let subject_v: Option<String> = if cnt > 6 {
            r.get(6).unwrap_or(None)
        } else {
            None
        };
        let h_id: Option<String> = r.get(cnt - 1).unwrap_or(None);
        run.record_values(
            "ios.sms",
            vec![
                h_id.clone().unwrap_or_default().into(),
                format!(
                    "{}{}",
                    subject_v.map(|s| format!("[{s}] ")).unwrap_or_default(),
                    body.unwrap_or_default()
                )
                .into(),
                ts_or_nil(apple(date_v.unwrap_or_default())),
                (if from_me_v == Some(1) {
                    "发送"
                } else {
                    "接收"
                })
                .into(),
                (if read_v == Some(1) {
                    "已读"
                } else {
                    "未读"
                })
                .into(),
                service_v.unwrap_or_default().into(),
                att.get(&rowid).cloned().unwrap_or_default().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 通话 ----------
/// 系统通话（ZCALLRECORD）与 WhatsApp 通话（ZWACDCALLEVENT / ZWAJOINABLECALLEVENT）。
pub fn calllog(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    calllog_conn(run, f, &conn)
}

pub fn calllog_conn(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
) -> lovelycarve_plugin::Result<usize> {
    if table_exists(conn, "ZCALLRECORD") {
        return system_call(run, f, conn);
    }
    if table_exists(conn, "ZWACDCALLEVENT") {
        return wa_call(
            run,
            f,
            conn,
            "ZWACDCALLEVENT",
            &["ZGROUPJIDSTRING"],
            &["ZGROUPJIDSTRING"],
            false,
        );
    }
    if table_exists(conn, "ZWAJOINABLECALLEVENT") {
        return wa_call(
            run,
            f,
            conn,
            "ZWAJOINABLECALLEVENT",
            &["ZFROMJIDSTRING", "ZGROUPJIDSTRING"],
            &["ZFROMJIDSTRING", "ZGROUPJIDSTRING"],
            true,
        );
    }
    run.warn_file(f, "SCHEMA", "无 ZCALLRECORD / WhatsApp 通话表")?;
    Ok(0)
}

fn system_call(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let c = cols(conn, "ZCALLRECORD");
    let Some(addr) = pick(&c, &["ZADDRESS"]) else {
        return Ok(0);
    };
    let date = pick(&c, &["ZDATE", "ZDATEORIGINATION"]).unwrap_or("ZDATE");
    let dur = pick(&c, &["ZDURATION"]);
    let orig = pick(&c, &["ZORIGINATED"]);
    let ans = pick(&c, &["ZANSWERED"]);
    let mut sel = vec![format!("m.{addr}"), format!("m.{date}")];
    if let Some(x) = dur {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = orig {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = ans {
        sel.push(format!("m.{x}"));
    }
    let sql = format!(
        "SELECT {} FROM ZCALLRECORD m ORDER BY m.{date}",
        sel.join(", ")
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let number: Option<String> = r.get(0).unwrap_or(None);
        let date_v: Option<f64> = r.get(1).unwrap_or(None);
        let d: Option<f64> = if cnt > 2 {
            r.get(2).unwrap_or(None)
        } else {
            None
        };
        let o: Option<i64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        let a: Option<i64> = if cnt > 4 {
            r.get(4).unwrap_or(None)
        } else {
            None
        };
        let kind = match (o, a) {
            (Some(1), _) => "呼出",
            (_, Some(0)) => "未接",
            _ => "呼入",
        };
        run.record_values(
            "ios.calllog",
            vec![
                number.unwrap_or_default().into(),
                Value::Nil,
                Value::from(d.unwrap_or(0.0)),
                ts_or_nil(apple(date_v.unwrap_or_default())),
                kind.into(),
                "系统电话".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

fn wa_call(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
    table: &str,
    jid_cols: &[&str],
    _fallback: &[&str],
    joinable: bool,
) -> lovelycarve_plugin::Result<usize> {
    let c = cols(conn, table);
    let date = pick(&c, &["ZDATE", "ZFIRSTDATE"]);
    let Some(date) = date else { return Ok(0) };
    let dur = pick(&c, &["ZDURATION"]);
    let outcome = pick(&c, &["ZOUTCOME", "ZCURRENTOUTCOME"]);
    let video = pick(&c, &["ZVIDEO"]);
    let mut jid = None;
    for want in jid_cols {
        if let Some(x) = pick(&c, &[want]) {
            jid = Some(x.to_string());
            break;
        }
    }
    let mut sel = vec!["m.Z_PK".to_string(), format!("m.{date}")];
    if let Some(x) = dur {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = outcome {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = video {
        sel.push(format!("m.{x}"));
    }
    if let Some(x) = &jid {
        sel.push(format!("m.{x}"));
    }
    let sql = format!("SELECT {} FROM {table} m ORDER BY m.{date}", sel.join(", "));
    // 参与者表：某些通话的对方 JID 只记录在 PARTICIPANT 子表。
    let part_table = format!("{table}PARTICIPANT");
    let mut parts: std::collections::HashMap<i64, String> = Default::default();
    if table_exists(conn, &part_table) {
        let pc = cols(conn, &part_table);
        if let (Some(fk), Some(j)) = (
            pick(&pc, &["Z1PARTICIPANTS", "Z4PARTICIPANTS", "ZCALLEVENT"]),
            pick(&pc, &["ZJIDSTRING"]),
        ) {
            if let Ok(mut s) = conn.prepare(&format!("SELECT {fk}, {j} FROM {part_table}")) {
                if let Ok(mut rs) = s.query([]) {
                    while let Ok(Some(r)) = rs.next() {
                        if let (Ok(k), Ok(Some(v))) =
                            (r.get::<_, i64>(0), r.get::<_, Option<String>>(1))
                        {
                            parts.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
    }
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let pk: i64 = r.get(0).unwrap_or_default();
        let date_v: Option<f64> = r.get(1).unwrap_or(None);
        let d: Option<f64> = if cnt > 2 {
            r.get(2).unwrap_or(None)
        } else {
            None
        };
        let out_v: Option<i64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        let video_v: Option<i64> = if cnt > 4 {
            r.get(4).unwrap_or(None)
        } else {
            None
        };
        let j: Option<String> = if cnt > 5 {
            r.get(5).unwrap_or(None)
        } else {
            None
        };
        let number = j
            .filter(|x| !x.is_empty())
            .or_else(|| parts.get(&pk).cloned())
            .unwrap_or_default();
        let kind = match out_v {
            Some(0) => "呼入",
            Some(2) => "未接",
            Some(3) => "呼出",
            _ => "通话",
        };
        let kind = if video_v == Some(1) {
            format!("{kind}（视频）")
        } else {
            kind.to_string()
        };
        let _ = joinable;
        run.record_values(
            "ios.calllog",
            vec![
                number.into(),
                Value::Nil,
                Value::from(d.unwrap_or(0.0)),
                ts_or_nil(apple(date_v.unwrap_or_default())),
                kind.into(),
                "WhatsApp".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 通讯录 ----------
pub fn contact(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    contact_conn(run, f, &conn)
}

pub fn contact_conn(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ABPerson") {
        run.warn_file(f, "SCHEMA", "无 ABPerson 表")?;
        return Ok(0);
    }
    let c = cols(conn, "ABPerson");
    let (Some(first), Some(last)) = (pick(&c, &["First"]), pick(&c, &["Last"])) else {
        return Ok(0);
    };
    let org = pick(&c, &["Organization"]);
    let note = pick(&c, &["Note"]);
    let created = pick(&c, &["Created", "CreationDate"]);
    let modified = pick(&c, &["ModificationDate", "Modification"]);
    let mut sel = vec!["p.ROWID".into(), format!("p.{first}"), format!("p.{last}")];
    for x in [org, note, created, modified].into_iter().flatten() {
        sel.push(format!("p.{x}"));
    }
    let sql = format!("SELECT {} FROM ABPerson p", sel.join(", "));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // ABMultiValue：property 3=电话 4=邮箱
    let mut values: std::collections::HashMap<i64, Vec<(i64, String)>> = Default::default();
    if table_exists(conn, "ABMultiValue") {
        if let Ok(mut s) = conn.prepare("SELECT record_id, property, value FROM ABMultiValue") {
            if let Ok(mut rs) = s.query([]) {
                while let Ok(Some(r)) = rs.next() {
                    if let (Ok(rid), Ok(Some(prop)), Ok(Some(val))) = (
                        r.get::<_, i64>(0),
                        r.get::<_, Option<i64>>(1),
                        r.get::<_, Option<String>>(2),
                    ) {
                        values.entry(rid).or_default().push((prop, val));
                    }
                }
            }
        }
    }
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let rid: i64 = r.get(0).unwrap_or_default();
        let f0: Option<String> = r.get(1).unwrap_or(None);
        let l0: Option<String> = r.get(2).unwrap_or(None);
        let mut i = 3usize;
        let mut nxt_s = |i: &mut usize| -> Option<String> {
            let v: Option<String> = if *i < cnt {
                r.get(*i).unwrap_or(None)
            } else {
                None
            };
            *i += 1;
            v
        };
        let org_v = nxt_s(&mut i);
        let note_v = nxt_s(&mut i);
        let created_v: Option<f64> = if i < cnt {
            r.get(i).unwrap_or(None)
        } else {
            None
        };
        i += 1;
        let modified_v: Option<f64> = if i < cnt {
            r.get(i).unwrap_or(None)
        } else {
            None
        };
        let (phones, emails): (Vec<String>, Vec<String>) = {
            let (mut p, mut e) = (Vec::new(), Vec::new());
            for (prop, v) in values.get(&rid).cloned().unwrap_or_default() {
                match prop {
                    3 => p.push(v),
                    4 => e.push(v),
                    _ => {}
                }
            }
            (p, e)
        };
        run.record_values(
            "ios.contact",
            vec![
                format!("{}{}", l0.unwrap_or_default(), f0.unwrap_or_default()).into(),
                phones.join("；").into(),
                emails.join("；").into(),
                org_v.unwrap_or_default().into(),
                note_v.unwrap_or_default().into(),
                ts_or_nil(apple(created_v.unwrap_or_default())),
                ts_or_nil(apple(modified_v.unwrap_or_default())),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 蓝牙配对/其他设备 ----------
pub fn bluetooth(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
    paired: bool,
) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let table = if paired {
        "PairedDevices"
    } else {
        "OtherDevices"
    };
    if !table_exists(&conn, table) {
        run.warn_file(f, "SCHEMA", format!("无 {table} 表"))?;
        return Ok(0);
    }
    let c = cols(&conn, table);
    let name = pick(&c, &["Name"]).unwrap_or("Name");
    let addr = pick(&c, &["Address", "ResolvedAddress"]);
    let seen = pick(&c, &["LastSeenTime"]);
    let last_conn = pick(&c, &["LastConnectionTime"]);
    let mut sel = vec![name.to_string()];
    for x in [addr, seen, last_conn].into_iter().flatten() {
        sel.push(x.to_string());
    }
    let sql = format!("SELECT {} FROM {table}", sel.join(", "));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let nm: Option<String> = r.get(0).unwrap_or(None);
        let ad: Option<String> = if cnt > 1 {
            r.get(1).unwrap_or(None)
        } else {
            None
        };
        let sn: Option<f64> = if cnt > 2 {
            r.get(2).unwrap_or(None)
        } else {
            None
        };
        let ct: Option<f64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        run.record_values(
            "ios.bt_device",
            vec![
                nm.unwrap_or_default().into(),
                ad.unwrap_or_default().into(),
                ts_or_nil(apple(sn.unwrap_or_default())),
                ts_or_nil(apple(ct.unwrap_or_default())),
                (if paired { "已配对" } else { "发现过" }).into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 账户 ----------
pub fn account(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    account_conn(run, f, &conn)
}

pub fn account_conn(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZACCOUNT") {
        run.warn_file(f, "SCHEMA", "无 ZACCOUNT 表")?;
        return Ok(0);
    }
    let c = cols(conn, "ZACCOUNT");
    let user = pick(&c, &["ZUSERNAME"]).unwrap_or("ZUSERNAME");
    let typ = pick(&c, &["ZACCOUNTTYPEDESCRIPTION"]);
    let desc = pick(&c, &["ZACCOUNTDESCRIPTION"]);
    let date = pick(&c, &["ZDATE"]);
    let bundle = pick(&c, &["ZOWNINGBUNDLEID"]);
    // ZACCOUNT.ZACCOUNTTYPE → ZACCOUNTTYPE.ZIDENTIFIER（com.apple.account.AppleAccount 等）
    let typed = pick(&c, &["ZACCOUNTTYPE"])
        .filter(|_| table_exists(conn, "ZACCOUNTTYPE"))
        .and_then(|fk| {
            let tc = cols(conn, "ZACCOUNTTYPE");
            pick(&tc, &["ZIDENTIFIER"]).map(|id| (fk.to_string(), id.to_string()))
        });
    let mut sel = vec![format!("a.{user}")];
    for x in [typ, desc, date, bundle].into_iter().flatten() {
        sel.push(format!("a.{x}"));
    }
    if let Some((_, id)) = &typed {
        sel.push(format!("t.{id} AS tid"));
    }
    let join = if typed.is_some() {
        " LEFT JOIN ZACCOUNTTYPE t ON t.Z_PK = a.ZACCOUNTTYPE"
    } else {
        ""
    };
    let sql = format!("SELECT {} FROM ZACCOUNT a{join}", sel.join(", "));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let u: Option<String> = r.get(0).unwrap_or(None);
        let t: Option<String> = if cnt > 1 {
            r.get(1).unwrap_or(None)
        } else {
            None
        };
        let d: Option<String> = if cnt > 2 {
            r.get(2).unwrap_or(None)
        } else {
            None
        };
        let dt: Option<f64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        let b: Option<String> = if cnt > 4 {
            r.get(4).unwrap_or(None)
        } else {
            None
        };
        let tid: Option<String> = if cnt > 5 {
            r.get(5).unwrap_or(None)
        } else {
            None
        };
        run.record_values(
            "ios.account",
            vec![
                u.unwrap_or_default().into(),
                tid.or(t).unwrap_or_default().into(),
                d.unwrap_or_default().into(),
                ts_or_nil(apple(dt.unwrap_or_default())),
                b.unwrap_or_default().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- WiFi ----------
/// WiFi 连接记录。时间口径只取"加入"语义字段：private-mac `lastJoined`、
/// known-networks `JoinedBySystemAt`/`JoinedByUserAt`/`prevJoined`、
/// BSSList `LastAssociatedAt`、`FirstJoinWithNewMacTimestamp`；
/// `LastDiscoveredAt`/`UpdatedAt` 是发现与记录更新时间，不是连接，不参与。
/// BSSID 取最后一次关联的 AP（BSSList 中 LastAssociatedAt 最大者），
/// 缺失时回退顶层/`__OSSpecific__` 的 BSSID。
/// SystemConfiguration 备份的 `List of known networks` 是运营商配置下发的已知
/// 网络（无连接时间戳），不代表连接过，不采集。
pub fn wifi(
    run: &Run,
    f: &FileRef,
    nets: &mut std::collections::HashMap<String, WifiNet>,
) -> lovelycarve_plugin::Result<usize> {
    let bytes = f.read_all(64 << 20)?;
    let Some(v) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", "wifi plist 解析失败")?;
        return Ok(0);
    };
    let mut nets_list: Vec<WifiNet> = Vec::new();
    if let Some(d) = v.as_dictionary() {
        if let Some(l) = d.get("ListOfPreferredNetworks") {
            if let Some(map) = l.as_dictionary() {
                for (ssid, info) in map {
                    nets_list.push(wifi_entry(Some(ssid.clone()), info.as_dictionary()));
                }
            } else if let Some(arr) = l.as_array() {
                for info in arr {
                    nets_list.push(wifi_entry(None, info.as_dictionary()));
                }
            }
        }
        if let Some(arr) = d
            .get("List of scanned networks with private mac")
            .and_then(|x| x.as_array())
        {
            for info in arr {
                // 仅采集真正关联过的条目（有 lastJoined / 成功关联标记）。
                let dd = info.as_dictionary();
                let joined = dd
                    .and_then(|x| x.get("lastJoined"))
                    .and_then(plist::Value::as_date)
                    .is_some();
                let assoc = dd
                    .and_then(|x| x.get("privateMacSuccessfulAssocAtleastOnce"))
                    .and_then(plist::Value::as_boolean)
                    .unwrap_or(false);
                if joined || assoc {
                    nets_list.push(wifi_entry(None, dd));
                }
            }
        }
        for (k, info) in d {
            if let Some(rest) = k.strip_prefix("wifi.network.ssid.") {
                let mut n = wifi_entry(None, info.as_dictionary());
                if n.ssid.is_empty() {
                    n.ssid = rest.to_string();
                }
                nets_list.push(n);
            }
        }
    }
    // 同一 SSID 会同时出现在 known-networks 与 private-mac 两个文件，
    // 合并进调用方持有的跨文件集合，全部文件解析完后统一发射。
    let mut n = 0usize;
    for mut net in nets_list.into_iter().filter(|n| !n.ssid.is_empty()) {
        let key = net.ssid.to_lowercase();
        let e = nets.entry(key).or_insert_with(|| net.clone());
        e.joins.extend(net.joins.drain(..));
        if net.bssid_rank > e.bssid_rank || (e.bssid.is_empty() && !net.bssid.is_empty()) {
            e.bssid = net.bssid;
            e.bssid_rank = net.bssid_rank;
        }
        if e.security.is_empty() {
            e.security = net.security;
        }
        if e.auto_join.is_empty() {
            e.auto_join = net.auto_join;
        }
        if net.hotspot {
            e.hotspot = true;
        }
        n += 1;
    }
    if n == 0 {
        run.warn_file(f, "WIFI_EMPTY", "wifi plist 未解析出网络")?;
    }
    Ok(n)
}

/// 发射合并后的 WiFi 记录（每个 SSID 一条；密码由钥匙串导出关联）。
pub fn wifi_emit(
    run: &Run,
    nets: &std::collections::HashMap<String, WifiNet>,
    secrets: &std::collections::HashMap<String, String>,
    f: &FileRef,
) -> lovelycarve_plugin::Result<usize> {
    let source = f.name().to_string();
    let mut n = 0usize;
    for w in nets.values() {
        let (first_joined, last_joined) = ts_min_max(&w.joins);
        let pwd = secrets
            .get(&w.ssid.to_lowercase())
            .cloned()
            .unwrap_or_default();
        run.record_values(
            "ios.wifi",
            vec![
                w.ssid.clone().into(),
                pwd.into(),
                w.security.clone().into(),
                w.auto_join.clone().into(),
                ts_or_nil(last_joined),
                ts_or_nil(first_joined),
                w.bssid.clone().into(),
                (if w.hotspot { "是" } else { "" }).into(),
                source.clone().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 时间串（同为 +08:00 RFC3339）按字典序即时间序取最值。
fn ts_min_max(v: &[Timestamp]) -> (Option<Timestamp>, Option<Timestamp>) {
    let mut min: Option<&Timestamp> = None;
    let mut max: Option<&Timestamp> = None;
    for t in v {
        if min.map(|m| t.as_str() < m.as_str()).unwrap_or(true) {
            min = Some(t);
        }
        if max.map(|m| t.as_str() > m.as_str()).unwrap_or(true) {
            max = Some(t);
        }
    }
    (min.cloned(), max.cloned())
}

#[derive(Clone, Default)]
pub struct WifiNet {
    ssid: String,
    security: String,
    auto_join: String,
    hotspot: bool,
    /// 全部"加入"事件（UTC+8），合并后取最值。
    joins: Vec<Timestamp>,
    /// 最后关联的 AP。
    bssid: String,
    /// BSSID 可信度：2=带关联时间戳的 BSSList 条目，1=顶层/__OSSpecific__ 键。
    bssid_rank: u8,
}

fn wifi_entry(fallback_ssid: Option<String>, d: Option<&plist::Dictionary>) -> WifiNet {
    let mut w = WifiNet {
        ssid: fallback_ssid.unwrap_or_default(),
        ..Default::default()
    };
    let Some(d) = d else { return w };
    let s = |k: &str| {
        d.get(k)
            .and_then(plist::Value::as_string)
            .map(|x| x.to_string())
    };
    if let Some(raw) = d.get("SSID").and_then(plist::Value::as_data) {
        w.ssid = String::from_utf8_lossy(raw)
            .trim_end_matches('\0')
            .to_string();
    }
    if w.ssid.is_empty() {
        if let Some(x) = s("SSID_STR").or_else(|| s("SSIDString")) {
            w.ssid = x;
        }
    }
    // 加入事件（连接语义）。
    let dt = |k: &str| {
        d.get(k)
            .and_then(plist::Value::as_date)
            .and_then(|x| plist_date(&x))
    };
    for k in [
        "lastJoined",
        "JoinedBySystemAt",
        "JoinedByUserAt",
        "prevJoined",
        "FirstJoinWithNewMacTimestamp",
    ] {
        if let Some(t) = dt(k) {
            w.joins.push(t);
        }
    }
    // BSSList：逐 AP 关联记录，BSSID 取最后关联的那条。
    let mut last_assoc: Option<(Timestamp, String)> = None;
    if let Some(list) = d.get("BSSList").and_then(plist::Value::as_array) {
        for b in list {
            let Some(bd) = b.as_dictionary() else {
                continue;
            };
            let Some(t) = bd
                .get("LastAssociatedAt")
                .and_then(plist::Value::as_date)
                .and_then(|x| plist_date(&x))
            else {
                continue;
            };
            w.joins.push(t.clone());
            let Some(bssid) = bd.get("BSSID").and_then(plist::Value::as_string) else {
                continue;
            };
            let better = match &last_assoc {
                None => true,
                Some((pt, _)) => t.as_str() > pt.as_str(),
            };
            if better {
                last_assoc = Some((t, bssid.to_string()));
            }
        }
    }
    if let Some((_, bssid)) = last_assoc {
        w.bssid = bssid;
        w.bssid_rank = 2;
    }
    if w.bssid.is_empty() {
        if let Some(x) = s("BSSID") {
            w.bssid = x;
            w.bssid_rank = 1;
        }
    }
    if w.bssid.is_empty() {
        if let Some(os) = d
            .get("__OSSpecific__")
            .and_then(plist::Value::as_dictionary)
        {
            if let Some(x) = os.get("BSSID").and_then(plist::Value::as_string) {
                w.bssid = x.to_string();
                w.bssid_rank = 1;
            }
        }
    }
    w.security = s("SupportedSecurityTypes")
        .or_else(|| s("SecurityMode"))
        .or_else(
            || match d.get("SecurityMode").and_then(|x| x.as_signed_integer()) {
                Some(3) | Some(5) => Some("WPA/WPA2".into()),
                Some(11) => Some("WPA3".into()),
                Some(1) => Some("WEP".into()),
                Some(0) => Some("开放".into()),
                _ => None,
            },
        )
        .or_else(|| {
            (d.get("IsOpenNetwork").and_then(plist::Value::as_boolean) == Some(true))
                .then(|| "开放".to_string())
        })
        .unwrap_or_default();
    w.auto_join = match d.get("AutoJoin").and_then(plist::Value::as_boolean) {
        Some(true) => "是".into(),
        Some(false) => "否".into(),
        None => match d.get("enabled").and_then(plist::Value::as_boolean) {
            Some(true) => "是".into(),
            Some(false) => "否".into(),
            None => String::new(),
        },
    };
    w.hotspot = d
        .get("PersonalHotspot")
        .and_then(plist::Value::as_boolean)
        .unwrap_or(false)
        || d.get("NetworkOriginatorName")
            .and_then(plist::Value::as_string)
            == Some("Personal Hotspot");
    w
}

// ---------- 相册 / 笔记（防御式） ----------
pub fn photo(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ZASSET") {
        return Ok(0);
    }
    let c = cols(&conn, "ZASSET");
    let pick_or = |want: &[&str], def: &'static str| pick(&c, want).unwrap_or(def).to_string();
    let file = pick_or(&["ZFILENAME"], "ZFILENAME");
    let mut exprs = vec![
        format!("z.{file} AS fname"),
        format!(
            "z.{} AS created",
            pick(&c, &["ZDATECREATED"]).unwrap_or("ZDATECREATED")
        ),
    ];
    let mut add = |alias: &str, name: &str, want: &[&str]| {
        if let Some(col) = pick(&c, want) {
            exprs.push(format!("z.{col} AS {alias}"));
        } else {
            exprs.push(format!("NULL AS {alias}"));
            let _ = name;
        }
    };
    add("kind", "kind", &["ZKIND"]);
    add("dur", "dur", &["ZDURATION"]);
    add("added", "added", &["ZADDEDDATE"]);
    add("lat", "lat", &["ZLATITUDE"]);
    add("lng", "lng", &["ZLONGITUDE"]);
    add("orient", "orient", &["ZORIENTATION"]);
    add("hidden", "hidden", &["ZHIDDEN"]);
    add("subtype", "subtype", &["ZKINDSUBTYPE"]);
    add("cloudguid", "cloudguid", &["ZCLOUDASSETGUID"]);
    add("cloudstate", "cloudstate", &["ZCLOUDLOCALSTATE"]);
    add("directory", "directory", &["ZDIRECTORY"]);
    add("shared", "shared", &["ZLASTSHAREDDATE"]);
    let has_attr = table_exists(&conn, "ZADDITIONALASSETATTRIBUTES")
        && cols(&conn, "ZADDITIONALASSETATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ac = if has_attr {
        cols(&conn, "ZADDITIONALASSETATTRIBUTES")
    } else {
        Vec::new()
    };
    let aexpr = |alias: &str, want: &[&str]| -> String {
        match pick(&ac, want) {
            Some(col) if has_attr => format!("a.{col} AS {alias}"),
            _ => format!("NULL AS {alias}"),
        }
    };
    let joins = if has_attr {
        " LEFT JOIN ZADDITIONALASSETATTRIBUTES a ON a.ZASSET = z.Z_PK"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {} FROM ZASSET z{joins}",
        exprs.join(", "),
        aexpr("orig_name", &["ZORIGINALFILENAME"]),
        aexpr("src_bundle", &["ZIMPORTEDBYBUNDLEIDENTIFIER"]),
        aexpr("src_name", &["ZIMPORTEDBYDISPLAYNAME"]),
        aexpr("src_id", &["ZIMPORTEDBY"]),
        aexpr("share_count", &["ZSHARECOUNT"]),
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("相册查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let cnt = stmt.column_count();
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let base = exprs.len();
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let get_s = |i: usize| -> Option<String> {
            if i < cnt {
                r.get(i).unwrap_or(None)
            } else {
                None
            }
        };
        let get_f = |i: usize| -> Option<f64> {
            if i < cnt {
                r.get(i).unwrap_or(None)
            } else {
                None
            }
        };
        let get_i = |i: usize| -> Option<i64> {
            if i < cnt {
                r.get(i).unwrap_or(None)
            } else {
                None
            }
        };
        let fname = get_s(0).unwrap_or_default();
        let created = get_f(1).unwrap_or_default();
        let kind = get_i(2).unwrap_or(0);
        let dur = get_f(3).unwrap_or_default();
        let added = get_f(4).unwrap_or_default();
        let lat = get_f(5).unwrap_or(f64::NAN);
        let lng = get_f(6).unwrap_or(f64::NAN);
        let orient = get_i(7).unwrap_or(0);
        let hidden = get_i(8).unwrap_or(0);
        let subtype = get_i(9).unwrap_or(0);
        let cloud_guid = get_s(10).unwrap_or_default();
        let cloud_state = get_i(11).unwrap_or(0);
        let directory = get_s(12).unwrap_or_default();
        let shared = get_f(13).unwrap_or_default();
        let orig_name = get_s(base).unwrap_or_default();
        let src_bundle = get_s(base + 1).unwrap_or_default();
        let src_name = get_s(base + 2).unwrap_or_default();
        let src_id = get_i(base + 3).unwrap_or(0);
        let share_count = get_i(base + 4).unwrap_or(0);
        let media_type = match kind {
            1 => "视频",
            2 => "音频",
            _ => "照片",
        };
        let cloud = if !cloud_guid.is_empty() {
            "iCloud 已同步"
        } else if cloud_state > 0 {
            "iCloud 占位"
        } else {
            ""
        };
        let source_app = if !src_name.is_empty() {
            src_name.clone()
        } else if !src_bundle.is_empty() {
            src_bundle.clone()
        } else if src_id == 8 {
            // Photos 的 ZIMPORTEDBY=8 为系统截图（SpringBoard）来源
            "系统截图（SpringBoard）".to_string()
        } else if src_id != 0 {
            format!("导入源 {src_id}")
        } else {
            String::new()
        };
        let (lat_v, lng_v) = if lat.is_finite() && lat.abs() <= 90.0 {
            (lat, lng)
        } else {
            (f64::NAN, f64::NAN)
        };
        run.record_values(
            "ios.photo",
            vec![
                fname.clone().into(),
                media_type.into(),
                Value::from(if dur.is_finite() { dur } else { 0.0 }),
                ts_or_nil(apple(created)),
                ts_or_nil(apple(added)),
                orig_name.into(),
                source_app.into(),
                cloud.into(),
                if lat_v.is_finite() {
                    Value::from(lat_v)
                } else {
                    Value::Nil
                },
                if lng_v.is_finite() {
                    Value::from(lng_v)
                } else {
                    Value::Nil
                },
                orient.into(),
                (if hidden == 1 { "是" } else { "" }).into(),
                subtype.into(),
                share_count.into(),
                ts_or_nil(apple(shared)),
                directory.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    n += albums(run, f, &conn)?;
    Ok(n)
}

/// 相簿清单：iOS 17 为 ZGENERICALBUM，旧版为 ZALBUM；成员计数尽力关联。
fn albums(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let table = ["ZGENERICALBUM", "ZALBUM"]
        .into_iter()
        .find(|t| table_exists(conn, t));
    let Some(table) = table else { return Ok(0) };
    let c = cols(conn, table);
    let Some(title) = pick(&c, &["ZTITLE", "ZTITLESTRING", "ZNAME"]) else {
        return Ok(0);
    };
    let kind = pick(&c, &["ZKINDSUBTYPE", "ZCLOUDALBUMSUBTYPE", "ZKIND"]);
    let cloud = pick(&c, &["ZCLOUDALBUMUUID", "ZCLOUDFEEDALBUMUUID"]);
    let pk = pick(&c, &["Z_PK"]);
    let mut sel = vec![format!("[{title}]")];
    if let Some(k) = kind {
        sel.push(format!("[{k}]"));
    }
    if let Some(cl) = cloud {
        sel.push(format!("[{cl}]"));
    }
    if let Some(pk) = pk {
        sel.push(format!("[{pk}]"));
    }
    let sql = format!(
        "SELECT {} FROM [{table}] ORDER BY ROWID LIMIT 500",
        sel.join(", ")
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let cnt = stmt.column_count();
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    // 成员计数：ZASSETALBUM(ZALBUM) 存在时按相簿分组计数。
    let mut counts: std::collections::HashMap<i64, i64> = Default::default();
    if table_exists(conn, "ZASSETALBUM") {
        if let Ok(mut st) =
            conn.prepare("SELECT [ZALBUM], COUNT(*) FROM [ZASSETALBUM] GROUP BY [ZALBUM]")
        {
            if let Ok(mut rs) = st.query([]) {
                while let Ok(Some(r)) = rs.next() {
                    if let (Ok(k), Ok(v)) = (r.get::<_, i64>(0), r.get::<_, i64>(1)) {
                        counts.insert(k, v);
                    }
                }
            }
        }
    }
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let name: Option<String> = r.get(0).unwrap_or(None);
        let Some(name) = name else { continue };
        if name.trim().is_empty() {
            continue;
        }
        let kind_v = if cnt > 1 {
            r.get::<_, Option<i64>>(1).unwrap_or(None)
        } else {
            None
        };
        let cloud_v = if cnt > 2 {
            r.get::<_, Option<String>>(2).unwrap_or(None)
        } else {
            None
        };
        let pk_v: Option<i64> = if cnt > 3 {
            r.get(3).unwrap_or(None)
        } else {
            None
        };
        let kind_text = match kind_v {
            Some(1) => "智能相簿",
            Some(2) => "用户相簿",
            Some(3) => "共享相簿",
            Some(k) => {
                let _ = k;
                "相簿"
            }
            None => "",
        };
        let mut note = String::new();
        if cloud_v
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            note = "iCloud 同步相簿".into();
        }
        run.record_values(
            "ios.photo_album",
            vec![
                name.into(),
                kind_text.into(),
                Value::from(pk_v.and_then(|k| counts.get(&k)).copied().unwrap_or(0)),
                note.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

fn emit_note(
    run: &Run,
    f: &FileRef,
    title: String,
    created: Option<Timestamp>,
    edited: Option<Timestamp>,
    note_detail: Option<String>,
) -> lovelycarve_plugin::Result<()> {
    let src = match note_detail {
        Some(detail) if !detail.is_empty() => crate::util::node_source(f).note(detail),
        _ => crate::util::node_source(f),
    };
    run.record_values(
        "ios.note",
        vec![title.into(), ts_or_nil(created), ts_or_nil(edited)],
    )
    .source(src)
    .emit()
}

pub fn note(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    note_conn(run, f, &conn)
}

pub fn note_conn(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if table_exists(conn, "ZICNOTE") {
        let c = cols(conn, "ZICNOTE");
        let Some(title) = pick(&c, &["ZTITLE"]) else {
            return Ok(0);
        };
        let created = pick(&c, &["ZDATECREATED"]);
        let edited = pick(&c, &["ZDATEEDITED"]);
        let mut sel = vec![title.to_string()];
        for x in [created, edited].into_iter().flatten() {
            sel.push(x.to_string());
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM ZICNOTE", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => return Ok(0),
        };
        let cnt = stmt.column_count();
        let mut rows = stmt
            .query([])
            .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        let mut n = 0usize;
        while let Ok(Some(r)) = rows.next() {
            let t: Option<String> = r.get(0).unwrap_or(None);
            let cr: Option<f64> = if cnt > 1 {
                r.get(1).unwrap_or(None)
            } else {
                None
            };
            let ed: Option<f64> = if cnt > 2 {
                r.get(2).unwrap_or(None)
            } else {
                None
            };
            emit_note(
                run,
                f,
                t.unwrap_or_default(),
                apple(cr.unwrap_or_default()),
                apple(ed.unwrap_or_default()),
                None,
            )?;
            n += 1;
        }
        return Ok(n);
    }
    if table_exists(conn, "ZNOTE") {
        let nc = cols(conn, "ZNOTE");
        let has_body = table_exists(conn, "ZNOTEBODY") && pick(&nc, &["ZBODY"]).is_some();
        let pk_col = pick(&nc, &["Z_PK"]).unwrap_or("rowid");
        let title_col = pick(&nc, &["ZTITLE"]);
        let summary_col = pick(&nc, &["ZSUMMARY"]);
        let created_col = pick(&nc, &["ZCREATIONDATE"]);
        let edited_col = pick(&nc, &["ZMODIFICATIONDATE"]);
        let deleted_col = pick(&nc, &["ZDELETEDFLAG"]);
        let body_fk = pick(&nc, &["ZBODY"]);
        let content_expr = if has_body {
            let bc = cols(conn, "ZNOTEBODY");
            pick(&bc, &["ZCONTENT"])
                .map(|c| format!("b.[{c}]"))
                .unwrap_or_else(|| "NULL".into())
        } else {
            "NULL".into()
        };
        let col_or_null = |c: Option<&str>| {
            c.map(|name| format!("n.[{name}]"))
                .unwrap_or_else(|| "NULL".into())
        };
        let join = match (has_body, body_fk) {
            (true, Some(fk)) => format!("LEFT JOIN [ZNOTEBODY] b ON n.[{fk}] = b.[Z_PK]"),
            _ => String::new(),
        };
        let sql = format!(
            "SELECT n.[{pk_col}], {title}, {summary}, {created}, {edited}, {deleted}, {content_expr} FROM [ZNOTE] n {join}",
            title = col_or_null(title_col),
            summary = col_or_null(summary_col),
            created = col_or_null(created_col),
            edited = col_or_null(edited_col),
            deleted = col_or_null(deleted_col),
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(0),
        };
        let mut rows = stmt
            .query([])
            .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        let mut n = 0usize;
        while let Ok(Some(r)) = rows.next() {
            let pk: i64 = r.get(0).unwrap_or(0);
            let title: Option<String> = r.get(1).unwrap_or(None);
            let summary: Option<String> = r.get(2).unwrap_or(None);
            let cr: Option<f64> = r.get(3).unwrap_or(None);
            let ed: Option<f64> = r.get(4).unwrap_or(None);
            let del: Option<i64> = r.get(5).unwrap_or(None);
            let content: Option<String> = r.get(6).unwrap_or(None);

            let raw_title = title.as_deref().unwrap_or("").trim();
            let raw_summary = summary.as_deref().unwrap_or("").trim();
            let raw_content = content.as_deref().unwrap_or("").trim();
            let display_base = if !raw_title.is_empty() {
                raw_title.to_string()
            } else if !raw_summary.is_empty() {
                raw_summary.to_string()
            } else {
                raw_content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect()
            };
            let full_title = if del == Some(1) {
                format!("[已删除] {display_base}")
            } else {
                display_base
            };
            let body_preview: String = if !raw_content.is_empty() {
                raw_content
            } else {
                raw_summary
            }
            .chars()
            .take(400)
            .collect();
            let detail = if body_preview.is_empty() {
                format!("notes.sqlite ZNOTE Z_PK={pk}")
            } else {
                format!("notes.sqlite ZNOTE Z_PK={pk}: {body_preview}")
            };
            emit_note(
                run,
                f,
                full_title,
                cr.and_then(apple),
                ed.and_then(apple),
                Some(detail),
            )?;
            n += 1;
        }
        return Ok(n);
    }
    Ok(0)
}

// ---------- Safari 浏览器历史 ----------
pub fn web_history(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    web_history_conn(run, f, &conn)
}

pub fn web_history_conn(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "history_items") || !table_exists(conn, "history_visits") {
        run.warn_file(f, "SCHEMA", "无 history_items/history_visits 表")?;
        return Ok(0);
    }
    // iOS 15+ 把 title 移到 history_visits，旧库仍在 history_items。
    let ic = cols(conn, "history_items");
    let vc = cols(conn, "history_visits");
    let title_expr = if vc.iter().any(|c| c.eq_ignore_ascii_case("title")) {
        "v.title"
    } else if ic.iter().any(|c| c.eq_ignore_ascii_case("title")) {
        "h.title"
    } else {
        "''"
    };
    let sql = format!(
        "SELECT h.url, COALESCE({title_expr}, ''), v.visit_time, \
         (SELECT COUNT(*) FROM history_visits v2 WHERE v2.history_item = v.history_item) \
         FROM history_items h JOIN history_visits v ON v.history_item = h.ROWID ORDER BY v.visit_time"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("历史查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut rows = stmt
        .query([])
        .map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    let mut seen_kw: std::collections::HashSet<String> = Default::default();
    while let Ok(Some(r)) = rows.next() {
        let url: Option<String> = r.get(0).unwrap_or(None);
        let title: Option<String> = r.get(1).unwrap_or(None);
        let vt: Option<f64> = r.get(2).unwrap_or(None);
        let cnt: i64 = r.get(3).unwrap_or(1);
        // visit_time：Apple epoch 秒
        let ts = vt
            .filter(|v| v.abs() > 1e14)
            .map(|v| ts_plus8(APPLE_EPOCH + (v / 1e9) as i64))
            .or_else(|| vt.map(|v| ts_plus8(APPLE_EPOCH + v as i64)))
            .flatten();
        let url_v = url.clone().unwrap_or_default();
        run.record_values(
            "ios.web_history",
            vec![
                ts.clone()
                    .map(|t| Value::from(t.as_str().to_string()))
                    .unwrap_or(Value::Nil),
                url_v.clone().into(),
                title.unwrap_or_default().into(),
                cnt.into(),
                "Safari".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
        if let Some(kw) = search_keyword(&url_v) {
            if seen_kw.insert(kw.to_lowercase()) {
                run.record_values(
                    "ios.app_search",
                    vec![
                        "Safari".into(),
                        kw.into(),
                        ts.map(|t| Value::from(t.as_str().to_string()))
                            .unwrap_or(Value::Nil),
                    ],
                )
                .source(crate::util::node_source(f))
                .emit()?;
            }
        }
    }
    Ok(n)
}

/// 从 URL 查询参数中提取搜索关键词（通用搜索引擎参数名）。
fn search_keyword(url: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if matches!(
            k.to_ascii_lowercase().as_str(),
            "q" | "query" | "search" | "text" | "wd" | "keyword" | "k"
        ) {
            let decoded = url_decode(v);
            if !decoded.trim().is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Photos 导入来源的 (bundle, 显示名) 映射（系统在导入时记录的桌面显示名，
/// 如 com.uazoo.ssc → "Discreet"）。供应用清单补全包名对不上的显示名。
pub fn import_name_map(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
) -> lovelycarve_plugin::Result<Vec<(String, String)>> {
    let (_t, conn) = match open_db(f, sides) {
        Ok(v) => v,
        Err(e) => return Err(lovelycarve_plugin::Error::new("DB_OPEN", e)),
    };
    if !table_exists(&conn, "ZADDITIONALASSETATTRIBUTES") {
        return Ok(Vec::new());
    }
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT a.[ZIMPORTEDBYBUNDLEIDENTIFIER], a.[ZIMPORTEDBYDISPLAYNAME] \
         FROM [ZADDITIONALASSETATTRIBUTES] a \
         WHERE a.[ZIMPORTEDBYBUNDLEIDENTIFIER] IS NOT NULL AND a.[ZIMPORTEDBYDISPLAYNAME] IS NOT NULL LIMIT 500",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(bid)), Ok(Some(name))) = (r.get::<_, Option<String>>(0), r.get::<_, Option<String>>(1)) {
                    let bid = bid.trim().to_string();
                    let name = name.trim().to_string();
                    if !bid.is_empty() && !name.is_empty() && seen.insert(bid.clone()) {
                        out.push((bid, name));
                    }
                }
            }
        }
    }
    Ok(out)
}
