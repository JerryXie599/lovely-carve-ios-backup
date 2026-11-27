//! Realm 数据库结构化解析。
//!
//! 按 Realm Core 文件格式（'T-DB'；node_header 8 字节：h[4]=标志位 bit7 inner/
//! bit6 hasrefs/bits4-3 wtype/bits2-0 width(log2)，h[5..8)=24-bit 元素数；
//! wtype=0 元素按 width/8 字节打包（小文件用窄引用），wtype=1 按字节倍数，
//! wtype=2 为原始字节 blob）递归解析：Group → 表节点（spec + 列簇）→ 各列数据。
//! 列名取自 spec 的名字 blob（NUL 分隔），输出每列的行值到 `ios.app_artifact`
//! （kind = "表名.列名"）。结构解析失败时回退为字符串表提取。
//!
//! 口径说明：列值为原始枚举/字符串（如 SendAnywhere transferType=3），
//! 不做业务语义转写；表名来自文件头部的 class_* 定义顺序。

use lovelycarve_plugin::{FileRef, Run, Value};

const MAX_BYTES: u64 = 64 << 20;
const MAX_NODES: usize = 4096;
const MAX_TABLES: usize = 32;
const MAX_VALUES: usize = 20;

struct Node {
    inner: bool,
    hasrefs: bool,
    wtype: u8,
    width: usize, // (1<<wbits)>>1 字节
    size: usize,
    data: usize,
}

fn hdr(b: &[u8], off: usize) -> Option<Node> {
    if off + 8 > b.len() {
        return None;
    }
    let h = &b[off..off + 8];
    let flags = h[4];
    Some(Node {
        inner: (flags >> 7) & 1 == 1,
        hasrefs: (flags >> 6) & 1 == 1,
        wtype: (flags >> 3) & 3,
        width: (1usize << (flags & 7)) >> 1,
        size: ((h[5] as usize) << 16) + ((h[6] as usize) << 8) + h[7] as usize,
        data: off + 8,
    })
}

fn int_at(b: &[u8], s: usize, eb: usize, signed: bool) -> i64 {
    let mut v: u64 = 0;
    for i in 0..eb.min(8) {
        v |= (b[s + i] as u64) << (8 * i);
    }
    if signed {
        v as i64
    } else {
        v as i64
    }
}

fn read_refs(b: &[u8], n: &Node) -> Vec<usize> {
    let mut out = Vec::new();
    let eb = match n.wtype {
        0 => n.width / 8,
        _ => n.width,
    };
    if eb == 0 {
        return out;
    }
    for i in 0..n.size.min(64) {
        let s = n.data + eb * i;
        if s + eb > b.len() {
            break;
        }
        out.push(int_at(b, s, eb, false) as usize);
    }
    out
}

fn leaf_ints(b: &[u8], n: &Node) -> Vec<i64> {
    let eb = match n.wtype {
        1 => n.width,
        0 => n.width / 8,
        _ => 0,
    };
    if eb == 0 || eb > 8 {
        return Vec::new();
    }
    (0..n.size.min(MAX_VALUES))
        .map(|i| int_at(b, n.data + eb * i, eb, eb == 8))
        .collect()
}

/// 字符串列：refs[偏移叶子, blob 叶子(wtype=2), 标志]。
fn col_strings(b: &[u8], col: usize) -> Option<Vec<String>> {
    let n = hdr(b, col)?;
    if n.inner || !n.hasrefs || n.size < 2 || n.size > 4 {
        return None;
    }
    let refs = read_refs(b, &n);
    let offs_node = hdr(b, *refs.first()?)?;
    let mut offs = leaf_ints(b, &offs_node);
    if offs.len() < 2 {
        return None;
    }
    let mut blob: &[u8] = &[];
    for r in refs.iter().skip(1) {
        if let Some(c) = hdr(b, *r) {
            if c.wtype == 2 {
                let end = (c.data + c.size).min(b.len());
                blob = &b[c.data..end];
            }
        }
    }
    if blob.is_empty() {
        return None;
    }
    offs.push(blob.len() as i64);
    offs.sort_unstable();
    let mut out = Vec::new();
    for w in offs.windows(2) {
        if w[0] < 0 || w[1] > blob.len() as i64 || w[1] < w[0] {
            continue;
        }
        let seg = &blob[w[0] as usize..w[1] as usize];
        out.push(
            String::from_utf8_lossy(seg)
                .trim_end_matches('\0')
                .to_string(),
        );
    }
    Some(out)
}

