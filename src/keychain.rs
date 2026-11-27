//! 钥匙串导出解析：KeyChain.json / keychain-backup.plist / ComparableKeychainBackup.plist。
//!
//! 输出 `ios.keychain_item`（含明文 Data），并提供 `secrets()` 供 WiFi 解析关联密码。
//! 仅解析检材中已存在的导出文件，不做任何解密或口令推导。

use crate::util::{ts_plus8, APPLE_EPOCH};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;
use std::collections::HashMap;

/// 单个导出文件的读取上限（钥匙串导出通常只有数百 KiB）。
const MAX_BYTES: u64 = 128 << 20;
/// 递归深度上限，异常结构不至于耗尽栈。
const MAX_DEPTH: usize = 32;

/// 三种导出格式统一的中间结构。
#[derive(Clone, Default)]
struct KcItem {
    account: String,
    service: String,
    access_group: String,
    created: Option<i64>,  // Unix 秒
    modified: Option<i64>, // Unix 秒
    data: Vec<u8>,
    note: &'static str,
}

/// 解析单个钥匙串导出文件。
pub fn items(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let name = f.name().to_string();
    let lower = name.to_ascii_lowercase();
    let is_json = lower == "keychain.json";
    let is_plist = lower == "keychain-backup.plist" || lower == "comparablekeychainbackup.plist";
    if !is_json && !is_plist {
        run.warn_file(
            f,
            "KEYCHAIN_NAME",
            format!("未识别的钥匙串导出文件名：{name}"),
        )?;
        return Ok(0);
    }
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) => {
            run.warn_file(f, "KEYCHAIN_READ", format!("{name} 读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let parsed = if is_json {
        parse_json(&bytes)
    } else {
        parse_plist(&bytes)
    };
    let parsed = match parsed {
        Ok(items) => items,
        Err(e) => {
            run.warn_file(f, "KEYCHAIN_PARSE", format!("{name} 解析失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut n = 0usize;
    for it in &parsed {
        // 只有能按 UTF-8 解释且不含控制字符的 Data 才作为明文输出，其余留空。
        let secret = printable(&it.data).unwrap_or_default();
        run.record_values(
            "ios.keychain_item",
            vec![
                it.account.clone().into(),
                it.service.clone().into(),
                it.access_group.clone().into(),
                ts_value(it.created),
                ts_value(it.modified),
                Value::from(it.data.len() as u64),
                secret.into(),
                it.note.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    if n == 0 {
        run.warn_file(f, "KEYCHAIN_EMPTY", format!("{name} 未解析出钥匙串条目"))?;
    }
    Ok(n)
}

/// 汇总所有钥匙串导出中的明文口令：key 为 account/service 的小写形式。
pub fn secrets(files: &[(String, FileRef)]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for (_path, f) in files {
        let lower = f.name().to_ascii_lowercase();
        let is_json = lower == "keychain.json";
        let is_plist =
            lower == "keychain-backup.plist" || lower == "comparablekeychainbackup.plist";
        if !is_json && !is_plist {
            continue;
        }
        let Ok(bytes) = f.read_all(MAX_BYTES) else {
            continue;
        };
        let parsed = if is_json {
            parse_json(&bytes)
        } else {
            parse_plist(&bytes)
        };
        let Ok(items) = parsed else { continue };
        for it in items {
            let Some(secret) = printable(&it.data) else {
                continue;
            };
            // AirPort 条目的 Account 就是 SSID，Service 仅作为补充键；
            // 先写入 account，避免服务名覆盖同名网络的口令。
            for key in [it.account, it.service] {
                let key = key.trim().to_lowercase();
                if key.is_empty() {
                    continue;
                }
                map.entry(key).or_insert_with(|| secret.clone());
            }
        }
    }
    map
}

/// Unix 秒 → UTC+8 Timestamp（无值时为 Nil）。
fn ts_value(unix: Option<i64>) -> Value {
    unix.and_then(ts_plus8)
        .map(|t: Timestamp| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil)
}

/// Data → 明文：严格要求 UTF-8、去掉尾部 NUL 后非空且不含控制字符。
fn printable(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let text = text.trim_end_matches('\0');
    if text.is_empty() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(text.to_string())
}

// ---------- KeyChain.json ----------

fn parse_json(bytes: &[u8]) -> Result<Vec<KcItem>, String> {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let root: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    collect_json(&root, &mut out, 0);
    Ok(out)
}

fn collect_json(v: &serde_json::Value, out: &mut Vec<KcItem>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_json(item, out, depth + 1);
            }
        }
        serde_json::Value::Object(obj) => {
            let account = json_text(obj, &["Account", "account", "acct"]);
            let service = json_text(obj, &["Service", "service", "svce"]);
            let data = json_get(obj, &["Data", "data", "v_Data"]);
            // 数组元素或单个对象都按字段识别，不依赖外层结构。
            if account.is_some() || service.is_some() || data.is_some() {
                out.push(KcItem {
                    account: account.unwrap_or_default(),
                    service: service.unwrap_or_default(),
                    access_group: json_text(obj, &["AccessGroup", "accessGroup", "agrp"])
                        .unwrap_or_default(),
                    created: json_time(obj, &["CreatedDate", "Created", "crat", "created"]),
                    modified: json_time(obj, &["ModifiedDate", "Modified", "mdat", "modified"]),
                    data: data.map(json_bytes).unwrap_or_default(),
                    note: "钥匙串导出(JSON)",
                });
                return;
            }
            for (_key, val) in obj {
                collect_json(val, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn json_get<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) -> Option<&'a serde_json::Value> {
    for name in names {
        if let Some(v) = obj.get(*name) {
            return Some(v);
        }
    }
    obj.iter()
        .find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n)))
        .map(|(_, v)| v)
}

fn json_text(obj: &serde_json::Map<String, serde_json::Value>, names: &[&str]) -> Option<String> {
    let text = json_get(obj, names)?.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn json_bytes(v: &serde_json::Value) -> Vec<u8> {
    match v {
        serde_json::Value::String(s) => decode_base64(s).unwrap_or_default(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_u64())
            .map(|x| x as u8)
            .collect(),
        _ => Vec::new(),
    }
}

fn json_time(obj: &serde_json::Map<String, serde_json::Value>, names: &[&str]) -> Option<i64> {
    let v = json_get(obj, names)?;
    match v {
        serde_json::Value::String(s) => parse_date_string(s),
        serde_json::Value::Number(n) => n.as_f64().and_then(apple_or_unix),
        _ => None,
    }
}

// ---------- keychain-backup.plist / ComparableKeychainBackup.plist ----------

fn parse_plist(bytes: &[u8]) -> Result<Vec<KcItem>, String> {
    let root = crate::util::read_plist(bytes).ok_or_else(|| "plist 解析失败".to_string())?;
    let mut out = Vec::new();
    collect_plist(&root, "", &mut out, 0);
    Ok(out)
}

fn collect_plist(v: &PValue, section: &str, out: &mut Vec<KcItem>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        PValue::Dictionary(d) => {
            let account = pdict_text(d, &["acct", "Account", "account"]);
            let service = pdict_text(d, &["svce", "Service", "service"]);
            let data = pdict_data(d, &["v_Data", "Data", "data"]);
            if account.is_some() || service.is_some() || data.is_some() {
                let group = pdict_text(d, &["agrp", "AccessGroup", "accessGroup"]);
                let class = pdict_text(d, &["class", "v_Class", "item_class"]);
                out.push(KcItem {
                    account: account.unwrap_or_default(),
                    service: service.unwrap_or_default(),
                    // 字段仅能容纳一个类别值：优先访问组（agrp），没有则用所在分组名。
                    access_group: group.or(class).unwrap_or_else(|| section.to_string()),
                    created: pdict_time(d, &["crat", "CreatedDate", "created"]),
                    modified: pdict_time(d, &["mdat", "ModifiedDate", "modified"]),
                    data: data.unwrap_or_default(),
                    note: "钥匙串导出(plist)",
                });
                return;
            }
            for (key, val) in d {
                if matches!(val, PValue::Dictionary(_) | PValue::Array(_)) {
                    collect_plist(val, key, out, depth + 1);
                }
            }
        }
        PValue::Array(arr) => {
            for item in arr {
                collect_plist(item, section, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// 精确键、忽略大小写键依次查找。
fn pdict_get<'a>(d: &'a plist::Dictionary, names: &[&str]) -> Option<&'a PValue> {
    for name in names {
        if let Some(v) = d.get(*name) {
            return Some(v);
        }
    }
    d.iter()
        .find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n)))
        .map(|(_, v)| v)
}

fn pdict_text(d: &plist::Dictionary, names: &[&str]) -> Option<String> {
    pdict_get(d, names).and_then(value_text)
}

fn pdict_data(d: &plist::Dictionary, names: &[&str]) -> Option<Vec<u8>> {
    let v = pdict_get(d, names)?;
    if let Some(bytes) = v.as_data() {
        return Some(bytes.to_vec());
    }
    if let Some(text) = v.as_string() {
        return decode_base64(text);
    }
    None
}

fn pdict_time(d: &plist::Dictionary, names: &[&str]) -> Option<i64> {
    let v = pdict_get(d, names)?;
    match v {
        PValue::Date(date) => plist_date_unix(*date),
        PValue::Integer(i) => i
            .as_signed()
            .map(|v| v as f64)
            .or_else(|| i.as_unsigned().map(|v| v as f64))
            .and_then(apple_or_unix),
        PValue::Real(f) => apple_or_unix(*f),
        PValue::String(s) => parse_date_string(s),
        _ => None,
    }
}

fn value_text(v: &PValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        let text = s.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    if let Some(i) = v.as_signed_integer() {
        return Some(i.to_string());
    }
    v.as_real().filter(|x| x.is_finite()).map(|x| x.to_string())
}

fn plist_date_unix(date: plist::Date) -> Option<i64> {
    std::time::SystemTime::from(date)
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

// ---------- 时间与编码 ----------

/// 数值时间：大于 2008 年 Unix 秒下限的按 Unix 秒处理，否则按 Apple epoch（2001-01-01）秒处理。
fn apple_or_unix(raw: f64) -> Option<i64> {
    if !raw.is_finite() || raw < 0.0 {
        return None;
    }
    if raw >= 1_200_000_000.0 {
        Some(raw as i64)
    } else {
        Some(APPLE_EPOCH + raw as i64)
    }
}

fn parse_date_string(raw: &str) -> Option<i64> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(dt.timestamp());
    }
    if let Some(unix) = parse_compact_time(text) {
        return Some(unix);
    }
    let body = text.trim_end_matches('Z').trim_end_matches('z');
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y/%m/%d %H:%M:%S%.f",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(body, fmt) {
            return Some(dt.and_utc().timestamp());
        }
    }
    if let Ok(n) = text.parse::<f64>() {
        return apple_or_unix(n);
    }
    None
}

/// `20250415094406.96596Z` 形式的紧凑时间（钥匙串 JSON 导出的常见写法）。
fn parse_compact_time(raw: &str) -> Option<i64> {
    let body = raw.trim().trim_end_matches('Z').trim_end_matches('z');
    let (head, _frac) = body.split_once('.').unwrap_or((body, ""));
    if head.len() != 14 || !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = head[0..4].parse().ok()?;
    let month: u32 = head[4..6].parse().ok()?;
    let day: u32 = head[6..8].parse().ok()?;
    let hour: u32 = head[8..10].parse().ok()?;
    let minute: u32 = head[10..12].parse().ok()?;
    let second: u32 = head[12..14].parse().ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let time = chrono::NaiveTime::from_hms_opt(hour, minute, second)?;
    Some(chrono::NaiveDateTime::new(date, time).and_utc().timestamp())
}

/// 标准 base64 解码：容忍缺少 `=` 填充、换行和 URL 安全字符。
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for ch in text.chars() {
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            '=' | '\r' | '\n' | ' ' | '\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}
