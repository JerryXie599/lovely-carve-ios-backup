//! 媒体文件元数据（EXIF）：JPEG/HEIC 的拍摄参数、GPS、方向与哈希。
//!
//! 与 Photos.sqlite 互补：Photos.sqlite 记录相册资产，本模块直接读原始媒体文件，
//! 覆盖被剥离数据库记录的媒体（社交软件附件、文件 App 文档等）。
//!
//! 口径：EXIF 2.3 / TIFF 6.0（IFD0、Exif IFD、GPS IFD），自实现最小解析器；
//! 容器只在前 4 MiB 内定位元数据，哈希按 1 MiB 分块流式读取，避免整文件入内存。

use lovelycarve_plugin::{FileRef, Run, Value};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// SHA256 只覆盖前 16 MiB（大文件取证口径，避免整盘媒体哈希拖慢任务）。
const HASH_LIMIT: u64 = 16 * 1024 * 1024;
/// 哈希分块大小。
const HASH_CHUNK: u64 = 1024 * 1024;
/// EXIF 定位窗口：JPEG 段表与 HEIC item 的元数据都在文件开头。
const EXIF_LIMIT: u64 = 4 * 1024 * 1024;

// IFD0
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_SOFTWARE: u16 = 0x0131;
const TAG_DATETIME: u16 = 0x0132;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
// Exif IFD
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
// GPS IFD
const TAG_GPS_LAT_REF: u16 = 0x0001;
const TAG_GPS_LAT: u16 = 0x0002;
const TAG_GPS_LNG_REF: u16 = 0x0003;
const TAG_GPS_LNG: u16 = 0x0004;

/// 容器类型：只有这两种按 EXIF 解析。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Jpeg,
    Heic,
}

/// 解析结果：找到 / 没有 / 有签名但结构不可用。
enum ExifOutcome {
    Found(ExifMeta),
    Missing,
    Invalid,
}

/// 从 EXIF 提取的字段（缺省为空串 / None）。
#[derive(Default)]
struct ExifMeta {
    make: String,
    model: String,
    software: String,
    taken: String,
    orientation: Option<i64>,
    gps_lat: Option<f64>,
    gps_lng: Option<f64>,
}

/// 解析单个媒体文件并输出 `ios.media_exif`。
pub fn scan(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    run.check_cancelled()?;

    let path = f.view_path().unwrap_or(f.name()).to_string();
    let app = app_for(&path);

    // 哈希失败视为该文件不可产出可信记录：告警后跳过，不影响整批任务。
    let sha256 = match sha256_hex(f) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(f, "EXIF_FAIL", format!("SHA256 计算失败：{e}"))?;
            return Ok(0);
        }
    };

    // 仅 JPEG/HEIC 解析 EXIF；其余媒体（mov/mp4/pdf/png/gif）只输出基础字段。
    let mut meta = ExifMeta::default();
    if let Some(kind) = container_kind(f.name()) {
        match read_head(f, EXIF_LIMIT) {
            Ok(head) => match extract(&head, kind) {
                ExifOutcome::Found(m) => meta = m,
                ExifOutcome::Missing => {}
                ExifOutcome::Invalid => {
                    run.warn_file(f, "EXIF_FAIL", "EXIF 结构不完整，已跳过元数据")?;
                }
            },
            Err(e) => {
                run.warn_file(f, "EXIF_FAIL", format!("读取文件头失败：{e}"))?;
            }
        }
    }

    // 字段顺序与 ios.media_exif 注册顺序严格一致，共 12 个值。
    run.record_values(
        "ios.media_exif",
        vec![
            path.into(),
            f.name().to_string().into(),
            app.into(),
            Value::from(f.size()),
            sha256.into(),
            text_value(&meta.make),
            text_value(&meta.model),
            text_value(&meta.software),
            text_value(&meta.taken),
            meta.orientation
                .map(|v| Value::from(v))
                .unwrap_or(Value::Nil),
            meta.gps_lat.map(|v| Value::from(v)).unwrap_or(Value::Nil),
            meta.gps_lng.map(|v| Value::from(v)).unwrap_or(Value::Nil),
        ],
    )
    .source(crate::util::node_source(f))
    .emit()?;
    Ok(1)
}