/// 时间戳列：refs[秒叶子, 纳秒叶子]。
fn col_timestamps(b: &[u8], col: usize) -> Option<Vec<String>> {
    let n = hdr(b, col)?;
    if n.inner || !n.hasrefs || n.size != 2 {
        return None;
    }
    let refs = read_refs(b, &n);
    let secs = leaf_ints(b, &hdr(b, *refs.first()?)?);
    let out: Vec<String> = secs
        .into_iter()
        .filter(|s| *s != i32::MAX as i64 && *s > 1_000_000_000)
        .map(crate::util::ts_plus8)
        .map(|t| t.map(|x| x.as_str().to_string()).unwrap_or_default())
        .collect();
    Some(out)
}

/// 通用列值解码：内联短字符串 / 定宽整数 / 打包位。
fn col_values(b: &[u8], col: usize) -> Vec<String> {
    let Some(n) = hdr(b, col) else {
        return Vec::new();
    };
    if n.inner || n.hasrefs {
        return Vec::new();
    }
    let eb = match n.wtype {
        1 => n.width,
        0 => n.width / 8,
        _ => 0,
    };
    if eb == 0 {
        // 子字节位打包（wtype=0，width 字段即位宽）
        if n.width == 0 {
            return vec!["0".into(); n.size.min(MAX_VALUES)];
        }
        let mut out = Vec::new();
        for i in 0..n.size.min(MAX_VALUES) {
            let mut v = 0i64;
            for j in 0..n.width.min(32) {
                let bit = i * n.width + j;
                if let Some(byte) = b.get(n.data + (bit >> 3)) {
                    v |= (((byte >> (bit & 7)) & 1) as i64) << j;
                }
            }
            out.push(v.to_string());
        }
        return out;
    }
    let mut vals = Vec::new();
    let mut all_ascii = eb >= 2;
    for i in 0..n.size.min(MAX_VALUES) {
        let s = n.data + eb * i;
        if s + eb > b.len() {
            break;
        }
        let raw = &b[s..s + eb];
        // 内联字符串槽：首 NUL 之前必须全为可打印字符（NUL 之后的字节为填充/标记）
        let nul = raw.iter().position(|c| *c == 0);
        let head = match nul {
            Some(p) => &raw[..p],
            None => raw,
        };
        let printable = !head.is_empty()
            && head.iter().all(|c| (0x20..0x7f).contains(c))
            && nul
                .map(|p| raw[p + 1..].iter().all(|c| *c == 0 || *c == 1 || *c == 2))
                .unwrap_or(true);
        if all_ascii && printable {
            let t: String = head.iter().map(|c| *c as char).collect();
            vals.push(t);
        } else {
            all_ascii = false;
            vals.push(int_at(b, s, eb.min(8), eb == 8).to_string());
        }
    }
    if !all_ascii {
        return vals;
    }
    // 8 字节整数列可能是 Double（尺寸/时间等），若全部落合理量级则按 f64 呈现
    if eb == 8 {
        let fvals: Vec<f64> = (0..n.size.min(MAX_VALUES))
            .map(|i| {
                let s = n.data + 8 * i;
                f64::from_le_bytes(b[s..s + 8].try_into().unwrap())
            })
            .collect();
        if fvals.iter().all(|v| v.is_finite() && v.abs() <= 1e10) {
            return fvals
                .into_iter()
                .map(|v| {
                    if v.fract() == 0.0 && v.abs() < 1e15 {
                        format!("{}", v as i64)
                    } else {
                        format!("{v}")
                    }
                })
                .collect();
        }
    }
    vals
}

