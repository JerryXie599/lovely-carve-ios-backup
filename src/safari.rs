//! Safari 附加数据：书签（Bookmarks.db）、下载记录（Downloads.plist）。
//!
//! 历史条目由 backupdb::web_history 输出；本模块补齐书签与下载两类通用数据源。

use crate::backupdb::open_db;
use crate::util::{ts_plus8, APPLE_EPOCH};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use rusqlite::Connection;
use std::collections::HashMap;

/// Apple epoch 秒/纳秒 → UTC+8 Timestamp（量级 >1e14 视为纳秒）。
fn apple_ts(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 {
        (v / 1e9) as i64
    } else {
        v as i64
    };
    ts_plus8(APPLE_EPOCH + secs)
}

/// 整数时间：纳秒量级先降为秒，避免 f64 精度损失。
fn apple_int(v: i64) -> Option<Timestamp> {
    if v == 0 {
        return None;
    }
    let secs = if v.abs() > 100_000_000_000_000 {
        v / 1_000_000_000
    } else {
        v
    };
    ts_plus8(APPLE_EPOCH + secs)
}

/// plist::Date（UTC 绝对时间）→ UTC+8 Timestamp。
fn plist_date(d: plist::Date) -> Option<Timestamp> {
    let unix = std::time::SystemTime::from(d)
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    ts_plus8(unix)
}

fn ts_value(t: Option<Timestamp>) -> Value {
    t.map(|t| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil)
}

fn table_exists(conn: &Connection, t: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |_| Ok(()),
    )
    .is_ok()
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
    want.iter().find_map(|w| {
        available
            .iter()
            .find(|c| c.eq_ignore_ascii_case(w))
            .map(|c| c.as_str())
    })
}

/// 行内整数列：兼容 INTEGER/REAL/TEXT 三种存储。
fn row_i64(r: &rusqlite::Row<'_>, idx: usize) -> Option<i64> {
    r.get::<_, Option<i64>>(idx)
        .ok()
        .flatten()
        .or_else(|| {
            r.get::<_, Option<f64>>(idx)
                .ok()
                .flatten()
                .map(|x| x as i64)
        })
        .or_else(|| {
            r.get::<_, Option<String>>(idx)
                .ok()
                .flatten()
                .and_then(|s| s.trim().parse::<i64>().ok())
        })
}

/// 行内时间列：兼容 INTEGER/REAL/TEXT。
fn row_time(r: &rusqlite::Row<'_>, idx: usize) -> Option<f64> {
    r.get::<_, Option<f64>>(idx).ok().flatten().or_else(|| {
        r.get::<_, Option<i64>>(idx)
            .ok()
            .flatten()
            .map(|x| x as f64)
            .or_else(|| {
                r.get::<_, Option<String>>(idx)
                    .ok()
                    .flatten()
                    .and_then(|s| s.trim().parse::<f64>().ok())
            })
    })
}

/// 沿 parent 向上最多 3 层拼出书签文件夹路径（根 → 叶）。
fn folder_path(map: &HashMap<i64, (String, Option<i64>)>, parent: Option<i64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = parent;
    for _ in 0..3 {
        let Some(id) = cur else { break };
        let Some((title, up)) = map.get(&id) else {
            break;
        };
        if !title.is_empty() {
            parts.push(title.clone());
        }
        cur = *up;
    }
    parts.reverse();
    parts.join(" / ")
}