/// 按路径推断来源应用/目录（大小写不敏感，先匹配专用目录再退回父目录）。
fn app_for(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_lowercase();
    let dir_hit =
        |name: &str| lower.contains(&format!("/{name}/")) || lower.starts_with(&format!("{name}/"));

    if dir_hit("dcim") {
        return "相册(DCIM)".to_string();
    }
    if lower.contains("/message/media/") {
        return "WhatsApp 媒体".to_string();
    }
    if lower.contains("/media/profile/") {
        return "WhatsApp 头像".to_string();
    }
    if lower.contains("/file provider storage/") {
        return "文件App".to_string();
    }
    if dir_hit("documents") {
        return "应用文档".to_string();
    }

    // 退回父目录名；只有文件名时用固定占位，避免空字段。
    let mut parent: Option<&str> = None;
    let mut last: Option<&str> = None;
    for seg in normalized.split('/').filter(|s| !s.is_empty()) {
        parent = last;
        last = Some(seg);
    }
    match parent {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "未知".to_string(),
    }
}

/// 扩展名判定容器；不认识的媒体返回 None（只做哈希）。
fn container_kind(name: &str) -> Option<Container> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some(Container::Jpeg)
    } else if lower.ends_with(".heic") || lower.ends_with(".heif") {
        Some(Container::Heic)
    } else {
        None
    }
}

