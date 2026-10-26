//! iOS 备份业务数据库解析：短信/通话/通讯录/蓝牙配对/账户/WiFi（+相册/笔记防御式）。
//! 输入文件名与表列口径依据标准 iOS 备份结构；时间列为 Apple epoch（2001-01-01），
//! iOS 16 起部分库改用纳秒整数，按量级自动判别。

use crate::util::{ts_plus8, APPLE_EPOCH};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use rusqlite::{Connection, OpenFlags};
use std::time::{SystemTime, UNIX_EPOCH};

/// Apple epoch 秒/纳秒 → UTC+8 Timestamp。
fn apple(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 { (v / 1e9) as i64 } else { v as i64 };
    ts_plus8(APPLE_EPOCH + secs)
}

/// plist::Date（绝对时间）→ UTC+8 Timestamp。
fn plist_date(d: &plist::Date) -> Option<Timestamp> {
    let unix = SystemTime::from(*d).duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    ts_plus8(unix)
}

fn ts_or_nil(t: Option<Timestamp>) -> Value {
    t.map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil)
}

/// 拷贝检材文件到临时目录并以只读打开（含同名 -wal/-shm）。
pub fn open_db(f: &FileRef, sidecars: &[FileRef]) -> Result<(tempfile::TempDir, Connection), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = tmp.path().join("db.sqlite");
    let mut buf = Vec::with_capacity(f.size() as usize);
    let mut off = 0u64;
    while off < f.size() {
        let n = ((f.size() - off).min(1 << 20)) as usize;
        buf.extend_from_slice(&f.read_at(off, n).unwrap_or_default());
        off += n as u64;
    }
    std::fs::write(&path, &buf).map_err(|e| e.to_string())?;
    for suffix in ["-wal", "-shm"] {
        let want = format!("{}{suffix}", f.name());
        if let Some(s) = sidecars.iter().find(|s| s.name() == want) {
            let mut b = Vec::with_capacity(s.size() as usize);
            let mut o = 0u64;
            while o < s.size() {
                let n = ((s.size() - o).min(1 << 20)) as usize;
                b.extend_from_slice(&s.read_at(o, n).unwrap_or_default());
                o += n as u64;
            }
            std::fs::write(tmp.path().join(format!("db.sqlite{suffix}")), b).ok();
        }
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    Ok((tmp, conn))
}

fn err(e: impl std::fmt::Display) -> lovelycarve_plugin::Error {
    lovelycarve_plugin::Error::new("SQL", e.to_string())
}

fn table_exists(conn: &Connection, t: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [t], |_| Ok(())).is_ok()
}

fn cols(conn: &Connection, t: &str) -> Vec<String> {
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

fn pick<'a>(available: &'a [String], want: &[&str]) -> Option<&'a str> {
    want.iter()
        .find_map(|w| available.iter().find(|c| c.eq_ignore_ascii_case(w)).map(|c| c.as_str()))
}

