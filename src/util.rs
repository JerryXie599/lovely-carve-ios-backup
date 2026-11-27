//! 时间与格式化工具。iOS 取证里三种时间基准并存：
//! - Manifest.plist 的 plist Date（Apple epoch 2001-01-01 UTC，plist crate 换算）
//! - powerlog 的 `timestamp` 列：基准不确定（本检材实测为 Unix 秒，历史库为 Apple epoch），
//!   由 [`Epoch::detect`] 自动判别
//! - 日志行首 / CSV 里的本地墙钟时间：时区未知，输出为 local（不进时间线）

use lovelycarve_plugin::Timestamp;

pub const APPLE_EPOCH: i64 = 978_307_200; // 2001-01-01T00:00:00Z
pub const BEIJING: i32 = 8 * 60;

/// powerlog `timestamp` 列的换算基准。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Epoch {
    Unix,
    Apple,
}

impl Epoch {
    pub fn to_unix(self, raw: f64) -> i64 {
        match self {
            Epoch::Unix => raw as i64,
            Epoch::Apple => APPLE_EPOCH + raw as i64,
        }
    }

    /// 用日志库自身的取值范围判别基准：`anchor_unix` 是检材可信时间
    /// （sysdiagnose 目录名等）。中值换算落在 [2007-01-01, anchor+2年] 视为合理。
    pub fn detect(samples: &[f64], anchor_unix: i64) -> Option<Epoch> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = sorted[sorted.len() / 2];
        let lo = 1_169_094_400i64; // 2007-01-01
        let hi = anchor_unix + 2 * 365 * 86_400;
        for epoch in [Epoch::Unix, Epoch::Apple] {
            let u = epoch.to_unix(mid);
            if (lo..=hi).contains(&u) {
                return Some(epoch);
            }
        }
        None
    }
}

/// Unix 秒 → UTC+8 RFC3339（带偏移，进时间线）。
pub fn ts_plus8(unix: i64) -> Option<Timestamp> {
    civil(unix + 8 * 3600)
        .map(|(y, mo, d, h, mi, s)| Timestamp::with_offset(y, mo, d, h, mi, s, BEIJING).unwrap())
}

/// Unix 秒 → UTC RFC3339。
pub fn ts_utc(unix: i64) -> Timestamp {
    Timestamp::from_unix(unix)
}

/// 本地墙钟时间（时区未知，不进时间线）。
pub fn ts_local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<Timestamp> {
    Timestamp::local(y, mo, d, h, mi, s)
}

pub fn civil(unix: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    chrono::DateTime::from_timestamp(unix, 0).map(|dt| {
        use chrono::{Datelike, Timelike};
        (
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
        )
    })
}

/// Unix 秒 → "YYYY-MM-DD HH:MM:SS"（UTC+8 墙钟展示用）。
pub fn fmt_plus8(unix: i64) -> String {
    civil(unix + 8 * 3600)
        .map(|(y, mo, d, h, mi, s)| format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
        .unwrap_or_else(|| format!("<invalid {unix}>"))
}

pub fn day_plus8(unix: i64) -> String {
    civil(unix + 8 * 3600)
        .map(|(y, mo, d, _h, _mi, _s)| format!("{y:04}-{mo:02}-{d:02}"))
        .unwrap_or_default()
}

pub fn fmt_secs(sec: f64) -> String {
    if sec.is_finite() && sec >= 0.0 {
        format!("{sec:.0}")
    } else {
        String::new()
    }
}

/// 宽容解析 plist：个别检材的 XML 日期超出可表示范围会让解析器 panic，
/// 这里兜底为 `None`，避免单个文件拖垮整个分析器。
pub fn read_plist(bytes: &[u8]) -> Option<plist::Value> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()
    }))
    .ok()
    .flatten()
    .or_else(|| parse_bplist_lenient(bytes))
}