/// 流式计算 SHA256；文件大于 64 MiB 时只算前 64 MiB。
/// 读取不完整（短读/宿主报错）时返回错误，不把残缺数据的哈希当作文件哈希。
fn sha256_hex(f: &FileRef) -> Result<String, String> {
    let limit = f.size().min(HASH_LIMIT);
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    while offset < limit {
        let want = (limit - offset).min(HASH_CHUNK) as usize;
        let chunk = f.read_at(offset, want).map_err(|e| e.to_string())?;
        if chunk.is_empty() {
            return Err(format!("读取中断于偏移 {offset}"));
        }
        hasher.update(&chunk);
        offset = offset.saturating_add(chunk.len() as u64);
    }
    if offset != limit {
        return Err(format!("读取不完整（{offset}/{limit} 字节）"));
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// 读取文件头部窗口（最多 `limit` 字节）。
fn read_head(f: &FileRef, limit: u64) -> Result<Vec<u8>, String> {
    let want = f.size().min(limit) as usize;
    f.read_at(0, want).map_err(|e| e.to_string())
}

/// 在容器头部定位并解析 EXIF。
fn extract(head: &[u8], kind: Container) -> ExifOutcome {
    let (offset, saw_signature) = match kind {
        Container::Jpeg => jpeg_exif_offset(head),
        Container::Heic => heic_exif_offset(head),
    };
    let Some(tiff_at) = offset else {
        return if saw_signature {
            ExifOutcome::Invalid
        } else {
            ExifOutcome::Missing
        };
    };
    match parse_tiff(&head[tiff_at..]) {
        Some(meta) => ExifOutcome::Found(meta),
        None => ExifOutcome::Invalid,
    }
}

/// JPEG：遍历段表，找 APP1（0xFFE1）且负载以 `Exif\0\0` 开头。
/// 返回（TIFF 头偏移，是否见过 EXIF 签名）。
fn jpeg_exif_offset(head: &[u8]) -> (Option<usize>, bool) {
    if head.len() < 4 || head[0] != 0xFF || head[1] != 0xD8 {
        return (None, false);
    }
    let mut i = 2usize;
    let mut saw = false;
    while i + 1 < head.len() {
        if head[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = head[i + 1];
        if marker == 0xFF {
            // 填充字节，继续找真正的标记。
            i += 1;
            continue;
        }
        match marker {
            // 无长度的独立标记。
            0xD8 | 0x01 | 0xD0..=0xD7 => {
                i += 2;
                continue;
            }
            // 扫描数据开始或图像结束，其后不会再有 APP1。
            0xDA | 0xD9 => break,
            _ => {}
        }
        let Some(seg_len) = read_be16(head, i + 2) else {
            break;
        };
        if seg_len < 2 {
            break;
        }
        let payload = i + 4;
        let seg_end = i + 2 + seg_len as usize;
        if marker == 0xE1 && payload + 6 <= head.len() && &head[payload..payload + 6] == b"Exif\0\0"
        {
            saw = true;
            let tiff_at = payload + 6;
            if Tiff::new(&head[tiff_at..]).is_some() {
                return (Some(tiff_at), true);
            }
        }
        if seg_end <= i || seg_end >= head.len() {
            break;
        }
        i = seg_end;
    }
    (None, saw)
}

/// HEIC/HEIF：前 4 MiB 内搜索 `Exif\0\0`，其后紧跟 TIFF 头。
/// 文件里可能存在同名字串（item 名等），因此逐个出现位置验证 TIFF 头。
fn heic_exif_offset(head: &[u8]) -> (Option<usize>, bool) {
    let needle = b"Exif\0\0";
    let mut start = 0usize;
    let mut saw = false;
    while let Some(pos) = find_bytes(&head[start..], needle) {
        let magic_at = start + pos;
        saw = true;
        let tiff_at = magic_at + needle.len();
        if Tiff::new(&head[tiff_at..]).is_some() {
            return (Some(tiff_at), true);
        }
        start = magic_at + 1;
    }
    (None, saw)
}

/// 简单子串查找（仅用于 6 字节签名的窗口内搜索）。
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read_be16(buf: &[u8], offset: usize) -> Option<u16> {
    let b = buf.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u16(buf: &[u8], offset: usize, le: bool) -> Option<u16> {
    let b = buf.get(offset..offset.checked_add(2)?)?;
    Some(if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    })
}

fn read_u32(buf: &[u8], offset: usize, le: bool) -> Option<u32> {
    let b = buf.get(offset..offset.checked_add(4)?)?;
    Some(if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    })
}

/// 单个 IFD 项；`raw` 为值区（内联 4 字节或跳转到偏移处后的实际字节）。
#[derive(Clone, Copy)]
struct Entry<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    raw: &'a [u8],
    le: bool,
}

impl Entry<'_> {
    /// 取第一个整数（BYTE/SHORT/LONG 及其有符号变体）。
    fn first_int(&self) -> Option<i64> {
        if self.count == 0 {
            return None;
        }
        match self.typ {
            1 | 7 => self.raw.first().map(|v| *v as i64),
            6 => self.raw.first().map(|v| *v as i8 as i64),
            3 => self.u16_at(0).map(|v| v as i64),
            8 => self.u16_at(0).map(|v| v as i16 as i64),
            4 | 13 => self.u32_at(0).map(|v| v as i64),
            9 => self.u32_at(0).map(|v| v as i32 as i64),
            _ => None,
        }
    }

    /// ASCII 值：截到首个 NUL，去掉首尾空白。
    fn text(&self) -> Option<String> {
        if self.typ != 2 {
            return None;
        }
        let end = self
            .raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.raw.len());
        let s = String::from_utf8_lossy(&self.raw[..end]);
        let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    /// 解析为浮点序列：RATIONAL/SRATIONAL，兼容 SHORT/LONG/FLOAT/DOUBLE。
    fn floats(&self) -> Vec<f64> {
        let count = (self.count as usize).min(8);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let value = match self.typ {
                5 | 10 => {
                    let Some(o) = i.checked_mul(8) else { break };
                    let Some(num) = read_u32(self.raw, o, self.le) else {
                        break;
                    };
                    let Some(den) = read_u32(self.raw, o + 4, self.le) else {
                        break;
                    };
                    let (num, den) = if self.typ == 10 {
                        ((num as i32) as f64, (den as i32) as f64)
                    } else {
                        (num as f64, den as f64)
                    };
                    if den == 0.0 {
                        continue;
                    }
                    num / den
                }
                3 => {
                    let Some(o) = i.checked_mul(2) else { break };
                    let Some(v) = read_u16(self.raw, o, self.le) else {
                        break;
                    };
                    v as f64
                }
                4 => {
                    let Some(o) = i.checked_mul(4) else { break };
                    let Some(v) = read_u32(self.raw, o, self.le) else {
                        break;
                    };
                    v as f64
                }
                11 => {
                    let Some(o) = i.checked_mul(4) else { break };
                    let Some(b) = self.raw.get(o..o + 4) else {
                        break;
                    };
                    let arr = [b[0], b[1], b[2], b[3]];
                    if self.le {
                        f32::from_le_bytes(arr) as f64
                    } else {
                        f32::from_be_bytes(arr) as f64
                    }
                }
                12 => {
                    let Some(o) = i.checked_mul(8) else { break };
                    let Some(b) = self.raw.get(o..o + 8) else {
                        break;
                    };
                    let arr = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
                    if self.le {
                        f64::from_le_bytes(arr)
                    } else {
                        f64::from_be_bytes(arr)
                    }
                }
                _ => break,
            };
            if value.is_finite() {
                out.push(value);
            }
        }
        out
    }

    fn u16_at(&self, offset: usize) -> Option<u16> {
        read_u16(self.raw, offset, self.le)
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        read_u32(self.raw, offset, self.le)
    }
}