// ---------- 短信 ----------
pub fn sms(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "message") {
        run.warn_file(f, "SCHEMA", "无 message 表")?;
        return Ok(0);
    }
    let c = cols(&conn, "message");
    let (Some(text), Some(date)) = (pick(&c, &["text"]), pick(&c, &["date"])) else {
        run.warn_file(f, "SCHEMA", "message 表缺 text/date 列")?;
        return Ok(0);
    };
    let handle_id = pick(&c, &["handle_id"]);
    let from_me = pick(&c, &["is_from_me"]);
    let is_read = pick(&c, &["is_read"]);
    let service = pick(&c, &["service"]);
    let subject = pick(&c, &["subject"]);
    let mut sel = vec![format!("m.ROWID"), format!("m.{text}"), format!("m.{date}")];
    if let Some(x) = from_me { sel.push(format!("m.{x}")); }
    if let Some(x) = is_read { sel.push(format!("m.{x}")); }
    if let Some(x) = service { sel.push(format!("m.{x}")); }
    if let Some(x) = subject { sel.push(format!("m.{x}")); }
    let join = handle_id.map(|h| format!(" LEFT JOIN handle h ON h.ROWID = m.{h}")).unwrap_or_default();
    let hsel = if handle_id.is_some() { ", h.id" } else { ", NULL" };
    let sql = format!("SELECT {}{} FROM message m{join} ORDER BY m.{date}", sel.join(", "), hsel);
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("sms 查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // 附件
    let mut att: std::collections::HashMap<i64, String> = Default::default();
    if table_exists(&conn, "message_attachment_join") && table_exists(&conn, "attachment") {
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
        let from_me_v: Option<i64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
        let read_v: Option<i64> = if cnt > 4 { r.get(4).unwrap_or(None) } else { None };
        let service_v: Option<String> = if cnt > 5 { r.get(5).unwrap_or(None) } else { None };
        let subject_v: Option<String> = if cnt > 6 { r.get(6).unwrap_or(None) } else { None };
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
                (if from_me_v == Some(1) { "发送" } else { "接收" }).into(),
                (if read_v == Some(1) { "已读" } else { "未读" }).into(),
                service_v.unwrap_or_default().into(),
                att.get(&rowid).cloned().unwrap_or_default().into(),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 通话 ----------
pub fn calllog(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ZCALLRECORD") {
        run.warn_file(f, "SCHEMA", "无 ZCALLRECORD 表")?;
        return Ok(0);
    }
    let c = cols(&conn, "ZCALLRECORD");
    let Some(addr) = pick(&c, &["ZADDRESS"]) else { return Ok(0); };
    let date = pick(&c, &["ZDATE", "ZDATEORIGINATION"]).unwrap_or("ZDATE");
    let dur = pick(&c, &["ZDURATION"]);
    let orig = pick(&c, &["ZORIGINATED"]);
    let ans = pick(&c, &["ZANSWERED"]);
    let mut sel = vec![format!("m.{addr}"), format!("m.{date}")];
    if let Some(x) = dur { sel.push(format!("m.{x}")); }
    if let Some(x) = orig { sel.push(format!("m.{x}")); }
    if let Some(x) = ans { sel.push(format!("m.{x}")); }
    let sql = format!("SELECT {} FROM ZCALLRECORD m ORDER BY m.{date}", sel.join(", "));
    let mut stmt = conn.prepare(&sql).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let number: Option<String> = r.get(0).unwrap_or(None);
        let date_v: Option<f64> = r.get(1).unwrap_or(None);
        let d: Option<f64> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
        let o: Option<i64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
        let a: Option<i64> = if cnt > 4 { r.get(4).unwrap_or(None) } else { None };
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
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 通讯录 ----------
pub fn contact(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ABPerson") {
        run.warn_file(f, "SCHEMA", "无 ABPerson 表")?;
        return Ok(0);
    }
    let c = cols(&conn, "ABPerson");
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
    let mut stmt = conn.prepare(&sql).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    // ABMultiValue：property 3=电话 4=邮箱
    let mut values: std::collections::HashMap<i64, Vec<(i64, String)>> = Default::default();
    if table_exists(&conn, "ABMultiValue") {
        if let Ok(mut s) = conn.prepare("SELECT record_id, property, value FROM ABMultiValue") {
            if let Ok(mut rs) = s.query([]) {
                while let Ok(Some(r)) = rs.next() {
                    if let (Ok(rid), Ok(Some(prop)), Ok(Some(val))) =
                        (r.get::<_, i64>(0), r.get::<_, Option<i64>>(1), r.get::<_, Option<String>>(2))
                    {
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
            let v: Option<String> = if *i < cnt { r.get(*i).unwrap_or(None) } else { None };
            *i += 1;
            v
        };
        let org_v = nxt_s(&mut i);
        let note_v = nxt_s(&mut i);
        let created_v: Option<f64> = if i < cnt { r.get(i).unwrap_or(None) } else { None };
        i += 1;
        let modified_v: Option<f64> = if i < cnt { r.get(i).unwrap_or(None) } else { None };
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
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 蓝牙配对/其他设备 ----------
pub fn bluetooth(run: &Run, f: &FileRef, sides: &[FileRef], paired: bool) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let table = if paired { "PairedDevices" } else { "OtherDevices" };
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
    let mut stmt = conn.prepare(&sql).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let nm: Option<String> = r.get(0).unwrap_or(None);
        let ad: Option<String> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
        let sn: Option<f64> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
        let ct: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
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
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- 账户 ----------
pub fn account(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ZACCOUNT") {
        run.warn_file(f, "SCHEMA", "无 ZACCOUNT 表")?;
        return Ok(0);
    }
    let c = cols(&conn, "ZACCOUNT");
    let user = pick(&c, &["ZUSERNAME"]).unwrap_or("ZUSERNAME");
    let typ = pick(&c, &["ZACCOUNTTYPEDESCRIPTION"]);
    let desc = pick(&c, &["ZACCOUNTDESCRIPTION"]);
    let date = pick(&c, &["ZDATE"]);
    let bundle = pick(&c, &["ZOWNINGBUNDLEID"]);
    let mut sel = vec![user.to_string()];
    for x in [typ, desc, date, bundle].into_iter().flatten() {
        sel.push(x.to_string());
    }
    let sql = format!("SELECT {} FROM ZACCOUNT", sel.join(", "));
    let mut stmt = conn.prepare(&sql).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let u: Option<String> = r.get(0).unwrap_or(None);
        let t: Option<String> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
        let d: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
        let dt: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
        let b: Option<String> = if cnt > 4 { r.get(4).unwrap_or(None) } else { None };
        run.record_values(
            "ios.account",
            vec![
                u.unwrap_or_default().into(),
                t.unwrap_or_default().into(),
                d.unwrap_or_default().into(),
                ts_or_nil(apple(dt.unwrap_or_default())),
                b.unwrap_or_default().into(),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- WiFi ----------
pub fn wifi(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = f.read_all(32 << 20)?;
    let Ok(v) = plist::Value::from_reader(std::io::Cursor::new(&bytes)) else {
        run.warn_file(f, "PLIST_PARSE", "wifi plist 解析失败")?;
        return Ok(0);
    };
    let mut n = 0usize;
    if let Some(nets) = v
        .as_dictionary()
        .and_then(|d| d.get("ListOfPreferredNetworks"))
        .and_then(plist::Value::as_dictionary)
    {
        for (ssid, info) in nets {
            let d = info.as_dictionary();
            let get_s = |k: &str| d.and_then(|x| x.get(k)).and_then(plist::Value::as_string).map(|s| s.to_string());
            let last = d.and_then(|x| x.get("LastJoined")).and_then(plist::Value::as_date).and_then(|x| plist_date(&x));
            let auto = d.and_then(|x| x.get("AutoJoin")).and_then(plist::Value::as_boolean);
            let sec = d.and_then(|x| x.get("SecurityMode")).and_then(|x| x.as_signed_integer());
            run.record_values(
                "ios.wifi",
                vec![
                    (get_s("SSIDString").unwrap_or_else(|| ssid.clone())).into(),
                    Value::Nil,
                    (match sec {
                        Some(3) | Some(5) => "WPA/WPA2",
                        Some(11) => "WPA3",
                        Some(1) => "WEP",
                        Some(0) => "开放",
                        _ => "",
                    })
                    .into(),
                    (if auto == Some(true) { "是" } else { "否" }).into(),
                    ts_or_nil(last),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
        }
    }
    if n == 0 {
        run.warn_file(f, "WIFI_EMPTY", "wifi plist 未解析出网络")?;
    }
    Ok(n)
}

// ---------- 相册 / 笔记（防御式） ----------
pub fn photo(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ZASSET") {
        return Ok(0);
    }
    let c = cols(&conn, "ZASSET");
    let file = pick(&c, &["ZFILENAME"]).unwrap_or("ZFILENAME");
    let created = pick(&c, &["ZDATECREATED"]);
    let media = pick(&c, &["ZMEDIATYPE"]);
    let dur = pick(&c, &["ZDURATION"]);
    let mut sel = vec![file.to_string()];
    for x in [created, media, dur].into_iter().flatten() {
        sel.push(x.to_string());
    }
    let mut stmt = match conn.prepare(&format!("SELECT {} FROM ZASSET", sel.join(", "))) {
        Ok(s) => s,
        Err(_) => return Ok(0),
    };
    let cnt = stmt.column_count();
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let fname: Option<String> = r.get(0).unwrap_or(None);
        let cr: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
        let mt: Option<i64> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
        let du: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
        run.record_values(
            "ios.photo",
            vec![
                fname.unwrap_or_default().into(),
                (match mt {
                    Some(1) => "视频",
                    Some(2) => "音频",
                    _ => "照片",
                })
                .into(),
                Value::from(du.unwrap_or(0.0)),
                ts_or_nil(apple(cr.unwrap_or_default())),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

pub fn note(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    if !table_exists(&conn, "ZICNOTE") {
        return Ok(0);
    }
    let c = cols(&conn, "ZICNOTE");
    let Some(title) = pick(&c, &["ZTITLE"]) else { return Ok(0); };
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
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let t: Option<String> = r.get(0).unwrap_or(None);
        let cr: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
        let ed: Option<f64> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
        run.record_values(
            "ios.note",
            vec![
                t.unwrap_or_default().into(),
                ts_or_nil(apple(cr.unwrap_or_default())),
                ts_or_nil(apple(ed.unwrap_or_default())),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- Safari 浏览器历史 ----------
pub fn web_history(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    if !table_exists(&conn, "history_items") || !table_exists(&conn, "history_visits") {
        run.warn_file(f, "SCHEMA", "无 history_items/history_visits 表")?;
        return Ok(0);
    }
    let sql = "SELECT h.url, h.title, v.visit_time, (SELECT COUNT(*) FROM history_visits v2 WHERE v2.history_item = v.history_item)                FROM history_items h JOIN history_visits v ON v.history_item = h.ROWID ORDER BY v.visit_time";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("历史查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let url: Option<String> = r.get(0).unwrap_or(None);
        let title: Option<String> = r.get(1).unwrap_or(None);
        let vt: Option<f64> = r.get(2).unwrap_or(None);
        let cnt: i64 = r.get(3).unwrap_or(1);
        // visit_time：Apple epoch 秒
        let ts = vt.filter(|v| v.abs() > 1e14).map(|v| ts_plus8(APPLE_EPOCH + (v / 1e9) as i64))
            .or_else(|| vt.map(|v| ts_plus8(APPLE_EPOCH + v as i64)))
            .flatten();
        run.record_values(
            "ios.web_history",
            vec![
                ts.map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                url.unwrap_or_default().into(),
                title.unwrap_or_default().into(),
                cnt.into(),
                "Safari".into(),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    Ok(n)
}