struct Table {
    name: String,
    cols: Vec<(String, Vec<String>)>,
}

/// 从文件解析全部表（列名 + 各列行值）。
fn parse_tables(b: &[u8]) -> Vec<Table> {
    let mut seen = std::collections::HashSet::new();
    if b.len() < 16 {
        return Vec::new();
    }
    let top = u64::from_le_bytes(b[0..8].try_into().unwrap()) as usize;
    let Some(topn) = hdr(b, top) else {
        return Vec::new();
    };
    if !topn.hasrefs {
        return Vec::new();
    }
    let mut tables: Vec<Table> = Vec::new();
    // FIFO：组内子表按声明顺序发现，表名（class_*）按同一顺序对应
    let mut queue: std::collections::VecDeque<usize> = read_refs(b, &topn)
        .into_iter()
        .filter(|r| *r > 8 && *r < b.len())
        .collect();
    let class_names = class_names_from_head(b);
    let mut table_idx = 0usize;
    while let Some(off) = queue.pop_front() {
        if seen.len() > MAX_NODES || tables.len() >= MAX_TABLES {
            break;
        }
        if !seen.insert(off) {
            continue;
        }
        let Some(n) = hdr(b, off) else { continue };
        if n.inner || !n.hasrefs {
            continue;
        }
        let refs = read_refs(b, &n);
        for r in &refs {
            if *r > 8 && *r < b.len() {
                queue.push_back(*r);
            }
        }
        // 表节点：child[0]=spec（refs，且 child[1]=名字 blob 叶子，内容为 NUL 分隔列名）
        let Some(spec_ref) = refs.first().copied() else {
            continue;
        };
        let Some(spec) = hdr(b, spec_ref) else {
            continue;
        };
        if spec.inner || !spec.hasrefs || spec.size < 3 {
            continue;
        }
        let srefs = read_refs(b, &spec);
        let Some(names_ref) = srefs.get(1).copied() else {
            continue;
        };
        let Some(nn) = hdr(b, names_ref) else {
            continue;
        };
        if nn.hasrefs || nn.wtype != 1 || nn.width == 0 {
            continue;
        }
        // 名字 blob 为定长槽（槽宽 = (1<<wbits)>>1 字节），槽内 NUL 填充，
        // 部分槽首带 1 字节杂讯；按槽切分后清洗出列名。
        let slot = nn.width;
        if slot == 0 || nn.size == 0 {
            continue;
        }
        let raw_end = (nn.data + slot * nn.size).min(b.len());
        let mut names: Vec<String> = Vec::new();
        for si in 0..nn.size {
            let sl = &b[nn.data + slot * si..raw_end.min(nn.data + slot * (si + 1))];
            let t: String = sl
                .iter()
                .map(|c| *c as char)
                .skip_while(|c| !c.is_ascii_alphanumeric())
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !t.is_empty() {
                names.push(t);
            }
        }
        if names.len() < 3 {
            continue;
        }
        // 列簇 = child[2]
        let Some(cl_ref) = refs.get(2).copied() else {
            continue;
        };
        let Some(cl) = hdr(b, cl_ref) else { continue };
        if cl.inner || !cl.hasrefs {
            continue;
        }
        let crefs = read_refs(b, &cl);
        if crefs.len() < names.len() + 1 {
            continue;
        }
        let name = class_names
            .get(table_idx)
            .map(|s| s.trim_start_matches("class_").to_string())
            .unwrap_or_else(|| format!("table{table_idx}"));
        table_idx += 1;
        let mut cols = Vec::new();
        for (i, cn) in names.iter().enumerate() {
            let Some(cref) = crefs.get(i + 1).copied() else {
                continue;
            };
            let vals = col_strings(b, cref)
                .or_else(|| col_timestamps(b, cref))
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| col_values(b, cref));
            if vals.is_empty() {
                continue;
            }
            cols.push((cn.clone(), vals));
        }
        if !cols.is_empty() {
            tables.push(Table { name, cols });
        }
    }
    tables
}