/// 最小 TIFF/IFD 读取器（大小端由头两字节决定）。
struct Tiff<'a> {
    data: &'a [u8],
    le: bool,
    ifd0: usize,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let le = match data.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        if read_u16(data, 2, le)? != 42 {
            return None;
        }
        let ifd0 = read_u32(data, 4, le)? as usize;
        if ifd0.checked_add(2)? > data.len() {
            return None;
        }
        Some(Self { data, le, ifd0 })
    }

    /// 读取一个 IFD 的全部项；表被截断时保留已解析的部分。
    fn ifd(&self, offset: usize) -> Option<Vec<Entry<'a>>> {
        let count = read_u16(self.data, offset, self.le)? as usize;
        if count > 4096 {
            // 异常项数，按损坏处理，避免无界循环。
            return None;
        }
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let Some(entry_at) = offset.checked_add(2)?.checked_add(index.checked_mul(12)?) else {
                break;
            };
            if entry_at.checked_add(12)? > self.data.len() {
                break;
            }
            let Some(tag) = read_u16(self.data, entry_at, self.le) else {
                break;
            };
            let Some(typ) = read_u16(self.data, entry_at + 2, self.le) else {
                break;
            };
            let Some(item_count) = read_u32(self.data, entry_at + 4, self.le) else {
                break;
            };
            let Some(unit) = type_size(typ) else { continue };
            let Some(size) = (item_count as usize).checked_mul(unit) else {
                continue;
            };
            if item_count > 1_048_576 {
                continue;
            }
            let raw = if size <= 4 {
                // 值内联在 4 字节值区。
                self.data.get(entry_at + 8..entry_at + 8 + size)
            } else {
                let Some(value_at) = read_u32(self.data, entry_at + 8, self.le) else {
                    continue;
                };
                let value_at = value_at as usize;
                // 偏移溢出/越过缓冲区时只跳过该项，不放弃整个 IFD。
                let Some(value_end) = value_at.checked_add(size) else {
                    continue;
                };
                self.data.get(value_at..value_end)
            };
            if let Some(raw) = raw {
                entries.push(Entry {
                    tag,
                    typ,
                    count: item_count,
                    raw,
                    le: self.le,
                });
            }
        }
        Some(entries)
    }
}

fn type_size(typ: u16) -> Option<usize> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// 在 IFD 项表中按标签查找（重复标签取第一条）。
fn find_tag<'a, 'b>(entries: &'b [Entry<'a>], tag: u16) -> Option<&'b Entry<'a>> {
    entries.iter().find(|e| e.tag == tag)
}

/// 文本字段：缺失统一输出 Nil，避免空串与“无此信息”混淆。
fn text_value(s: &str) -> Value {
    if s.is_empty() {
        Value::Nil
    } else {
        Value::from(s.to_string())
    }
}