/// 自实现的最小二进制 plist 解析器（bplist00）。
///
/// 标准解析器遇到越界日期会拒绝整个文件（WhatsApp 组偏好、部分系统偏好等
/// 真实检材常发）；这里自行走对象表，把无法表示的日期替换为占位字符串，
/// 其余键值照常解出。
pub fn parse_bplist_lenient(bytes: &[u8]) -> Option<plist::Value> {
    if bytes.len() < 40 || !bytes.starts_with(b"bplist00") {
        return None;
    }
    let t = bytes.len() - 32;
    let offset_size = bytes[t + 6] as usize;
    let ref_size = bytes[t + 7] as usize;
    let num_objects = u64::from_be_bytes(bytes[t + 8..t + 16].try_into().ok()?) as usize;
    let top_object = u64::from_be_bytes(bytes[t + 16..t + 24].try_into().ok()?) as usize;
    let offset_table = u64::from_be_bytes(bytes[t + 24..t + 32].try_into().ok()?) as usize;
    if offset_size == 0
        || offset_size > 8
        || ref_size == 0
        || ref_size > 8
        || num_objects == 0
        || num_objects > 1_000_000
    {
        return None;
    }
    let offsets: Vec<Option<usize>> = (0..num_objects)
        .map(|i| {
            if i >= num_objects || offset_table + offset_size * i + offset_size > bytes.len() {
                return None;
            }
            let mut v = 0usize;
            for k in 0..offset_size {
                v = (v << 8) | bytes[offset_table + offset_size * i + k] as usize;
            }
            (v < bytes.len()).then_some(v)
        })
        .collect();
    fn be_int(b: &[u8], s: usize, w: usize) -> i128 {
        let mut v: i128 = 0;
        for k in 0..w.min(16) {
            v = (v << 8) | b.get(s + k).copied().unwrap_or(0) as i128;
        }
        v
    }
    fn obj(
        b: &[u8],
        offsets: &[Option<usize>],
        ref_size: usize,
        ref_id: usize,
        depth: usize,
    ) -> Option<plist::Value> {
        if depth > 48 {
            return None;
        }
        let off = (*offsets.get(ref_id)?)?;
        let marker = *b.get(off)?;
        let kind = marker >> 4;
        let mut len = (marker & 0x0F) as usize;
        let payload = off + 1;
        if std::env::var("BPLIST_TRACE").is_ok() {
            eprintln!(
                "[bplist] obj#{ref_id} off={off:#x} kind={kind:#x} len={len:#x} depth={depth}"
            );
        }
        // 低半字节 0xF = 扩展长度：紧跟一个完整 int 对象（marker + 大端值）
        let ext = len == 0xF;
        let mut payload = payload;
        if ext {
            let int_marker = *b.get(payload)?;
            let vw = 1usize << (int_marker & 0x07);
            len = be_int(b, payload + 1, vw) as usize;
            payload += 1 + vw;
        }
        obj_inner(b, offsets, ref_size, kind as usize, payload, len, depth)
    }
    fn obj_inner(
        b: &[u8],
        offsets: &[Option<usize>],
        ref_size: usize,
        kind: usize,
        payload: usize,
        len: usize,
        depth: usize,
    ) -> Option<plist::Value> {
        match kind {
            0x0 => match len {
                0x9 => Some(plist::Value::Boolean(true)),
                0x8 => Some(plist::Value::Boolean(false)),
                _ => None,
            },
            0x1 => {
                let w = 1 << len.min(3);
                Some(plist::Value::Integer((be_int(b, payload, w) as i64).into()))
            }
            0x2 => {
                let w = 1 << len.min(3);
                let mut arr = [0u8; 8];
                for k in 0..w.min(8) {
                    arr[8 - w.min(8) + k] = b.get(payload + k).copied().unwrap_or(0);
                }
                Some(plist::Value::Real(f64::from_be_bytes(arr)))
            }
            0x3 => {
                // Apple epoch 日期；越界替换为占位字符串保住其余键值
                let mut arr = [0u8; 8];
                for k in 0..8 {
                    arr[k] = b.get(payload + k).copied().unwrap_or(0);
                }
                let secs = f64::from_be_bytes(arr);
                const APPLE_EPOCH: i64 = 978_307_200;
                if !secs.is_finite() || secs.abs() > 3e9 {
                    return Some(plist::Value::String("<日期越界>".into()));
                }
                let unix = APPLE_EPOCH + secs as i64;
                let sys =
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix.max(0) as u64);
                Some(plist::Value::Date(plist::Date::from(sys)))
            }
            0x4 => Some(plist::Value::Data(
                b.get(payload..payload + len.min(b.len().saturating_sub(payload)))?
                    .to_vec(),
            )),
            0x5 => Some(plist::Value::String(
                String::from_utf8_lossy(
                    b.get(payload..payload + len.min(b.len().saturating_sub(payload)))?,
                )
                .to_string(),
            )),
            0x6 => {
                let units = len.min((b.len() - payload) / 2);
                let utf16: Vec<u16> = (0..units)
                    .map(|i| u16::from_be_bytes([b[payload + 2 * i], b[payload + 2 * i + 1]]))
                    .collect();
                Some(plist::Value::String(String::from_utf16_lossy(&utf16)))
            }
            0x8 => Some(plist::Value::Integer(
                (be_int(b, payload, len.min(8)) as i64).into(),
            )),
            0xA => {
                let mut arr = Vec::new();
                for i in 0..len {
                    let s = payload + ref_size * i;
                    if s + ref_size > b.len() {
                        return None;
                    }
                    let r = be_int(b, s, ref_size) as usize;
                    arr.push(obj(b, offsets, ref_size, r, depth + 1)?);
                }
                Some(plist::Value::Array(arr))
            }
            // Apple 实现里 dict 的 marker 高半字节是 0xD（对照 plist crate binary_reader）
            0xD => {
                let mut dict = plist::Dictionary::new();
                for i in 0..len {
                    let ks = payload + ref_size * i;
                    let vs = payload + len * ref_size + ref_size * i;
                    if ks + ref_size > b.len() || vs + ref_size > b.len() {
                        return None;
                    }
                    let k = be_int(b, ks, ref_size) as usize;
                    let v = be_int(b, vs, ref_size) as usize;
                    let key = match obj(b, offsets, ref_size, k, depth + 1)? {
                        plist::Value::String(s) => s,
                        other => format!("{other:?}"),
                    };
                    let value = obj(b, offsets, ref_size, v, depth + 1)?;
                    dict.insert(key, value);
                }
                Some(plist::Value::Dictionary(dict))
            }
            _ => None,
        }
    }
    obj(bytes, &offsets, ref_size, top_object, 0)
}

