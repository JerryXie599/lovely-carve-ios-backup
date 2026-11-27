//! 通用 Key-Value + JSON 应用库解析（Apollo/GraphQL 缓存等）。
//!
//! 许多现代 App 用单表 `records(key, record)` 存缓存，value 为 JSON。本模块按
//! 列名与内容特征识别，抽取消息类记录到 `ios.app_msg`，时间字段按量级判别。

use crate::backupdb::open_db;
use crate::util::ts_plus8;
use lovelycarve_plugin::{FileRef, Run, Value};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::Value as Json;

/// 单库最多输出的记录数。
const MAX_RECORDS: usize = 5000;
/// 单表最多扫描的行数（防御超大缓存表）。
const MAX_SCAN_ROWS: usize = 20_000;
/// 用户 extra 字段保留的最大字符数。
const MAX_EXTRA: usize = 1000;

const KEY_COLUMNS: &[&str] = &["key"];
const VALUE_COLUMNS: &[&str] = &["value", "record", "json", "data", "content"];
/// 消息正文候选字段。
const MESSAGE_TEXT_KEYS: &[&str] = &["text", "body", "content", "message"];
/// 消息时间候选字段。
const TIME_KEYS: &[&str] = &["creationTime", "createTime", "timestamp", "time", "date"];
/// 用户/机器人身份字段。
const USER_KEYS: &[&str] = &["displayName", "name", "fullName", "handle", "uid"];

/// 解析单个 KV+JSON 数据库。
pub fn scan(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    run.check_cancelled()?;
    // 同批输入里可能包含同名 -wal/-shm，交给 open_db 一并还原。
    let (_tmp, conn) = match open_db(f, run.inputs()) {
        Ok(x) => x,
        Err(e) => {
            run.warn_file(f, "DB_OPEN", format!("KV 数据库打开失败：{e}"))?;
            return Ok(0);
        }
    };
    let tables = match table_names(&conn) {
        Ok(t) => t,
        Err(e) => {
            run.warn_file(f, "SQL", format!("sqlite_master 读取失败：{e}"))?;
            return Ok(0);
        }
    };

    let app = app_label(f);
    let mut emitted = 0usize;
    for table in tables {
        if emitted >= MAX_RECORDS {
            break;
        }
        run.check_cancelled()?;
        let cols = columns(&conn, &table);
        let Some(key) = pick(&cols, KEY_COLUMNS) else {
            continue;
        };
        // 同一张表可能同时有 value/record 等多列，逐列抽样，取第一个确实存 JSON 的列。
        let mut value = None;
        for want in VALUE_COLUMNS {
            let Some(col) = cols
                .iter()
                .find(|c| c.eq_ignore_ascii_case(want))
                .map(|c| c.as_str())
            else {
                continue;
            };
            if col.eq_ignore_ascii_case(key) {
                continue;
            }
            if sample_is_json(&conn, &table, col) {
                value = Some(col);
                break;
            }
        }
        let Some(value) = value else {
            continue;
        };

        let sql = format!(
            "SELECT {}, {} FROM {} WHERE {} IS NOT NULL LIMIT {}",
            quote(key),
            quote(value),
            quote(&table),
            quote(value),
            MAX_SCAN_ROWS
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                run.warn_file(f, "SQL", format!("表 {table} 查询失败：{e}"))?;
                continue;
            }
        };
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(e) => {
                run.warn_file(f, "SQL", format!("表 {table} 查询失败：{e}"))?;
                continue;
            }
        };
        while let Ok(Some(row)) = rows.next() {
            let key_text = row.get_ref(0).ok().and_then(cell_text).unwrap_or_default();
            let Some(raw) = row.get_ref(1).ok().and_then(cell_text) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<Json>(&raw) else {
                continue;
            };
            let Some(obj) = json.as_object() else {
                continue;
            };

            // 消息：含正文候选字段 + 时间字段。
            if let Some(content) = text_field(obj, MESSAGE_TEXT_KEYS) {
                if field(obj, TIME_KEYS).is_some() {
                    let chat =
                        text_field(obj, &["chat", "conversation", "title"]).unwrap_or_default();
                    let sender = sender_text(obj);
                    let msg_type = text_field(obj, &["type"]).unwrap_or_else(|| "json".to_string());
                    run.record_values(
                        "ios.app_msg",
                        vec![
                            app.clone().into(),
                            chat.into(),
                            sender.into(),
                            content.into(),
                            time_value(obj),
                            String::new().into(),
                            msg_type.into(),
                            String::new().into(),
                        ],
                    )
                    .source(crate::util::node_source(f))
                    .emit()?;
                    emitted += 1;
                    if emitted >= MAX_RECORDS {
                        break;
                    }
                    continue;
                }
            }

            // 用户/机器人定义：含身份字段且不含消息正文。
            if text_field(obj, MESSAGE_TEXT_KEYS).is_some() {
                continue;
            }
            if field(obj, USER_KEYS).is_none() {
                continue;
            }
            let mut user_id = key_text.trim().to_string();
            if user_id.is_empty() {
                user_id = text_field(obj, &["uid", "handle"]).unwrap_or_default();
            }
            let nickname =
                text_field(obj, &["displayName", "fullName", "name"]).unwrap_or_default();
            if user_id.is_empty() && nickname.is_empty() {
                continue;
            }
            let extra = truncate_chars(&raw, MAX_EXTRA);
            run.record_values(
                "ios.app_user",
                vec![
                    app.clone().into(),
                    user_id.into(),
                    nickname.into(),
                    String::new().into(),
                    String::new().into(),
                    String::new().into(),
                    extra.into(),
                ],
            )
            .source(crate::util::node_source(f))
            .emit()?;
            emitted += 1;
            if emitted >= MAX_RECORDS {
                break;
            }
        }
        if emitted >= MAX_RECORDS {
            break;
        }
    }
    Ok(emitted)
}

