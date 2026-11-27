//! WhatsApp iOS 底层二进制协议解码器（`ZWAMESSAGEINFO.ZRECEIPTINFO`、`ZWAMEDIAITEM` 投票/回应扩展等）。
//!
//! 兼容两代存储格式：
//! - 早期 iOS WhatsApp：`bplist00`（含直接字典及 `NSKeyedArchiver` 归档的 `d`/`r`/`p`/`u` 回执时间戳字典）
//! - 现代 iOS WhatsApp：Protobuf 编码（支持按状态分桶字段 `delivered`/`read`/`played`/`pending`
//!   及逐成员嵌套子消息，同时提取消息回应 `reactions` 与投票选择 `poll_votes` 及选项 SHA-256 哈希匹配）

use std::collections::{HashMap, HashSet};

use plist::Value as PlistValue;
use sha2::{Digest, Sha256};

use crate::util::{civil, read_plist, APPLE_EPOCH};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedReceiptPerson {
    pub jid: String,
    pub time: Option<String>,
    pub delivered_at: Option<String>,
    pub read_at: Option<String>,
    pub played_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedReaction {
    pub jid: String,
    pub emoji: String,
    pub time: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedPollVote {
    pub voter_jid: String,
    pub time: Option<String>,
    pub option_hashes: Vec<[u8; 32]>,
    pub option_indices: Vec<usize>,
    pub option_texts: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedReceiptBlob {
    pub read: Vec<ParsedReceiptPerson>,
    pub played: Vec<ParsedReceiptPerson>,
    pub delivered: Vec<ParsedReceiptPerson>,
    pub pending: Vec<ParsedReceiptPerson>,
    pub reactions: Vec<ParsedReaction>,
    pub poll_votes: Vec<ParsedPollVote>,
    pub mode: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPollDef {
    pub question: String,
    pub allow_multiple: bool,
    pub options: Vec<(String, [u8; 32])>,
}

pub fn sha256_bytes(text: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    h.finalize().into()
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// 将可能是 Apple epoch / Unix 秒 / 毫秒 / 纳秒的数字转为 UTC+8 RFC3339 字符串。
/// 过滤掉枚举小整数或越界年份（仅接受 2009-01-01 ~ 2045-01-01），防止误判状态码为时间。
pub fn norm_wa_ts(raw: f64) -> Option<String> {
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    let abs = raw.abs();
    let secs_f = if abs > 1e16 {
        raw / 1e9
    } else if abs > 1e11 {
        raw / 1000.0
    } else {
        raw
    };
    let unix = if secs_f > 1_200_000_000.0 {
        secs_f.round() as i64
    } else if secs_f > 252_460_800.0 {
        // Apple epoch: 252_460_800 = 2009-01-01
        APPLE_EPOCH + secs_f.round() as i64
    } else {
        return None;
    };
    // Reasonable range: 2009-01-01 .. 2045-01-01
    if !(1_230_768_000..=2_366_841_600).contains(&unix) {
        return None;
    }
    let (y, mo, d, h, mi, s) = civil(unix + 8 * 3600)?;
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}+08:00"))
}

fn looks_like_jid(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.len() > 160 {
        return false;
    }
    t.contains("@s.whatsapp.net")
        || t.contains("@lid")
        || t.contains("@g.us")
        || t.contains("@c.us")
        || t.contains("@broadcast")
        || (t.len() >= 7
            && t.len() <= 24
            && t.bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b':'))
}

/// 清理 WhatsApp 名称/号码中的不可见 방향控制字符（LRM/RLM/LRE/RLE/PDF）与不换行空格（NBSP）。
pub fn clean_display_text(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{2069}'
                    | '\u{feff}'
            )
        })
        .map(|c| if c == '\u{00a0}' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 判断 `ZPUSHNAME` / `ZFIRSTNAME` 是否为可展示的人类可读名称，过滤 iOS WhatsApp 写入的 Base64 Protobuf 乱码（如 `CKj9xdMGIABIAZABAPABAtgC2fSs16SGlgM=`、`+CgAQsPfA0wYgAw==`）。
pub fn is_usable_display_name(raw: &str) -> bool {
    let s = clean_display_text(raw);
    if s.is_empty() {
        return false;
    }
    if s.len() >= 4
        && !s.contains(' ')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        if s.ends_with('=') {
            return false;
        }
        if s.len() >= 10
            && (s.starts_with("+Cg")
                || s.starts_with("+EA")
                || s.contains("GIAB")
                || s.contains("GIAA"))
        {
            return false;
        }
    }
    true
}

fn b64_decode_std(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// 从 `ZWAMESSAGE`（当 `ZISFROMME = 1` 时）的 Base64 Protobuf `ZPUSHNAME` 中提取本机账户自身 LID（`field 3` = `0x1a`）。
/// 例如 `CKL8xdMGGhMxOTIyNTIwMzA4MDgxMzFAbGlkIAGqAgD4AgE=` 解码后 `field 3` 为 `"192252030808131@lid"`。
pub fn extract_jid_from_b64_proto(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() < 12 || s.contains(' ') {
        return None;
    }
    let bytes = b64_decode_std(s)?;
    let fields = decode_proto(&bytes)?;
    for (tag, val) in fields {
        if tag == 3 {
            if let WireVal::Bytes(b) = val {
                if let Ok(jid) = std::str::from_utf8(b) {
                    if looks_like_jid(jid) {
                        return Some(jid.trim().to_owned());
                    }
                }
            }
        }
    }
    None
}

/// 解码现代 iOS WhatsApp `ZRECEIPTINFO` 中 `0x8c` / `0x8x` 前缀的压缩 BCD 编码 JID：
/// 例如 `8c 19 22 52 03 08 08 13 1f` → `"192252030808131@lid"`。
pub fn decode_bcd_jid(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || bytes.len() > 16 || (bytes[0] & 0xf0) != 0x80 {
        return None;
    }
    let mut digits = String::with_capacity((bytes.len() - 1) * 2);
    for (idx, &b) in bytes[1..].iter().enumerate() {
        let hi = (b >> 4) & 0x0f;
        let lo = b & 0x0f;
        if hi <= 9 {
            digits.push((b'0' + hi) as char);
        } else {
            return None;
        }
        if lo <= 9 {
            digits.push((b'0' + lo) as char);
        } else if lo == 0x0f && idx + 2 == bytes.len() {
            // 奇数位末尾半字节 0xF 填充
        } else {
            return None;
        }
    }
    if !(7..=20).contains(&digits.len()) {
        return None;
    }
    let suffix = if bytes[0] == 0x8c {
        "@lid"
    } else {
        "@s.whatsapp.net"
    };
    Some(format!("{digits}{suffix}"))
}

fn unix_to_rfc3339_plus8(unix: i64) -> Option<String> {
    if !(1_230_768_000..=2_366_841_600).contains(&unix) {
        return None;
    }
    let (y, mo, d, h, mi, s) = civil(unix + 8 * 3600)?;
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}+08:00"))
}

fn apple_or_unix_to_unix(raw: f64) -> Option<i64> {
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    let abs = raw.abs();
    let secs_f = if abs > 1e16 {
        raw / 1e9
    } else if abs > 1e11 {
        raw / 1000.0
    } else {
        raw
    };
    let unix = if secs_f > 1_200_000_000.0 {
        secs_f.round() as i64
    } else if secs_f > 252_460_800.0 {
        APPLE_EPOCH + secs_f.round() as i64
    } else {
        return None;
    };
    (1_230_768_000..=2_366_841_600)
        .contains(&unix)
        .then_some(unix)
}

fn looks_like_emoji(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > 8 || looks_like_jid(t) {
        return false;
    }
    t.chars().any(|c| c as u32 >= 0x2000)
}

// ---------- Wire Protobuf ----------

#[derive(Clone, Debug)]
enum WireVal<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

fn read_proto_varint(buf: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut val = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(pos)?;
        pos += 1;
        val |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((val, pos));
        }
    }
    None
}