/// Bookmarks.db：bookmarks / bookmarks_folders 表。
pub fn bookmarks(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_tmp, conn) =
        open_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;

    // 主表缺失说明不是 Safari 书签库，直接降级。
    if !table_exists(&conn, "bookmarks") {
        run.warn_file(f, "SCHEMA", "无 bookmarks 表")?;
        return Ok(0);
    }
    let bc = cols(&conn, "bookmarks");
    let Some(url_col) = pick(&bc, &["url"]) else {
        run.warn_file(f, "SCHEMA", "bookmarks 表缺 url 列")?;
        return Ok(0);
    };
    let title_col = pick(&bc, &["title"]);
    let parent_col = pick(&bc, &["parent"]);
    let created_col = pick(&bc, &["created"]);

    // id -> (文件夹标题, 上级 id)；先取文件夹表，再用书签表里 url 为空的目录行补充。
    let mut folders: HashMap<i64, (String, Option<i64>)> = HashMap::new();
    if table_exists(&conn, "bookmarks_folders") {
        let fc = cols(&conn, "bookmarks_folders");
        if let Some(ftitle) = pick(&fc, &["title"]) {
            let fparent = pick(&fc, &["parent"]);
            let pexpr = fparent
                .map(|c| format!("[{c}]"))
                .unwrap_or_else(|| "NULL".into());
            let sql = format!("SELECT ROWID, [{ftitle}], {pexpr} FROM bookmarks_folders");
            if let Ok(mut s) = conn.prepare(&sql) {
                if let Ok(mut rs) = s.query([]) {
                    while let Ok(Some(r)) = rs.next() {
                        let id = r.get::<_, i64>(0).unwrap_or_default();
                        let title = r
                            .get::<_, Option<String>>(1)
                            .unwrap_or(None)
                            .unwrap_or_default();
                        let parent = row_i64(&r, 2);
                        folders.insert(id, (title, parent));
                    }
                }
            }
        }
    }
    // 部分版本把文件夹作为 bookmarks 里的 url 空行保存，同样纳入解析。
    if let Some(ttitle) = title_col {
        let pexpr = parent_col
            .map(|c| format!("[{c}]"))
            .unwrap_or_else(|| "NULL".into());
        let sql = format!(
            "SELECT ROWID, [{ttitle}], {pexpr} FROM bookmarks \
             WHERE [{url_col}] IS NULL OR TRIM([{url_col}]) = ''"
        );
        if let Ok(mut s) = conn.prepare(&sql) {
            if let Ok(mut rs) = s.query([]) {
                while let Ok(Some(r)) = rs.next() {
                    let id = r.get::<_, i64>(0).unwrap_or_default();
                    let title = r
                        .get::<_, Option<String>>(1)
                        .unwrap_or(None)
                        .unwrap_or_default();
                    if !title.is_empty() {
                        folders.insert(id, (title, row_i64(&r, 2)));
                    }
                }
            }
        }
    }

    let title_expr = title_col
        .map(|c| format!("COALESCE([{c}], '')"))
        .unwrap_or_else(|| "''".into());
    let parent_expr = parent_col
        .map(|c| format!("[{c}]"))
        .unwrap_or_else(|| "NULL".into());
    let created_expr = created_col
        .map(|c| format!("[{c}]"))
        .unwrap_or_else(|| "NULL".into());
    let sql = format!(
        "SELECT [{url_col}], {title_expr}, {parent_expr}, {created_expr} FROM bookmarks \
         WHERE [{url_col}] IS NOT NULL AND TRIM([{url_col}]) <> '' ORDER BY {created_expr}"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            run.warn_file(f, "SQL", format!("书签查询失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => {
            run.warn_file(f, "SQL", format!("书签查询失败：{e}"))?;
            return Ok(0);
        }
    };

    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        let url: Option<String> = r.get(0).unwrap_or(None);
        let url = url.unwrap_or_default();
        if url.trim().is_empty() {
            continue;
        }
        let title: Option<String> = r.get(1).unwrap_or(None);
        let parent = row_i64(&r, 2);
        let added = row_time(&r, 3).and_then(apple_ts);
        run.record_values(
            "ios.web_bookmark",
            vec![
                "Safari".into(),
                title.unwrap_or_default().into(),
                url.into(),
                folder_path(&folders, parent).into(),
                ts_value(added),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

// ---------- Downloads.plist ----------

/// 在 plist 字典中按候选键名取值（先精确、后忽略大小写）。
fn dict_get<'a>(d: &'a plist::Dictionary, keys: &[&str]) -> Option<&'a plist::Value> {
    for k in keys {
        if let Some(v) = d.get(k) {
            return Some(v);
        }
    }
    for k in keys {
        if let Some((_, v)) = d.iter().find(|(name, _)| name.eq_ignore_ascii_case(k)) {
            return Some(v);
        }
    }
    None
}

fn looks_like_entry(d: &plist::Dictionary) -> bool {
    dict_get(d, &["URL", "Url", "DownloadEntryURL"]).is_some()
        || dict_get(d, &["Path", "FileName", "Name", "DownloadEntryPath"]).is_some()
}

/// 兼容数组根、`{Downloads: [...]}`、单个条目、URL → 条目字典四种形态。
fn download_entries(v: &plist::Value) -> Vec<&plist::Dictionary> {
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .filter_map(plist::Value::as_dictionary)
            .filter(|d| looks_like_entry(d))
            .collect();
    }
    let Some(d) = v.as_dictionary() else {
        return Vec::new();
    };
    if let Some(inner) = dict_get(d, &["Downloads", "DownloadList", "DownloadItems"]) {
        let found = download_entries(inner);
        if !found.is_empty() {
            return found;
        }
    }
    if looks_like_entry(d) {
        return vec![d];
    }
    d.values()
        .filter_map(plist::Value::as_dictionary)
        .filter(|x| looks_like_entry(x))
        .collect()
}

