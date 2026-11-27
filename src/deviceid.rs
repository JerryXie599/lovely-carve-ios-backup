//! 设备标识与 SIM 配置：iDevice_info.txt、CommCenter、热点、蓝牙设备表。
//!
//! 这些文件与 Manifest.plist 互补：文件级提取的检材常缺 Manifest 的 Lockdown 字段，
//! 设备标识改由 iDevice_info.txt / CommCenter 提供。

use crate::util::{ts_plus8, APPLE_EPOCH};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;
use std::collections::HashSet;

/// 单个文件的读取上限。
const MAX_BYTES: u64 = 64 << 20;
/// 递归深度上限。
const MAX_DEPTH: usize = 24;

/// Unix 秒 → UTC+8 Timestamp（无值时为 Nil）。
fn ts_value(unix: Option<i64>) -> Value {
    unix.and_then(ts_plus8)
        .map(|t: Timestamp| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil)
}

// ---------- 通用取值 ----------

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

/// 数值时间：明显大于 2008 年的按 Unix 秒处理，否则按 Apple epoch（2001-01-01）秒处理。
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

fn plist_date_unix(date: plist::Date) -> Option<i64> {
    std::time::SystemTime::from(date)
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn plist_time(v: &PValue) -> Option<i64> {
    match v {
        PValue::Date(date) => plist_date_unix(*date),
        PValue::Integer(i) => i
            .as_signed()
            .map(|v| v as f64)
            .or_else(|| i.as_unsigned().map(|v| v as f64))
            .and_then(apple_or_unix),
        PValue::Real(f) => apple_or_unix(*f),
        PValue::String(s) => s.trim().parse::<f64>().ok().and_then(apple_or_unix),
        _ => None,
    }
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// 在字典中按键名包含关系查找第一个非空标量文本。
fn find_text(d: &plist::Dictionary, needles: &[&str]) -> Option<String> {
    for (key, val) in d {
        let lower = key.to_ascii_lowercase();
        if needles.iter().any(|n| lower.contains(n)) {
            if let Some(text) = value_text(val) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// 在字典中按键名包含关系查找第一个形似 MAC 的地址。
fn find_mac(d: &plist::Dictionary) -> Option<String> {
    for (key, val) in d {
        let lower = key.to_ascii_lowercase();
        if lower.contains("address") || lower.contains("mac") {
            if let Some(text) = value_text(val) {
                let text = text.trim();
                if looks_like_mac(text) {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

/// 在字典中按键名包含关系查找第一个可解释的时间值。
fn find_time(d: &plist::Dictionary, needles: &[&str]) -> Option<i64> {
    for (key, val) in d {
        let lower = key.to_ascii_lowercase();
        if needles.iter().any(|n| lower.contains(n)) {
            if let Some(unix) = plist_time(val) {
                return Some(unix);
            }
        }
    }
    None
}

/// 递归查找第一个键名匹配的标量文本。
fn deep_find(d: &plist::Dictionary, needles: &[&str], depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(found) = find_text(d, needles) {
        return Some(found);
    }
    for (_key, val) in d {
        match val {
            PValue::Dictionary(sd) => {
                if let Some(found) = deep_find(sd, needles, depth + 1) {
                    return Some(found);
                }
            }
            PValue::Array(arr) => {
                for item in arr {
                    if let PValue::Dictionary(sd) = item {
                        if let Some(found) = deep_find(sd, needles, depth + 1) {
                            return Some(found);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// 按完整键名递归查找标量（用于多卡配置中的“当前数据卡/语音卡” ICCID）。
fn find_scalar_by_key(v: &PValue, needle: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    match v {
        PValue::Dictionary(d) => {
            if let Some(val) = d.get(needle) {
                if let Some(text) = value_text(val) {
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            for (_key, val) in d {
                if let Some(found) = find_scalar_by_key(val, needle, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        PValue::Array(arr) => arr
            .iter()
            .find_map(|item| find_scalar_by_key(item, needle, depth + 1)),
        _ => None,
    }
}

fn looks_like_mac(text: &str) -> bool {
    let t = text.trim();
    if t.len() == 17 {
        let sep = t.as_bytes()[2] as char;
        if sep == ':' || sep == '-' {
            let parts: Vec<&str> = t.split(sep).collect();
            return parts.len() == 6
                && parts
                    .iter()
                    .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
    t.len() == 12 && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn looks_like_iccid(text: &str) -> bool {
    let t = text.trim();
    (18..=22).contains(&t.len()) && t.chars().all(|c| c.is_ascii_digit())
}

// ---------- iDevice_info.txt ----------

/// iDevice_info.txt：`Key: Value` 形式的设备标识总表。
pub fn idevice_info(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let text = match f.read_text(MAX_BYTES) {
        Ok(text) => text,
        Err(e) => {
            run.warn_file(f, "IDEVICE_READ", format!("iDevice_info.txt 读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut n = 0usize;
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = split_pair(line) else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !plain_key(key) || value.is_empty() {
            continue;
        }
        run.record_values(
            "ios.device_info",
            vec![
                key.to_string().into(),
                device_name(key).into(),
                value.to_string().into(),
                "iDevice_info.txt".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    if n == 0 {
        run.warn_file(f, "IDEVICE_EMPTY", "iDevice_info.txt 未解析出键值")?;
    }
    Ok(n)
}

/// 取最早出现的分隔符，兼容 `Key: Value` 与 `Key = Value`。
fn split_pair(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':');
    let equals = line.find('=');
    match (colon, equals) {
        (Some(c), Some(e)) if e < c => line.split_once('='),
        (Some(_), _) => line.split_once(':'),
        (None, Some(_)) => line.split_once('='),
        (None, None) => None,
    }
}

/// 过滤掉 `<< = Row = >>` 之类的分隔行与异常键名。
fn plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 80
        && key.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | ' ' | '.' | '-' | '/' | '(' | ')' | '[' | ']')
        })
}

fn device_name(key: &str) -> &'static str {
    match key {
        "InternationalMobileEquipmentIdentity" => "IMEI",
        "InternationalMobileEquipmentIdentity2" => "IMEI2",
        "MobileEquipmentIdentifier" => "MEID",
        "IntegratedCircuitCardIdentity" => "ICCID",
        "InternationalMobileSubscriberIdentity" => "IMSI",
        "PhoneNumber" => "手机号码",
        "BluetoothAddress" => "蓝牙地址",
        "SerialNumber" => "序列号",
        "ProductType" => "产品类型",
        "ProductVersion" => "系统版本",
        "BuildVersion" => "构建版本",
        "UniqueDeviceID" => "UDID",
        "DeviceName" => "设备名称",
        "ModelNumber" => "型号",
        _ => "",
    }
}

// ---------- com.apple.commcenter.plist ----------

/// com.apple.commcenter.plist：ICCID、IMSI、号码、多卡配置。
pub fn commcenter(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) => {
            run.warn_file(
                f,
                "COMMCENTER_READ",
                format!("commcenter.plist 读取失败：{e}"),
            )?;
            return Ok(0);
        }
    };
    let Some(root) = crate::util::read_plist(&bytes) else {
        run.warn_file(
            f,
            "PLIST_PARSE",
            "com.apple.commcenter.plist 解析失败".to_string(),
        )?;
        return Ok(0);
    };
    let source = f.name().to_string();
    let mut n = 0usize;

    // 1) 标识类标量：键名含 ICCID/IMEI/IMSI/PhoneNumber。
    let mut values: Vec<(String, String)> = Vec::new();
    collect_cc_values(&root, "", &mut values, 0);
    for (path, value) in &values {
        let label = cc_label(last_segment(path));
        run.record_values(
            "ios.device_info",
            vec![
                path.clone().into(),
                label.into(),
                value.clone().into(),
                source.clone().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }

    // 2) 多卡配置与历史 SIM 钱包：每个 SIM 一条 ios.sim。
    let mut rows: Vec<SimRow> = Vec::new();
    collect_sim_rows(&root, "", "", false, &source, &mut rows, 0);
    let data_iccid = find_scalar_by_key(&root, "kDataSimIccidKey", 0).unwrap_or_default();
    let voice_iccid = find_scalar_by_key(&root, "kVoiceSimIccidKey", 0).unwrap_or_default();
    for mut row in merge_sim_rows(rows) {
        if !row.iccid.is_empty() {
            if row.iccid == data_iccid {
                row.extra.push_str("；当前数据卡");
            }
            if row.iccid == voice_iccid {
                row.extra.push_str("；当前语音卡");
            }
        }
        run.record_values(
            "ios.sim",
            vec![
                row.slot.into(),
                row.iccid.into(),
                row.imei.into(),
                row.phone.into(),
                row.label.into(),
                row.extra.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }

    if n == 0 {
        run.warn_file(
            f,
            "COMMCENTER_EMPTY",
            "com.apple.commcenter.plist 未解析出设备标识或 SIM 配置",
        )?;
    }
    Ok(n)
}

fn collect_cc_values(v: &PValue, path: &str, out: &mut Vec<(String, String)>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        PValue::Dictionary(d) => {
            for (key, val) in d {
                let child = join_path(path, key);
                let lower = key.to_ascii_lowercase();
                let wanted = ["iccid", "imei", "imsi", "phonenumber"]
                    .iter()
                    .any(|n| lower.contains(n));
                if wanted {
                    if let Some(text) = value_text(val) {
                        let text = text.trim().to_string();
                        if !text.is_empty() && !out.iter().any(|(p, v)| p == &child && v == &text) {
                            out.push((child.clone(), text));
                        }
                    }
                }
                if matches!(val, PValue::Dictionary(_) | PValue::Array(_)) {
                    collect_cc_values(val, &child, out, depth + 1);
                }
            }
        }
        PValue::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                collect_cc_values(item, &format!("{path}[{i}]"), out, depth + 1);
            }
        }
        _ => {}
    }
}

fn cc_label(key: &str) -> &'static str {
    let lower = key.to_ascii_lowercase();
    if lower.contains("iccid") {
        "ICCID"
    } else if lower.contains("imei") {
        "IMEI"
    } else if lower.contains("imsi") {
        "IMSI"
    } else if lower.contains("phonenumber") || lower.contains("phnum") {
        "手机号码"
    } else {
        ""
    }
}

#[derive(Default)]
struct SimRow {
    slot: String,
    iccid: String,
    imei: String,
    phone: String,
    label: String,
    extra: String,
}

fn has_sim_identity(d: &plist::Dictionary) -> bool {
    for (key, val) in d {
        let lower = key.to_ascii_lowercase();
        let identity = lower.contains("iccid")
            || lower.contains("imei")
            || lower.contains("imsi")
            || lower.contains("phonenumber")
            || lower.contains("phnum")
            || lower.contains("msisdn");
        if identity && value_text(val).map(|t| !t.is_empty()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// 字典中是否直接挂着按槽位/ICCID 分组的 SIM 记录（数组或字典键）。
fn has_sim_child_records(d: &plist::Dictionary) -> bool {
    d.iter().any(|(key, val)| {
        let lower = key.to_ascii_lowercase();
        let slot_like =
            looks_like_iccid(key) || (!key.is_empty() && key.chars().all(|c| c.is_ascii_digit()));
        match val {
            PValue::Array(items) => {
                lower.contains("sim")
                    && items
                        .iter()
                        .any(|item| matches!(item, PValue::Dictionary(_)))
            }
            PValue::Dictionary(sd) => slot_like && has_sim_identity(sd),
            _ => false,
        }
    })
}

fn row_from(
    d: &plist::Dictionary,
    slot: &str,
    iccid_hint: &str,
    source: &str,
    kind: &str,
) -> SimRow {
    let iccid = if iccid_hint.is_empty() {
        deep_find(d, &["iccid"], 0).unwrap_or_default()
    } else {
        iccid_hint.to_string()
    };
    SimRow {
        slot: slot.to_string(),
        iccid,
        imei: deep_find(d, &["imei"], 0).unwrap_or_default(),
        phone: deep_find(d, &["phonenumber", "phnum", "msisdn"], 0).unwrap_or_default(),
        label: deep_find(d, &["carriername", "subscriptionlabel"], 0).unwrap_or_default(),
        extra: format!("来源 {source}；{kind}"),
    }
}

fn collect_sim_rows(
    v: &PValue,
    path: &str,
    slot: &str,
    inside_sim: bool,
    source: &str,
    out: &mut Vec<SimRow>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        PValue::Dictionary(d) => {
            // PersonalWallet 以 ICCID 为字典键，每个键就是一张历史 SIM。
            if slot.eq_ignore_ascii_case("PersonalWallet") {
                for (key, val) in d {
                    if !looks_like_iccid(key) {
                        continue;
                    }
                    let Some(sd) = val.as_dictionary() else {
                        continue;
                    };
                    out.push(row_from(sd, key, key, source, "PersonalWallet 字典键"));
                }
                return;
            }
            // 目录型字典（如 kLocalMultiSimPreferencesKey）本身也带当前卡 ICCID，
            // 但真正的每卡记录在子数组/子字典里，此时只下钻，避免生成重复行。
            if inside_sim && has_sim_identity(d) && !has_sim_child_records(d) {
                out.push(row_from(d, slot, "", source, "SIM 配置字典"));
                return;
            }
            for (key, val) in d {
                let lower = key.to_ascii_lowercase();
                let next_inside = inside_sim || lower.contains("sim") || lower == "personalwallet";
                let child = join_path(path, key);
                if matches!(val, PValue::Dictionary(_) | PValue::Array(_)) {
                    collect_sim_rows(val, &child, key, next_inside, source, out, depth + 1);
                }
            }
        }
        PValue::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let index_slot = (i + 1).to_string();
                // SIM 字典数组：每一项就是一张卡，槽位用 1 起的序号。
                if inside_sim {
                    if let Some(sd) = item.as_dictionary() {
                        out.push(row_from(sd, &index_slot, "", source, "SIM 字典数组"));
                        continue;
                    }
                }
                collect_sim_rows(
                    item,
                    &format!("{path}[{i}]"),
                    &index_slot,
                    inside_sim,
                    source,
                    out,
                    depth + 1,
                );
            }
        }
        _ => {}
    }
}

/// 同一张 SIM 可能同时出现在多卡偏好与钱包中：按 ICCID 合并，缺失字段互补。
fn merge_sim_rows(rows: Vec<SimRow>) -> Vec<SimRow> {
    let mut out: Vec<SimRow> = Vec::new();
    for row in rows {
        if row.iccid.is_empty()
            && row.imei.is_empty()
            && row.phone.is_empty()
            && row.label.is_empty()
        {
            continue;
        }
        let same = out.iter().position(|e| {
            if !row.iccid.is_empty() {
                e.iccid == row.iccid
            } else {
                e.iccid.is_empty() && !row.slot.is_empty() && e.slot == row.slot
            }
        });
        match same {
            Some(i) => {
                let e = &mut out[i];
                if e.iccid.is_empty() {
                    e.iccid = row.iccid;
                }
                if e.imei.is_empty() {
                    e.imei = row.imei;
                }
                if e.phone.is_empty() {
                    e.phone = row.phone;
                }
                if e.label.is_empty() {
                    e.label = row.label;
                }
            }
            None => out.push(row),
        }
    }
    out
}

// ---------- 热点 ----------

/// 个人热点配置（com.apple.MobileInternetSharing / MobileHotspot）。
pub fn hotspot(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) => {
            run.warn_file(f, "HOTSPOT_READ", format!("热点配置读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let Some(root) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", "热点配置解析失败".to_string())?;
        return Ok(0);
    };
    let mut named: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    collect_named_strings(&root, "", &mut named, &mut seen, 0);
    let source = f.name().to_string();
    let mut n = 0usize;
    for (path, value) in &named {
        run.record_values(
            "ios.device_info",
            vec![
                path.clone().into(),
                hotspot_label(last_segment(path)).into(),
                value.clone().into(),
                source.clone().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    // 最可能的 SSID：优先 SSID 键，其次热点/设备名，最后任一名称。
    let best = named
        .iter()
        .find(|(p, _)| last_segment(p).to_ascii_lowercase().contains("ssid"))
        .or_else(|| {
            named
                .iter()
                .find(|(p, _)| last_segment(p).to_ascii_lowercase().contains("hotspot"))
        })
        .or_else(|| {
            named
                .iter()
                .find(|(p, _)| last_segment(p).to_ascii_lowercase().contains("devicename"))
        })
        .or_else(|| named.first());
    if let Some((_path, value)) = best {
        run.record_values(
            "ios.finding",
            vec![
                "个人热点名称".into(),
                value.clone().into(),
                format!("来源 {source}；热点名称通常等于设备名").into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    } else {
        run.warn_file(f, "HOTSPOT_EMPTY", "热点配置中未找到 SSID/设备名")?;
    }
    Ok(n)
}

fn collect_named_strings(
    v: &PValue,
    path: &str,
    out: &mut Vec<(String, String)>,
    seen: &mut HashSet<(String, String)>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        PValue::Dictionary(d) => {
            for (key, val) in d {
                let child = join_path(path, key);
                let lower = key.to_ascii_lowercase();
                let wanted = ["ssid", "hotspot", "devicename", "name"]
                    .iter()
                    .any(|n| lower.contains(n));
                if wanted {
                    if let Some(text) = val.as_string() {
                        let text = text.trim();
                        if !text.is_empty() && !text.chars().any(|c| c.is_control()) {
                            let entry = (child.clone(), text.to_string());
                            if seen.insert(entry.clone()) {
                                out.push(entry);
                            }
                        }
                    }
                }
                if matches!(val, PValue::Dictionary(_) | PValue::Array(_)) {
                    collect_named_strings(val, &child, out, seen, depth + 1);
                }
            }
        }
        PValue::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                collect_named_strings(item, &format!("{path}[{i}]"), out, seen, depth + 1);
            }
        }
        _ => {}
    }
}

fn hotspot_label(key: &str) -> &'static str {
    let lower = key.to_ascii_lowercase();
    if lower.contains("ssid") {
        "热点SSID"
    } else if lower.contains("hotspot") {
        "热点配置"
    } else if lower.contains("devicename") {
        "设备名称"
    } else {
        "名称"
    }
}

// ---------- 蓝牙设备表 ----------

struct BtDev {
    name: String,
    mac: String,
    last_seen: Option<i64>,
    last_conn: Option<i64>,
}

/// com.apple.MobileBluetooth.devices.plist：历史蓝牙设备。
pub fn bluetooth_plist(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) => {
            run.warn_file(f, "BT_PLIST_READ", format!("蓝牙设备表读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let Some(root) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", "蓝牙设备表解析失败".to_string())?;
        return Ok(0);
    };
    let mut devs: Vec<BtDev> = Vec::new();
    let mut locals: Vec<(String, String)> = Vec::new();
    collect_bt(&root, "", &mut devs, &mut locals, 0);

    let source = f.name().to_string();
    let mut n = 0usize;
    for (path, mac) in &locals {
        run.record_values(
            "ios.device_info",
            vec![
                path.clone().into(),
                "本机蓝牙地址".into(),
                mac.clone().into(),
                source.clone().into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    let mut seen: HashSet<String> = HashSet::new();
    for dev in devs {
        if !seen.insert(dev.mac.to_ascii_lowercase()) {
            continue;
        }
        run.record_values(
            "ios.bt_device",
            vec![
                dev.name.into(),
                dev.mac.into(),
                ts_value(dev.last_seen),
                ts_value(dev.last_conn),
                "历史设备".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    if n == 0 {
        run.warn_file(f, "BT_PLIST_EMPTY", "蓝牙设备表未解析出设备")?;
    }
    Ok(n)
}

fn collect_bt(
    v: &PValue,
    path: &str,
    devs: &mut Vec<BtDev>,
    locals: &mut Vec<(String, String)>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        PValue::Dictionary(d) => {
            let lower_path = path.to_ascii_lowercase();
            let is_local = lower_path.contains("localdevice") || lower_path.contains("controller");
            let name = find_text(d, &["name", "devicename", "displayname"]);
            let mac = find_mac(d);
            if is_local {
                // 本机地址单独进 device_info，不再重复算作历史设备。
                if let Some(mac) = &mac {
                    if !locals.iter().any(|(p, m)| p == path && m == mac) {
                        locals.push((path.to_string(), mac.clone()));
                    }
                }
            } else if let (Some(name), Some(mac)) = (name, mac) {
                if !name.is_empty() {
                    devs.push(BtDev {
                        name,
                        mac,
                        last_seen: find_time(d, &["lastseen"]),
                        last_conn: find_time(d, &["lastconn", "lastconnected", "lastconnection"]),
                    });
                    return;
                }
            }
            for (key, val) in d {
                // 兼容“字典键即 MAC、名称在值字典里”的导出结构。
                if let Some(sd) = val.as_dictionary() {
                    if looks_like_mac(key) {
                        if let Some(name) = find_text(sd, &["name", "devicename", "displayname"]) {
                            if !name.is_empty() {
                                devs.push(BtDev {
                                    name,
                                    mac: key.clone(),
                                    last_seen: find_time(sd, &["lastseen"]),
                                    last_conn: find_time(
                                        sd,
                                        &["lastconn", "lastconnected", "lastconnection"],
                                    ),
                                });
                                continue;
                            }
                        }
                    }
                }
                if matches!(val, PValue::Dictionary(_) | PValue::Array(_)) {
                    collect_bt(val, &join_path(path, key), devs, locals, depth + 1);
                }
            }
        }
        PValue::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                collect_bt(item, &format!("{path}[{i}]"), devs, locals, depth + 1);
            }
        }
        _ => {}
    }
}