fn decode_proto(buf: &[u8]) -> Option<Vec<(u32, WireVal<'_>)>> {
    if buf.is_empty() {
        return None;
    }
    let mut pos = 0usize;
    let mut fields = Vec::new();
    while pos < buf.len() {
        let (tag, next) = read_proto_varint(buf, pos)?;
        pos = next;
        let field_num = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u8;
        if field_num == 0 {
            return None;
        }
        let val = match wire_type {
            0 => {
                let (v, n) = read_proto_varint(buf, pos)?;
                pos = n;
                WireVal::Varint(v)
            }
            1 => {
                let b: [u8; 8] = buf.get(pos..pos + 8)?.try_into().ok()?;
                pos += 8;
                WireVal::Fixed64(b)
            }
            2 => {
                let (len, n) = read_proto_varint(buf, pos)?;
                pos = n;
                let len = usize::try_from(len).ok()?;
                if len > buf.len().saturating_sub(pos) {
                    return None;
                }
                let slice = &buf[pos..pos + len];
                pos += len;
                WireVal::Bytes(slice)
            }
            5 => {
                let b: [u8; 4] = buf.get(pos..pos + 4)?.try_into().ok()?;
                pos += 4;
                WireVal::Fixed32(b)
            }
            _ => return None,
        };
        fields.push((field_num, val));
    }
    Some(fields)
}