pub fn fmt_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// 常见 ProductType → 营销型号（iPhone 主要型号）。
pub fn iphone_model(product_type: &str) -> Option<&'static str> {
    Some(match product_type {
        "iPhone1,1" => "iPhone",
        "iPhone1,2" => "iPhone 3G",
        "iPhone2,1" => "iPhone 3GS",
        "iPhone3,1" | "iPhone3,2" | "iPhone3,3" => "iPhone 4",
        "iPhone4,1" => "iPhone 4S",
        "iPhone5,1" | "iPhone5,2" => "iPhone 5",
        "iPhone5,3" | "iPhone5,4" => "iPhone 5c",
        "iPhone6,1" | "iPhone6,2" => "iPhone 5s",
        "iPhone7,1" => "iPhone 6 Plus",
        "iPhone7,2" => "iPhone 6",
        "iPhone8,1" => "iPhone 6s",
        "iPhone8,2" => "iPhone 6s Plus",
        "iPhone8,4" => "iPhone SE (1st gen)",
        "iPhone9,1" | "iPhone9,3" => "iPhone 7",
        "iPhone9,2" | "iPhone9,4" => "iPhone 7 Plus",
        "iPhone10,1" | "iPhone10,4" => "iPhone 8",
        "iPhone10,2" | "iPhone10,5" => "iPhone 8 Plus",
        "iPhone10,3" | "iPhone10,6" => "iPhone X",
        "iPhone11,2" => "iPhone XS",
        "iPhone11,4" | "iPhone11,6" => "iPhone XS Max",
        "iPhone11,8" => "iPhone XR",
        "iPhone12,1" => "iPhone 11",
        "iPhone12,3" => "iPhone 11 Pro",
        "iPhone12,5" => "iPhone 11 Pro Max",
        "iPhone12,8" => "iPhone SE (2nd gen)",
        "iPhone13,1" => "iPhone 12 mini",
        "iPhone13,2" => "iPhone 12",
        "iPhone13,3" => "iPhone 12 Pro",
        "iPhone13,4" => "iPhone 12 Pro Max",
        "iPhone14,2" => "iPhone 13 Pro",
        "iPhone14,3" => "iPhone 13 Pro Max",
        "iPhone14,4" => "iPhone 13 mini",
        "iPhone14,5" => "iPhone 13",
        "iPhone14,6" => "iPhone SE (3rd gen)",
        "iPhone14,7" => "iPhone 14",
        "iPhone14,8" => "iPhone 14 Plus",
        "iPhone15,2" => "iPhone 14 Pro",
        "iPhone15,3" => "iPhone 14 Pro Max",
        "iPhone15,4" => "iPhone 15",
        "iPhone15,5" => "iPhone 15 Plus",
        "iPhone16,1" => "iPhone 15 Pro",
        "iPhone16,2" => "iPhone 15 Pro Max",
        _ => return None,
    })
}