fn tag_text(entries: &[Entry<'_>], tag: u16) -> String {
    find_tag(entries, tag)
        .and_then(Entry::text)
        .unwrap_or_default()
}

/// 解析 TIFF：IFD0 → Exif IFD（拍摄时间）→ GPS IFD（经纬度）。
fn parse_tiff(data: &[u8]) -> Option<ExifMeta> {
    let tiff = Tiff::new(data)?;
    let ifd0 = tiff.ifd(tiff.ifd0)?;
    let mut meta = ExifMeta::default();

    meta.make = tag_text(&ifd0, TAG_MAKE);
    meta.model = tag_text(&ifd0, TAG_MODEL);
    meta.software = tag_text(&ifd0, TAG_SOFTWARE);
    meta.orientation = find_tag(&ifd0, TAG_ORIENTATION)
        .and_then(Entry::first_int)
        .filter(|v| (1..=8).contains(v));

    // 拍摄时间优先 Exif IFD 的 DateTimeOriginal，缺失时退回 IFD0 的 DateTime。
    if let Some(offset) = find_tag(&ifd0, TAG_EXIF_IFD).and_then(Entry::first_int) {
        if let Some(exif_ifd) = tiff.ifd(offset as usize) {
            meta.taken = tag_text(&exif_ifd, TAG_DATETIME_ORIGINAL);
        }
    }
    if meta.taken.is_empty() {
        meta.taken = tag_text(&ifd0, TAG_DATETIME);
    }

    if let Some(offset) = find_tag(&ifd0, TAG_GPS_IFD).and_then(Entry::first_int) {
        if let Some(gps_ifd) = tiff.ifd(offset as usize) {
            parse_gps(&gps_ifd, &mut meta);
        }
    }

    Some(meta)
}

/// GPS IFD：度分秒按 度 + 分/60 + 秒/3600 合成十进制度，南纬/西经取负。
fn parse_gps(entries: &[Entry<'_>], meta: &mut ExifMeta) {
    let lat_ref = tag_text(entries, TAG_GPS_LAT_REF).to_ascii_uppercase();
    let lng_ref = tag_text(entries, TAG_GPS_LNG_REF).to_ascii_uppercase();

    if let Some(v) = gps_degrees(entries, TAG_GPS_LAT, lat_ref.starts_with('S')) {
        if (-90.0..=90.0).contains(&v) {
            meta.gps_lat = Some(v);
        }
    }
    if let Some(v) = gps_degrees(entries, TAG_GPS_LNG, lng_ref.starts_with('W')) {
        if (-180.0..=180.0).contains(&v) {
            meta.gps_lng = Some(v);
        }
    }
}

fn gps_degrees(entries: &[Entry<'_>], tag: u16, negative: bool) -> Option<f64> {
    let parts = find_tag(entries, tag)?.floats();
    if parts.is_empty() {
        return None;
    }
    // 正常为 3 个 RATIONAL（度、分、秒）；只有 1~2 个时按已有分量累加。
    let mut degrees = parts[0].abs();
    if let Some(minutes) = parts.get(1) {
        degrees += minutes.abs() / 60.0;
    }
    if let Some(seconds) = parts.get(2) {
        degrees += seconds.abs() / 3600.0;
    }
    if !degrees.is_finite() {
        return None;
    }
    Some(if negative { -degrees } else { degrees })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn le_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    fn sample_tiff() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"II");
        le_u16(&mut v, 42);
        le_u32(&mut v, 8);
        // IFD0：6 项，起点 8，结束 86
        le_u16(&mut v, 6);
        le_u16(&mut v, 0x010F);
        le_u16(&mut v, 2);
        le_u32(&mut v, 4);
        v.extend_from_slice(b"Lec\0");
        le_u16(&mut v, 0x0110);
        le_u16(&mut v, 2);
        le_u32(&mut v, 4);
        v.extend_from_slice(b"Cam\0");
        le_u16(&mut v, 0x0112);
        le_u16(&mut v, 3);
        le_u32(&mut v, 1);
        le_u16(&mut v, 3);
        le_u16(&mut v, 0);
        le_u16(&mut v, 0x0131);
        le_u16(&mut v, 2);
        le_u32(&mut v, 4);
        v.extend_from_slice(b"FW1\0");
        le_u16(&mut v, 0x8769);
        le_u16(&mut v, 4);
        le_u32(&mut v, 1);
        le_u32(&mut v, 86);
        le_u16(&mut v, 0x8825);
        le_u16(&mut v, 4);
        le_u32(&mut v, 1);
        le_u32(&mut v, 104);
        le_u32(&mut v, 0);
        // Exif IFD：起点 86，1 项，结束 104
        le_u16(&mut v, 1);
        le_u16(&mut v, 0x9003);
        le_u16(&mut v, 2);
        le_u32(&mut v, 20);
        le_u32(&mut v, 158);
        le_u32(&mut v, 0);
        // GPS IFD：起点 104，4 项，结束 158
        le_u16(&mut v, 4);
        le_u16(&mut v, 0x0001);
        le_u16(&mut v, 2);
        le_u32(&mut v, 2);
        v.extend_from_slice(b"S\0\0\0");
        le_u16(&mut v, 0x0002);
        le_u16(&mut v, 5);
        le_u32(&mut v, 3);
        le_u32(&mut v, 178);
        le_u16(&mut v, 0x0003);
        le_u16(&mut v, 2);
        le_u32(&mut v, 2);
        v.extend_from_slice(b"W\0\0\0");
        le_u16(&mut v, 0x0004);
        le_u16(&mut v, 5);
        le_u32(&mut v, 3);
        le_u32(&mut v, 202);
        le_u32(&mut v, 0);
        // 数据区 158：拍摄时间 20 字节
        assert_eq!(v.len(), 158);
        v.extend_from_slice(b"2024:01:02 03:04:05\0");
        // 178：纬度 22/1, 30/1, 15/1
        for (num, den) in [(22u32, 1u32), (30, 1), (15, 1)] {
            le_u32(&mut v, num);
            le_u32(&mut v, den);
        }
        // 202：经度 114/1, 10/1, 30/1
        for (num, den) in [(114u32, 1u32), (10, 1), (30, 1)] {
            le_u32(&mut v, num);
            le_u32(&mut v, den);
        }
        v
    }

    #[test]
    fn little_endian_gps() {
        let tiff = sample_tiff();
        let meta = match parse_tiff(&tiff) {
            Some(m) => m,
            None => panic!("LE TIFF 解析失败"),
        };
        assert_eq!(meta.make, "Lec");
        assert_eq!(meta.model, "Cam");
        assert_eq!(meta.software, "FW1");
        assert_eq!(meta.taken, "2024:01:02 03:04:05");
        assert_eq!(meta.orientation, Some(3));
        let lat = meta.gps_lat.expect("纬度");
        let lng = meta.gps_lng.expect("经度");
        assert!((lat + 22.504_166_666_666_668).abs() < 1e-9, "lat={lat}");
        assert!((lng + 114.175).abs() < 1e-9, "lng={lng}");

        // JPEG：APP0 之后的 APP1 也能定位
        let tiff = sample_tiff();
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        let seg_len = 2 + 6 + tiff.len();
        jpeg.extend_from_slice(&[0xFF, 0xE1]);
        jpeg.extend_from_slice(&(seg_len as u16).to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&tiff);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        match extract(&jpeg, Container::Jpeg) {
            ExifOutcome::Found(m) => assert_eq!(m.model, "Cam"),
            _ => panic!("JPEG APP1 未解析"),
        }

        // HEIC：先出现同名 item 字串，再出现真正的 Exif 负载
        let tiff = sample_tiff();
        let mut heic = b"....Exif\0\0not-a-tiff".to_vec();
        heic.extend_from_slice(b"padding padding padding");
        heic.extend_from_slice(b"Exif\0\0");
        heic.extend_from_slice(&tiff);
        match extract(&heic, Container::Heic) {
            ExifOutcome::Found(m) => assert_eq!(m.make, "Lec"),
            _ => panic!("HEIC 第二个签名未解析"),
        }
    }

    #[test]
    fn app_mapping() {
        assert_eq!(
            app_for("var/mobile/Media/DCIM/100APPLE/a.JPG"),
            "相册(DCIM)"
        );
        assert_eq!(
            app_for("var/mobile/Containers/Shared/AppGroup/x/Message/Media/a.jpg"),
            "WhatsApp 媒体"
        );
        assert_eq!(
            app_for("var/mobile/Containers/Shared/AppGroup/x/Media/Profile/a.jpg"),
            "WhatsApp 头像"
        );
        assert_eq!(
            app_for("var/mobile/Containers/Shared/AppGroup/x/File Provider Storage/a.pdf"),
            "文件App"
        );
        assert_eq!(
            app_for("var/mobile/Containers/Data/Application/x/Documents/a.jpg"),
            "应用文档"
        );
        assert_eq!(app_for("var/mobile/Media/PhotoData/a.jpg"), "PhotoData");
        assert_eq!(app_for("a.jpg"), "未知");
    }
}