fn wire_as_ts(val: &WireVal<'_>) -> Option<String> {
    match val {
        WireVal::Varint(v) => norm_wa_ts(*v as f64),
        WireVal::Fixed64(b) => {
            let as_f64 = f64::from_le_bytes(*b);
            if let Some(ts) = norm_wa_ts(as_f64) {
                return Some(ts);
            }
            let as_i64 = i64::from_le_bytes(*b);
            norm_wa_ts(as_i64 as f64)
        }
        _ => None,
    }
}

fn wire_as_utf8<'a>(val: &'a WireVal<'a>) -> Option<&'a str> {
    match val {
        WireVal::Bytes(b) => std::str::from_utf8(b).ok(),
        _ => None,
    }
}

// ---------- bplist00 (direct + NSKeyedArchiver) ----------

fn nskeyed_resolve<'a>(val: &'a PlistValue, objects: &'a [PlistValue]) -> &'a PlistValue {
    match val {
        PlistValue::Uid(u) => objects.get(u.get() as usize).unwrap_or(val),
        _ => val,
    }
}

fn plist_to_ts(val: &PlistValue, objects: Option<&[PlistValue]>) -> Option<String> {
    let val = objects.map_or(val, |objs| nskeyed_resolve(val, objs));
    match val {
        PlistValue::Date(d) => {
            let secs = std::time::SystemTime::from(*d)
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs() as f64;
            norm_wa_ts(secs)
        }
        PlistValue::Real(r) => norm_wa_ts(*r),
        PlistValue::Integer(i) => i.as_signed().and_then(|v| norm_wa_ts(v as f64)),
        PlistValue::Dictionary(dict) => {
            if let Some(t) = dict.get("NS.time") {
                return plist_to_ts(t, objects);
            }
            for k in [
                "time",
                "ts",
                "timestamp",
                "t",
                "read",
                "delivered",
                "played",
            ] {
                if let Some(v) = dict.get(k).and_then(|v| plist_to_ts(v, objects)) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}

fn plist_extract_dict_entries(
    val: &PlistValue,
    objects: Option<&[PlistValue]>,
) -> Vec<(String, Option<String>)> {
    let val = objects.map_or(val, |objs| nskeyed_resolve(val, objs));
    let mut out = Vec::new();
    match val {
        PlistValue::Dictionary(dict) => {
            if let (Some(keys), Some(vals), Some(objs)) =
                (dict.get("NS.keys"), dict.get("NS.objects"), objects)
            {
                if let (Some(ka), Some(va)) = (keys.as_array(), vals.as_array()) {
                    for (k, v) in ka.iter().zip(va.iter()) {
                        let rk = nskeyed_resolve(k, objs);
                        if let Some(jid) = rk.as_string() {
                            out.push((jid.to_owned(), plist_to_ts(v, objects)));
                        }
                    }
                    return out;
                }
            }
            if let (Some(objs_arr), Some(objs)) = (dict.get("NS.objects"), objects) {
                if let Some(arr) = objs_arr.as_array() {
                    for elem in arr {
                        let re = nskeyed_resolve(elem, objs);
                        if let Some(s) = re.as_string() {
                            out.push((s.to_owned(), None));
                        }
                    }
                    return out;
                }
            }
            for (k, v) in dict {
                if looks_like_jid(k) || !matches!(v, PlistValue::Dictionary(_)) {
                    out.push((k.clone(), plist_to_ts(v, objects)));
                }
            }
        }
        PlistValue::Array(arr) => {
            for item in arr {
                let item = objects.map_or(item, |objs| nskeyed_resolve(item, objs));
                if let Some(s) = item.as_string() {
                    out.push((s.to_owned(), None));
                } else if let Some(d) = item.as_dictionary() {
                    let jid = ["jid", "JID", "sender", "participant"]
                        .iter()
                        .find_map(|k| d.get(*k).and_then(|v| v.as_string()))
                        .unwrap_or("");
                    if !jid.is_empty() {
                        out.push((jid.to_owned(), plist_to_ts(item, objects)));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn decode_receipt_plist(val: &PlistValue, out: &mut ParsedReceiptBlob) {
    out.mode = "bplist";
    let Some(root_dict) = val.as_dictionary() else {
        return;
    };
    let objects = root_dict
        .get("$objects")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice);
    let top_val = if let (Some(top), Some(objs)) = (root_dict.get("$top"), objects) {
        top.as_dictionary()
            .and_then(|d| d.get("root"))
            .map(|r| nskeyed_resolve(r, objs))
            .unwrap_or(val)
    } else {
        val
    };
    let Some(top_dict) = top_val.as_dictionary() else {
        return;
    };

    // Resolve NSKeyedArchiver top NSDictionary if encoded with NS.keys/NS.objects
    let mut kv_pairs: Vec<(String, &PlistValue)> = Vec::new();
    if let (Some(keys), Some(vals), Some(objs)) = (
        top_dict.get("NS.keys").and_then(|v| v.as_array()),
        top_dict.get("NS.objects").and_then(|v| v.as_array()),
        objects,
    ) {
        for (k, v) in keys.iter().zip(vals.iter()) {
            if let Some(ks) = nskeyed_resolve(k, objs).as_string() {
                kv_pairs.push((ks.to_owned(), v));
            }
        }
    } else {
        for (k, v) in top_dict {
            kv_pairs.push((k.clone(), v));
        }
    }

    for (key, raw_v) in kv_pairs {
        let lower = key.to_ascii_lowercase();
        if matches!(lower.as_str(), "d" | "delivered" | "delivery") {
            for (jid, ts) in plist_extract_dict_entries(raw_v, objects) {
                out.delivered.push(ParsedReceiptPerson {
                    jid,
                    time: ts.clone(),
                    delivered_at: ts,
                    ..Default::default()
                });
            }
        } else if matches!(lower.as_str(), "r" | "read" | "seen") {
            for (jid, ts) in plist_extract_dict_entries(raw_v, objects) {
                out.read.push(ParsedReceiptPerson {
                    jid,
                    time: ts.clone(),
                    read_at: ts,
                    ..Default::default()
                });
            }
        } else if matches!(lower.as_str(), "p" | "played" | "play") {
            for (jid, ts) in plist_extract_dict_entries(raw_v, objects) {
                out.played.push(ParsedReceiptPerson {
                    jid,
                    time: ts.clone(),
                    played_at: ts,
                    ..Default::default()
                });
            }
        } else if matches!(lower.as_str(), "u" | "undelivered" | "pending" | "unread") {
            for (jid, ts) in plist_extract_dict_entries(raw_v, objects) {
                out.pending.push(ParsedReceiptPerson {
                    jid,
                    time: ts,
                    ..Default::default()
                });
            }
        }
    }
}

// ---------- Protobuf receipt + reaction + poll-vote decoder ----------

#[derive(Default)]
struct FlatProtoEntry {
    strings: Vec<(u32, String)>,
    timestamps: Vec<(u32, String)>,
    ints: Vec<(u32, u64)>,
    hashes32: Vec<[u8; 32]>,
    nested: Vec<(u32, Vec<u8>)>,
}

fn inspect_proto_entry<'a>(fields: &[(u32, WireVal<'a>)]) -> FlatProtoEntry {
    let mut e = FlatProtoEntry::default();
    for (num, val) in fields {
        if let Some(ts) = wire_as_ts(val) {
            e.timestamps.push((*num, ts));
        }
        match val {
            WireVal::Varint(v) => e.ints.push((*num, *v)),
            WireVal::Bytes(b) => {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    e.hashes32.push(arr);
                }
                if let Ok(s) = std::str::from_utf8(b) {
                    if !s.is_empty() && !s.bytes().any(|c| c < 0x20 && c != b'\n' && c != b'\t') {
                        e.strings.push((*num, s.to_owned()));
                        continue;
                    }
                }
                if !b.is_empty() {
                    e.nested.push((*num, (*b).to_vec()));
                }
            }
            _ => {}
        }
    }
    e
}

fn collect_hashes_and_indices_recursive(
    bytes: &[u8],
    hashes: &mut Vec<[u8; 32]>,
    indices: &mut Vec<usize>,
    texts: &mut Vec<String>,
    ts_out: &mut Option<String>,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    let Some(fields) = decode_proto(bytes) else {
        return;
    };
    for (_num, val) in &fields {
        if ts_out.is_none() {
            if let Some(ts) = wire_as_ts(val) {
                *ts_out = Some(ts);
            }
        }
        match val {
            WireVal::Varint(v) if *v < 64 => indices.push(*v as usize),
            WireVal::Bytes(b) => {
                if b.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(b);
                    hashes.push(arr);
                } else if let Ok(s) = std::str::from_utf8(b) {
                    if !s.is_empty() && !looks_like_jid(s) && s.chars().all(|c| !c.is_control()) {
                        if s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()) {
                            let mut arr = [0u8; 32];
                            let mut ok = true;
                            for i in 0..32 {
                                if let Ok(byte) = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16) {
                                    arr[i] = byte;
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                hashes.push(arr);
                                continue;
                            }
                        }
                        texts.push(s.to_owned());
                    }
                }
                if b.len() >= 2 && decode_proto(b).is_some() {
                    collect_hashes_and_indices_recursive(
                        b,
                        hashes,
                        indices,
                        texts,
                        ts_out,
                        depth + 1,
                    );
                }
            }
            _ => {}
        }
    }
}

fn resolve_offset_or_abs_ts(raw: u64, base_unix: Option<i64>) -> Option<String> {
    if let Some(abs) = norm_wa_ts(raw as f64) {
        return Some(abs);
    }
    if raw < 100_000_000 {
        if let Some(base) = base_unix {
            return unix_to_rfc3339_plus8(base + raw as i64);
        }
    }
    None
}

fn decode_receipt_protobuf(blob: &[u8], msg_date_apple: Option<f64>, out: &mut ParsedReceiptBlob) {
    let Some(top) = decode_proto(blob) else {
        return;
    };
    out.mode = "protobuf";

    // Top-level field 3 in modern iOS ZRECEIPTINFO is the base Unix timestamp (seconds);
    // fall back to the message's ZMESSAGEDATE if top-level field 3 is absent.
    let base_unix: Option<i64> = top
        .iter()
        .find_map(|(tag, val)| {
            if *tag == 3 {
                if let WireVal::Varint(v) = val {
                    return apple_or_unix_to_unix(*v as f64);
                }
            }
            None
        })
        .or_else(|| msg_date_apple.and_then(apple_or_unix_to_unix));

    // Bucket-based helper when field tag distinguishes delivered(1)/read(2)/played(3)/pending(4)
    // OR modern iOS WhatsApp per-recipient entries in field 2 with BCD JIDs and relative offsets (4=delivered, 5=read, 6=played, 9=device fanout, 10=device ack).
    for (tag, val) in &top {
        let WireVal::Bytes(sub_bytes) = val else {
            continue;
        };
        if let Some(sub_fields) = decode_proto(sub_bytes) {
            let flat = inspect_proto_entry(&sub_fields);
            let utf8_jid = flat
                .strings
                .iter()
                .find(|(_, s)| looks_like_jid(s))
                .map(|(_, s)| s.clone());
            let bcd_jid = sub_fields.iter().find_map(|(k, v)| {
                if *k == 1 {
                    if let WireVal::Bytes(b) = v {
                        return decode_bcd_jid(b);
                    }
                }
                None
            });

            let has_rel_or_device_fields = sub_fields.iter().any(|(k, v)| {
                matches!(
                    (*k, v),
                    (4 | 5 | 6, WireVal::Varint(_)) | (9 | 10, WireVal::Bytes(_))
                )
            });

            if bcd_jid.is_some() || (*tag == 2 && has_rel_or_device_fields) {
                if let Some(jid_str) = bcd_jid.or_else(|| utf8_jid.clone()) {
                    let find_offset_ts = |want_tag: u32| -> (bool, Option<String>) {
                        for (k, v) in &sub_fields {
                            if *k == want_tag {
                                if let WireVal::Varint(n) = v {
                                    return (true, resolve_offset_or_abs_ts(*n, base_unix));
                                }
                            }
                        }
                        (false, None)
                    };
                    let (has_del, del_ts) = find_offset_ts(4);
                    let (has_read, read_ts) = find_offset_ts(5);
                    let (has_play, play_ts) = find_offset_ts(6);

                    if has_play {
                        out.played.push(ParsedReceiptPerson {
                            jid: jid_str,
                            time: play_ts
                                .clone()
                                .or_else(|| read_ts.clone())
                                .or_else(|| del_ts.clone()),
                            delivered_at: del_ts,
                            read_at: read_ts,
                            played_at: play_ts,
                            ..Default::default()
                        });
                    } else if has_read {
                        out.read.push(ParsedReceiptPerson {
                            jid: jid_str,
                            time: read_ts.clone().or_else(|| del_ts.clone()),
                            delivered_at: del_ts,
                            read_at: read_ts,
                            ..Default::default()
                        });
                    } else if has_del {
                        out.delivered.push(ParsedReceiptPerson {
                            jid: jid_str,
                            time: del_ts.clone(),
                            delivered_at: del_ts,
                            ..Default::default()
                        });
                    } else {
                        out.pending.push(ParsedReceiptPerson {
                            jid: jid_str,
                            ..Default::default()
                        });
                    }
                    continue;
                }
            }

            // Check if this submessage is a map entry or direct recipient entry: (jid, ts)
            if let Some(jid_str) = utf8_jid {
                let first_ts = flat.timestamps.first().map(|(_, t)| t.clone());
                match *tag {
                    1 => out.delivered.push(ParsedReceiptPerson {
                        jid: jid_str,
                        time: first_ts.clone(),
                        delivered_at: first_ts,
                        ..Default::default()
                    }),
                    2 => out.read.push(ParsedReceiptPerson {
                        jid: jid_str,
                        time: first_ts.clone(),
                        read_at: first_ts,
                        ..Default::default()
                    }),
                    3 => out.played.push(ParsedReceiptPerson {
                        jid: jid_str,
                        time: first_ts.clone(),
                        played_at: first_ts,
                        ..Default::default()
                    }),
                    4 => out.pending.push(ParsedReceiptPerson {
                        jid: jid_str,
                        time: first_ts,
                        ..Default::default()
                    }),
                    5 | 6 | 7 if !flat.strings.is_empty() => {
                        if let Some((_, emoji)) =
                            flat.strings.iter().find(|(_, s)| looks_like_emoji(s))
                        {
                            out.reactions.push(ParsedReaction {
                                jid: jid_str,
                                emoji: emoji.clone(),
                                time: first_ts,
                            });
                        } else if !flat.hashes32.is_empty() || *tag >= 8 {
                            out.poll_votes.push(ParsedPollVote {
                                voter_jid: jid_str,
                                time: first_ts,
                                option_hashes: flat.hashes32.clone(),
                                ..Default::default()
                            });
                        }
                    }
                    _ => {
                        // Nested poll votes or multi-timestamp per-recipient entries
                        let mut hashes = flat.hashes32.clone();
                        let mut indices = Vec::new();
                        let mut texts = Vec::new();
                        let mut vote_ts = first_ts.clone();
                        for (_, n) in &flat.nested {
                            collect_hashes_and_indices_recursive(
                                n,
                                &mut hashes,
                                &mut indices,
                                &mut texts,
                                &mut vote_ts,
                                0,
                            );
                        }
                        if !hashes.is_empty() {
                            out.poll_votes.push(ParsedPollVote {
                                voter_jid: jid_str,
                                time: vote_ts,
                                option_hashes: hashes,
                                option_indices: indices,
                                option_texts: texts,
                            });
                        } else if flat.timestamps.len() >= 2 {
                            // Heuristic per-recipient record carrying multiple timestamps
                            let del_ts = flat
                                .timestamps
                                .iter()
                                .find(|(k, _)| *k == 2 || *k == 1)
                                .map(|(_, t)| t.clone());
                            let read_ts = flat
                                .timestamps
                                .iter()
                                .find(|(k, _)| *k == 3 || *k == 2)
                                .map(|(_, t)| t.clone());
                            let play_ts = flat
                                .timestamps
                                .iter()
                                .find(|(k, _)| *k == 4)
                                .map(|(_, t)| t.clone());
                            if let Some(pt) = play_ts {
                                out.played.push(ParsedReceiptPerson {
                                    jid: jid_str,
                                    time: Some(pt.clone()),
                                    delivered_at: del_ts,
                                    read_at: read_ts,
                                    played_at: Some(pt),
                                    ..Default::default()
                                });
                            } else if let Some(rt) = read_ts {
                                out.read.push(ParsedReceiptPerson {
                                    jid: jid_str,
                                    time: Some(rt.clone()),
                                    delivered_at: del_ts,
                                    read_at: Some(rt),
                                    ..Default::default()
                                });
                            } else if let Some(dt) = del_ts {
                                out.delivered.push(ParsedReceiptPerson {
                                    jid: jid_str,
                                    time: Some(dt.clone()),
                                    delivered_at: Some(dt),
                                    ..Default::default()
                                });
                            }
                        } else if let Some((_, emoji)) =
                            flat.strings.iter().find(|(_, s)| looks_like_emoji(s))
                        {
                            out.reactions.push(ParsedReaction {
                                jid: jid_str,
                                emoji: emoji.clone(),
                                time: first_ts,
                            });
                        }
                    }
                }
            } else {
                // Submessage might be a repeated wrapper containing nested entries (e.g. map inside container)
                for (sub_tag, sub_val) in &sub_fields {
                    let WireVal::Bytes(inner) = sub_val else {
                        continue;
                    };
                    if let Some(inner_fields) = decode_proto(inner) {
                        let inner_flat = inspect_proto_entry(&inner_fields);
                        if let Some(jid_str) = inner_flat
                            .strings
                            .iter()
                            .find(|(_, s)| looks_like_jid(s))
                            .map(|(_, s)| s.clone())
                        {
                            let ts = inner_flat.timestamps.first().map(|(_, t)| t.clone());
                            if let Some((_, emoji)) =
                                inner_flat.strings.iter().find(|(_, s)| looks_like_emoji(s))
                            {
                                out.reactions.push(ParsedReaction {
                                    jid: jid_str,
                                    emoji: emoji.clone(),
                                    time: ts,
                                });
                            } else if !inner_flat.hashes32.is_empty()
                                || !inner_flat.nested.is_empty()
                            {
                                let mut hashes = inner_flat.hashes32.clone();
                                let mut indices = Vec::new();
                                let mut texts = Vec::new();
                                let mut vote_ts = ts.clone();
                                for (_, n) in &inner_flat.nested {
                                    collect_hashes_and_indices_recursive(
                                        n,
                                        &mut hashes,
                                        &mut indices,
                                        &mut texts,
                                        &mut vote_ts,
                                        0,
                                    );
                                }
                                if !hashes.is_empty() || !texts.is_empty() {
                                    out.poll_votes.push(ParsedPollVote {
                                        voter_jid: jid_str,
                                        time: vote_ts,
                                        option_hashes: hashes,
                                        option_indices: indices,
                                        option_texts: texts,
                                    });
                                    continue;
                                }
                            } else {
                                let bucket = if *tag <= 4 { *tag } else { *sub_tag };
                                match bucket {
                                    1 => out.delivered.push(ParsedReceiptPerson {
                                        jid: jid_str,
                                        time: ts.clone(),
                                        delivered_at: ts,
                                        ..Default::default()
                                    }),
                                    2 => out.read.push(ParsedReceiptPerson {
                                        jid: jid_str,
                                        time: ts.clone(),
                                        read_at: ts,
                                        ..Default::default()
                                    }),
                                    3 => out.played.push(ParsedReceiptPerson {
                                        jid: jid_str,
                                        time: ts.clone(),
                                        played_at: ts,
                                        ..Default::default()
                                    }),
                                    4 => out.pending.push(ParsedReceiptPerson {
                                        jid: jid_str,
                                        time: ts,
                                        ..Default::default()
                                    }),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn decode_receipt_info(blob: &[u8]) -> ParsedReceiptBlob {
    decode_receipt_info_with_base(blob, None)
}

pub fn decode_receipt_info_with_base(
    blob: &[u8],
    msg_date_apple: Option<f64>,
) -> ParsedReceiptBlob {
    let mut out = ParsedReceiptBlob::default();
    if blob.is_empty() {
        return out;
    }
    if blob.starts_with(b"bplist") || blob.starts_with(b"<?xml") || blob.starts_with(b"<plist") {
        if let Some(pl) = read_plist(blob) {
            decode_receipt_plist(&pl, &mut out);
        }
    } else {
        decode_receipt_protobuf(blob, msg_date_apple, &mut out);
    }
    dedupe_receipts(&mut out);
    out
}

fn dedupe_receipts(out: &mut ParsedReceiptBlob) {
    let merge = |list: &mut Vec<ParsedReceiptPerson>| {
        let mut by_jid: HashMap<String, usize> = HashMap::new();
        let mut compact: Vec<ParsedReceiptPerson> = Vec::new();
        for item in list.drain(..) {
            let key = canonical_jid(&item.jid);
            if let Some(&idx) = by_jid.get(&key) {
                let dst = &mut compact[idx];
                if dst.time.is_none() {
                    dst.time = item.time;
                }
                if dst.delivered_at.is_none() {
                    dst.delivered_at = item.delivered_at;
                }
                if dst.read_at.is_none() {
                    dst.read_at = item.read_at;
                }
                if dst.played_at.is_none() {
                    dst.played_at = item.played_at;
                }
            } else {
                by_jid.insert(key, compact.len());
                compact.push(item);
            }
        }
        *list = compact;
    };
    merge(&mut out.played);
    merge(&mut out.read);
    merge(&mut out.delivered);
    merge(&mut out.pending);
}

pub fn canonical_jid(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((left, right)) = trimmed.split_once('@') {
        let bare = left.split(':').next().unwrap_or(left);
        format!(
            "{}@{}",
            bare.to_ascii_lowercase(),
            right.to_ascii_lowercase()
        )
    } else {
        trimmed
            .split(':')
            .next()
            .unwrap_or(trimmed)
            .to_ascii_lowercase()
    }
}

fn collect_proto_poll_strings(
    bytes: &[u8],
    out_texts: &mut Vec<String>,
    allow_mult: &mut bool,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    let Some(fields) = decode_proto(bytes) else {
        return;
    };
    for (tag, val) in fields {
        match val {
            WireVal::Varint(v) if tag == 2 || tag == 3 => {
                // selectableOptionsCount: 0 = unlimited/multiple, 1 = single, >1 = multiple
                if v == 0 || v > 1 {
                    *allow_mult = true;
                }
            }
            WireVal::Bytes(b) => {
                if let Ok(s) = std::str::from_utf8(b) {
                    let t = s.trim();
                    if !t.is_empty()
                        && !t.chars().any(|c| c.is_control() && c != '\n')
                        && !looks_like_jid(t)
                    {
                        out_texts.push(t.to_owned());
                        continue;
                    }
                }
                if b.len() >= 2 {
                    collect_proto_poll_strings(b, out_texts, allow_mult, depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// 解码投票主题与候选项定义（兼容 Protobuf `ZMETADATA` / `bplist00` 及常见文本序列化）。
pub fn decode_poll_definition(
    ztext: &str,
    media_title: &str,
    metadata_blob: Option<&[u8]>,
) -> Option<ParsedPollDef> {
    let mut question = String::new();
    let mut option_texts: Vec<String> = Vec::new();
    let mut allow_multiple = false;

    if let Some(blob) = metadata_blob.filter(|b| !b.is_empty()) {
        if blob.starts_with(b"bplist") || blob.starts_with(b"<?xml") {
            if let Some(PlistValue::Dictionary(d)) = read_plist(blob) {
                if let Some(q) = d
                    .get("name")
                    .or_else(|| d.get("question"))
                    .and_then(|v| v.as_string())
                {
                    question = q.trim().to_owned();
                }
                if let Some(PlistValue::Array(opts)) = d.get("options") {
                    for o in opts {
                        if let Some(s) = o.as_string() {
                            option_texts.push(s.trim().to_owned());
                        } else if let Some(od) = o.as_dictionary() {
                            if let Some(s) = od
                                .get("name")
                                .or_else(|| od.get("text"))
                                .and_then(|v| v.as_string())
                            {
                                option_texts.push(s.trim().to_owned());
                            }
                        }
                    }
                }
                if let Some(sel) = d
                    .get("selectableOptionsCount")
                    .and_then(|v| v.as_signed_integer())
                {
                    allow_multiple = sel == 0 || sel > 1;
                }
            }
        } else {
            let mut extracted = Vec::new();
            collect_proto_poll_strings(blob, &mut extracted, &mut allow_multiple, 0);
            if !extracted.is_empty() {
                question = extracted[0].clone();
                option_texts.extend(extracted.into_iter().skip(1));
            }
        }
    }

    if question.is_empty() {
        let src = if !media_title.trim().is_empty() {
            media_title.trim()
        } else {
            ztext.trim()
        };
        let mut lines: Vec<String> = src
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| l.trim_start_matches(['-', '*', '•', ' ']).trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        if !lines.is_empty() {
            question = lines.remove(0);
            if option_texts.is_empty() && !lines.is_empty() {
                option_texts = lines;
            }
        }
    }

    if question.is_empty() && option_texts.is_empty() {
        return None;
    }

    let mut seen = HashSet::new();
    let mut options = Vec::new();
    for text in option_texts {
        if !text.is_empty() && seen.insert(text.clone()) {
            let hash = sha256_bytes(&text);
            options.push((text, hash));
        }
    }

    Some(ParsedPollDef {
        question,
        allow_multiple,
        options,
    })
}

/// 从投票更新消息（如 `ZMESSAGETYPE = 66` 或附加二进制列）里提取选中项与投票者。
pub fn decode_poll_vote_payload(
    voter_jid: &str,
    ts: Option<String>,
    blob: Option<&[u8]>,
    text_hint: &str,
) -> Option<ParsedPollVote> {
    let mut hashes = Vec::new();
    let mut indices = Vec::new();
    let mut texts = Vec::new();
    let mut out_ts = ts;

    if let Some(b) = blob.filter(|b| !b.is_empty()) {
        collect_hashes_and_indices_recursive(
            b,
            &mut hashes,
            &mut indices,
            &mut texts,
            &mut out_ts,
            0,
        );
    }
    if texts.is_empty() && hashes.is_empty() && !text_hint.trim().is_empty() {
        texts.push(text_hint.trim().to_owned());
    }
    if hashes.is_empty() && indices.is_empty() && texts.is_empty() {
        return None;
    }
    Some(ParsedPollVote {
        voter_jid: voter_jid.to_owned(),
        time: out_ts,
        option_hashes: hashes,
        option_indices: indices,
        option_texts: texts,
    })
}