/// 二进制值的通用识别：魔数判型；bplist 直接解析并提取可读字符串。
pub fn describe_blob(b: &[u8]) -> String {
    let hex_head = |n: usize| -> String {
        b.iter()
            .take(n)
            .map(|x| format!("{x:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    if b.starts_with(b"bplist00") {
        if let Some(v) = crate::util::read_plist(b) {
            let mut strs = Vec::new();
            collect_plist_strings(&v, &mut strs, 0);
            if !strs.is_empty() {
                let joined: String = strs.join("；").chars().take(300).collect();
                return format!("bplist({} 字节)：{}", b.len(), joined);
            }
        }
        return format!("[bplist {} 字节]", b.len());
    }
    let kind: Option<&str> = if b.starts_with(b"SQLite format 3 ") {
        Some("SQLite 数据库")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("JPEG 图片")
    } else if b.starts_with(b"\x89PNG") {
        Some("PNG 图片")
    } else if b.starts_with(b"GIF8") {
        Some("GIF 图片")
    } else if b.starts_with(b"%PDF") {
        Some("PDF 文档")
    } else if b.starts_with(b"PK") {
        Some("ZIP 容器")
    } else if b.starts_with(&[0x1F, 0x8B]) {
        Some("GZip 压缩")
    } else if b.starts_with(b"RIFF") {
        Some("RIFF 容器（WAV/WebP/AVI）")
    } else if b.len() > 11 && &b[4..8] == b"ftyp" {
        Some("ISO Media（MP4/MOV/HEIC）")
    } else if b.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("ICO 图标")
    } else if b.starts_with(b"BM") {
        Some("BMP 图片")
    } else {
        None
    };
    match kind {
        Some(k) => format!("[{k} {} 字节]", b.len()),
        None => format!("[二进制 {} 字节] 头部 {}", b.len(), hex_head(8)),
    }
}

fn collect_plist_strings(v: &plist::Value, out: &mut Vec<String>, depth: usize) {
    if out.len() >= 40 || depth > 12 {
        return;
    }
    match v {
        plist::Value::String(s) => {
            let s = s.trim();
            if s.len() > 1 && !s.starts_with('$') && !out.iter().any(|x| x == s) {
                out.push(s.chars().take(80).collect());
            }
        }
        plist::Value::Array(a) => {
            for x in a {
                collect_plist_strings(x, out, depth + 1);
            }
        }
        plist::Value::Dictionary(d) => {
            for (k, x) in d {
                collect_plist_strings(&plist::Value::String(k.clone()), out, depth + 1);
                collect_plist_strings(x, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// 按节点 ID 定位的来源引用。
///
/// 文件夹类检材的源路径解析（root+path 逐段下行）不含卷根节点名段，
/// 会报"源文件不存在"；而 nodeId（folder:<hex 路径>）在索引与实时目录中
/// 都能唯一定位。故记录来源统一保留 path 作为展示、定位走 nodeId。
pub fn node_source(f: &lovelycarve_plugin::FileRef) -> lovelycarve_plugin::SourceRef {
    let mut s = f.source();
    s.root = None;
    s
}