/// 只取路径末段作为文件名。
fn base_name(s: &str) -> String {
    let s = s.trim();
    let s = s.split(['?', '#']).next().unwrap_or(s);
    let s = s.trim_end_matches(['/', '\\']);
    let name = s.rsplit(['/', '\\']).next().unwrap_or(s);
    if name.is_empty() {
        s.to_string()
    } else {
        name.to_string()
    }
}

fn size_value(v: Option<&plist::Value>) -> Value {
    let Some(v) = v else { return Value::Nil };
    let n = v
        .as_unsigned_integer()
        .or_else(|| v.as_signed_integer().and_then(|x| u64::try_from(x).ok()))
        .or_else(|| {
            v.as_real()
                .filter(|x| x.is_finite() && *x >= 0.0)
                .map(|x| x as u64)
        })
        .or_else(|| v.as_string().and_then(|s| s.trim().parse::<u64>().ok()));
    n.map(Value::from).unwrap_or(Value::Nil)
}

fn entry_time(v: Option<&plist::Value>) -> Option<Timestamp> {
    let v = v?;
    if let Some(d) = v.as_date() {
        return plist_date(d);
    }
    if let Some(i) = v.as_signed_integer() {
        return apple_int(i);
    }
    if let Some(f) = v.as_real() {
        return apple_ts(f);
    }
    v.as_string()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .and_then(apple_ts)
}

/// Downloads.plist：Safari 下载列表。
pub fn downloads(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(32 << 20) {
        Ok(b) => b,
        Err(e) => {
            run.warn_file(f, "READ", format!("下载列表读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let Some(v) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", "Downloads.plist 解析失败")?;
        return Ok(0);
    };
    let entries = download_entries(&v);
    if entries.is_empty() {
        return Ok(0);
    }

    let mut n = 0usize;
    for d in entries {
        let url = dict_get(d, &["URL", "Url", "DownloadEntryURL"])
            .and_then(|x| x.as_string())
            .map(str::to_string)
            .unwrap_or_default();
        let file = dict_get(d, &["Path", "FileName", "Name", "DownloadEntryPath"])
            .and_then(|x| x.as_string())
            .map(base_name)
            .unwrap_or_default();
        // 两个关键字段都没有的字典不是下载条目。
        if url.is_empty() && file.is_empty() {
            continue;
        }
        let size = size_value(dict_get(
            d,
            &["FileSize", "Size", "DownloadEntryProgressTotalToLoad"],
        ));
        let time = entry_time(dict_get(
            d,
            &[
                "StartTime",
                "DownloadTime",
                "Date",
                "DownloadEntryDateAdded",
                "DownloadEntryDateFinished",
            ],
        ));
        run.record_values(
            "ios.web_download",
            vec![
                "Safari".into(),
                url.into(),
                file.into(),
                size,
                ts_value(time),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}