/// 文件头部 4KB 内的 class_ 表名（按出现顺序）。
fn class_names_from_head(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let limit = b.len().min(0x1000);
    while i + 6 < limit {
        match b[i..limit].windows(6).position(|w| w == b"class_") {
            Some(pos) => {
                let start = i + pos;
                let end = b[start..limit]
                    .iter()
                    .position(|c| !c.is_ascii_alphanumeric() && *c != b'_')
                    .map(|p| start + p)
                    .unwrap_or(limit);
                let name = String::from_utf8_lossy(&b[start..end]).to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
                i = end;
            }
            None => break,
        }
    }
    out
}

/// 结构化解析 realm：每表每列一条 `ios.app_artifact`。
pub fn scan(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    run.check_cancelled()?;
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(b) => b,
        Err(e) => {
            run.warn_file(f, "READ", format!("realm 读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let path = f.view_path().unwrap_or(f.name());
    let app = app_of(path);
    let tables = parse_tables(&bytes);
    let mut n = 0usize;
    if tables.is_empty() {
        return fallback(run, f, &bytes, &app);
    }
    for t in tables.iter().take(MAX_TABLES) {
        for (col, vals) in t.cols.iter().take(64) {
            run.check_cancelled()?;
            let joined: String = vals.join("；").chars().take(400).collect();
            if joined.trim().is_empty() || joined == "0；0" {
                continue;
            }
            run.record_values(
                "ios.app_artifact",
                vec![
                    app.clone().into(),
                    format!("{}.{}", t.name, col).into(),
                    joined.into(),
                    path.into(),
                    "Realm 结构化解析（表.列）".into(),
                ],
            )
            .source(crate::util::node_source(f))
            .emit()?;
            n += 1;
        }
    }
    Ok(n)
}

fn app_of(path: &str) -> String {
    let p = path.to_lowercase();
    if let Some(i) = p.find("applications/") {
        let rest = &p[i + "applications/".len()..];
        if let Some(j) = rest.find('/') {
            return rest[..j].to_string();
        }
    }
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 回退：字符串表提取（结构解析失败时）。
fn fallback(run: &Run, f: &FileRef, bytes: &[u8], app: &str) -> lovelycarve_plugin::Result<usize> {
    let text = String::from_utf8_lossy(bytes);
    let mut seen = std::collections::HashSet::new();
    let mut n = 0usize;
    for token in text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '~' | '/'))
    }) {
        if token.len() < 6 || token.len() > 512 || n >= 3000 {
            continue;
        }
        if !seen.insert(token.to_string()) {
            continue;
        }
        let kind = classify(token);
        run.record_values(
            "ios.app_artifact",
            vec![
                app.to_string().into(),
                kind.into(),
                token.to_string().into(),
                f.view_path().unwrap_or(f.name()).to_string().into(),
                "Realm 字符串表提取（回退）".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

fn classify(token: &str) -> &'static str {
    if token.chars().all(|c| c.is_ascii_digit()) && (9..=16).contains(&token.len()) {
        "设备/账号标识"
    } else if token.starts_with("~/") || token.starts_with('/') || token.contains("/var/") {
        "路径"
    } else if [
        ".jpg", ".jpeg", ".png", ".gif", ".mov", ".mp4", ".pdf", ".db", ".sqlite", ".plist",
        ".mp3", ".mkv",
    ]
    .iter()
    .any(|e| token.to_lowercase().contains(e))
    {
        "文件名"
    } else if token.contains("_STATE_") || token.contains("_SUCCESS") {
        "状态常量"
    } else if token.starts_with("class_") {
        "模型定义"
    } else {
        "字符串"
    }
}