/// sqlite_master 中的用户表名。
fn table_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        if let Ok(Some(name)) = row.get::<_, Option<String>>(0) {
            out.push(name);
        }
    }
    Ok(out)
}

/// 表列名。
fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let sql = format!("PRAGMA table_info({})", quote(table));
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(Some(name)) = row.get::<_, Option<String>>(1) {
                    out.push(name);
                }
            }
        }
    }
    out
}

/// 从候选列名中取第一个存在的列（忽略大小写）。
fn pick<'a>(available: &'a [String], want: &[&str]) -> Option<&'a str> {
    want.iter().find_map(|w| {
        available
            .iter()
            .find(|c| c.eq_ignore_ascii_case(w))
            .map(|c| c.as_str())
    })
}

/// value 列抽样：是否存在以 `{` 或 `[` 开头的值。
fn sample_is_json(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!(
        "SELECT {} FROM {} WHERE {} IS NOT NULL LIMIT 32",
        quote(column),
        quote(table),
        quote(column)
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return false;
    };
    let Ok(mut rows) = stmt.query([]) else {
        return false;
    };
    while let Ok(Some(row)) = rows.next() {
        if let Some(text) = row.get_ref(0).ok().and_then(cell_text) {
            if is_json_text(&text) {
                return true;
            }
        }
    }
    false
}

fn is_json_text(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

/// SQLite 单元格转文本（BLOB 按 UTF-8 容错解码）。
fn cell_text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(x) => Some(format!("{x}")),
        ValueRef::Null => None,
    }
}

/// 对象内按候选键取值（忽略大小写）。
fn field<'a>(obj: &'a serde_json::Map<String, Json>, keys: &[&str]) -> Option<&'a Json> {
    for want in keys {
        if let Some((_, v)) = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)) {
            return Some(v);
        }
    }
    None
}

/// 字符串/数字/布尔统一转文本，空串视为缺失。
fn scalar(v: &Json) -> Option<String> {
    match v {
        Json::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// 按候选键顺序取第一个可用标量；前面的键为空/为对象时继续尝试后面的键。
fn text_field(obj: &serde_json::Map<String, Json>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|want| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(want))
            .and_then(|(_, v)| scalar(v))
    })
}

/// 发送者：字符串直接使用；对象时取常见身份字段。
fn sender_text(obj: &serde_json::Map<String, Json>) -> String {
    for want in ["author", "sender", "user"] {
        let Some((_, v)) = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)) else {
            continue;
        };
        if let Some(s) = scalar(v) {
            return s;
        }
        if let Some(map) = v.as_object() {
            if let Some(s) = text_field(
                map,
                &["displayName", "fullName", "name", "uid", "handle", "id"],
            ) {
                return s;
            }
        }
    }
    String::new()
}

/// 对象内第一个可解析的时间字段 → UTC+8 Timestamp。
///
/// 同一对象可能存在多个时间键（部分为 null/字符串/对象），逐个尝试，
/// 全部不可用时返回 Nil，记录本身仍保留。
fn time_value(obj: &serde_json::Map<String, Json>) -> Value {
    for want in TIME_KEYS {
        let Some((_, v)) = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)) else {
            continue;
        };
        let Some(raw) = json_number(v) else {
            continue;
        };
        let Some(secs) = unix_seconds(raw) else {
            continue;
        };
        if let Some(t) = ts_plus8(secs) {
            return Value::from(t.as_str().to_string());
        }
    }
    Value::Nil
}

/// JSON 数值/数字字符串 → f64。
fn json_number(v: &Json) -> Option<f64> {
    match v {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// 数值量级 → Unix 秒：>1e15 微秒、>1e12 毫秒、其余秒。
fn unix_seconds(raw: f64) -> Option<i64> {
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    let secs = if raw > 1e15 {
        raw / 1_000_000.0
    } else if raw > 1e12 {
        raw / 1_000.0
    } else {
        raw
    };
    // 限定在 1970..=2100，避免异常量级传给时间格式化。
    if !(1.0..=4_102_444_800.0).contains(&secs) {
        return None;
    }
    Some(secs as i64)
}

/// 记录对应的 App 标识：优先 Applications/<bundle>，否则文件名去扩展名。
fn app_label(f: &FileRef) -> String {
    if let Some(bundle) = f.view_path().and_then(bundle_from_path) {
        return bundle;
    }
    file_stem(f.name())
}

fn bundle_from_path(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for pair in segs.windows(2) {
        if pair[0].eq_ignore_ascii_case("Applications") {
            let bundle = pair[1].trim();
            if !bundle.is_empty() {
                return Some(bundle.to_string());
            }
        }
    }
    None
}

fn file_stem(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

/// 按字符截断，避免切断 UTF-8。
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// SQLite 标识符引用（双引号，内部双引号转义）。
fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}
