//! 纯 Rust SQLite `-wal` + `-shm` 底层帧、B-Tree 页与空闲块（Freeblock）取证解析器。
//!
//! 当 SQLite 日志包含以下内容时，标准 `sqlite3` 引擎只能读到“最后一次已提交状态”：
//! - 已被后续事务 `DELETE` 覆盖的历史版本（Committed WAL 历史帧，如照片先被删入最近删除又被永久清空）
//! - `-wal` 尾部尚未写入 commit 标记（`db_size == 0`）或尚未被 `-shm` `mxFrame` 索引的未提交帧
//! - 曾被重置 salt 后残留在 `-wal` 尾部的旧 Salt 历史残留帧
//! - B-Tree 叶子页 Freeblock（空闲块链表）里残留的已删行记录字节
//!
//! 本模块不调用 C `sqlite3`、零外部依赖，直接结合 DB + `-wal` + `-shm` 还原每一帧状态与表格历史行。

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalFrameStatus {
    BaseDb,
    CommittedWal,
    UncommittedWal,
    PriorSaltResidual,
    BtreeFreeblock,
}

impl WalFrameStatus {
    pub fn zh_label(self) -> &'static str {
        match self {
            Self::BaseDb => "基础库页面",
            Self::CommittedWal => "已提交 WAL 历史帧",
            Self::UncommittedWal => "未提交 WAL 帧",
            Self::PriorSaltResidual => "旧 Salt 残留帧",
            Self::BtreeFreeblock => "B-Tree 空闲块恢复",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    Float(f64),
    Blob(Vec<u8>),
    Text(String),
}

impl SqlValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Float(f) if f.is_finite() => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) if f.is_finite() => Some(*f),
            Self::Int(n) => Some(*n as f64),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DbHeader {
    pub page_size: usize,
    pub usable_size: usize,
    pub total_pages: usize,
}

#[derive(Clone, Debug)]
pub struct ShmInfo {
    pub version: u32,
    pub is_init: u8,
    pub big_end_cksum: bool,
    pub page_size: usize,
    pub mx_frame: u32,
    pub n_page: u32,
    pub salt1: u32,
    pub salt2: u32,
    pub n_backfill: u32,
    pub valid: bool,
}

#[derive(Clone, Debug)]
pub struct WalInfo {
    pub big_end_cksum: bool,
    pub format_version: u32,
    pub page_size: usize,
    pub ckpt_seq: u32,
    pub salt: [u8; 8],
    pub salt1: u32,
    pub salt2: u32,
    pub hdr_cksum_valid: bool,
}

#[derive(Clone, Debug)]
pub struct WalFrame<'a> {
    pub frame_no: u32,
    pub pgno: u32,
    pub db_size: u32,
    pub salt: [u8; 8],
    pub commit_no: u32,
    pub status: WalFrameStatus,
    pub page: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct WalShmInspection<'a> {
    pub db: DbHeader,
    pub wal: Option<WalInfo>,
    pub shm: Option<ShmInfo>,
    pub frames: Vec<WalFrame<'a>>,
    pub committed_frames: usize,
    pub uncommitted_frames: usize,
    pub prior_salt_frames: usize,
    pub commit_count: u32,
}

#[derive(Clone, Debug)]
pub struct HistoricalRow {
    pub rowid: i64,
    pub frame_no: u32,
    pub commit_no: u32,
    pub pgno: u32,
    pub status: WalFrameStatus,
    pub is_freeblock: bool,
    pub cols: HashMap<String, SqlValue>,
}

#[derive(Clone, Debug)]
pub struct TableMeta {
    pub root_page: u32,
    pub columns: Vec<String>,
    pub integer_pk_col: Option<usize>,
}

pub fn parse_db_header(db: &[u8]) -> Option<DbHeader> {
    if db.len() < 100 || !db.starts_with(b"SQLite format 3\0") {
        return None;
    }
    let raw_ps = u16::from_be_bytes([db[16], db[17]]);
    let page_size = if raw_ps == 1 { 65_536 } else { raw_ps as usize };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return None;
    }
    let reserved = db[20] as usize;
    let usable_size = page_size.saturating_sub(reserved);
    let total_pages = db.len() / page_size;
    Some(DbHeader {
        page_size,
        usable_size,
        total_pages,
    })
}

pub fn parse_shm_info(shm: &[u8]) -> Option<ShmInfo> {
    if shm.len() < 104 {
        return None;
    }
    let read_hdr = |off: usize| -> Option<(u32, u8, bool, usize, u32, u32, u32, u32)> {
        if shm.len() < off + 48 {
            return None;
        }
        let ver = u32::from_ne_bytes(shm[off..off + 4].try_into().ok()?);
        let is_init = shm[off + 12];
        let big_end = shm[off + 13] != 0;
        let sz_raw = u16::from_ne_bytes(shm[off + 14..off + 16].try_into().ok()?);
        let ps = if sz_raw == 1 { 65_536 } else { sz_raw as usize };
        let mx_frame = u32::from_ne_bytes(shm[off + 16..off + 20].try_into().ok()?);
        let n_page = u32::from_ne_bytes(shm[off + 20..off + 24].try_into().ok()?);
        // Salt in WAL-index matches the 8 bytes stored directly from WAL header.
        let salt1 = u32::from_be_bytes(shm[off + 32..off + 36].try_into().ok()?);
        let salt2 = u32::from_be_bytes(shm[off + 36..off + 40].try_into().ok()?);
        Some((ver, is_init, big_end, ps, mx_frame, n_page, salt1, salt2))
    };
    let h1 = read_hdr(0)?;
    let h2 = read_hdr(48);
    let valid = h1.0 == 3_007_000 && h1.1 == 1 && h2.is_none_or(|b| b == h1);
    let n_backfill = u32::from_ne_bytes(shm[96..100].try_into().ok()?);
    Some(ShmInfo {
        version: h1.0,
        is_init: h1.1,
        big_end_cksum: h1.2,
        page_size: h1.3,
        mx_frame: h1.4,
        n_page: h1.5,
        salt1: h1.6,
        salt2: h1.7,
        n_backfill,
        valid,
    })
}

fn wal_checksum(big_end: bool, data: &[u8], init: (u32, u32)) -> (u32, u32) {
    let mut s0 = init.0;
    let mut s1 = init.1;
    let mut i = 0;
    while i + 8 <= data.len() {
        let w0: [u8; 4] = data[i..i + 4].try_into().unwrap();
        let w1: [u8; 4] = data[i + 4..i + 8].try_into().unwrap();
        let x0 = if big_end {
            u32::from_be_bytes(w0)
        } else {
            u32::from_le_bytes(w0)
        };
        let x1 = if big_end {
            u32::from_be_bytes(w1)
        } else {
            u32::from_le_bytes(w1)
        };
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
        i += 8;
    }
    (s0, s1)
}

pub fn inspect_wal_shm<'a>(
    db: &[u8],
    wal: Option<&'a [u8]>,
    shm: Option<&[u8]>,
) -> Option<WalShmInspection<'a>> {
    let db_hdr = parse_db_header(db)?;
    let shm_info = shm.and_then(parse_shm_info);

    let Some(wal_bytes) = wal.filter(|w| w.len() >= 32) else {
        return Some(WalShmInspection {
            db: db_hdr,
            wal: None,
            shm: shm_info,
            frames: Vec::new(),
            committed_frames: 0,
            uncommitted_frames: 0,
            prior_salt_frames: 0,
            commit_count: 0,
        });
    };

    let magic = u32::from_be_bytes(wal_bytes[0..4].try_into().ok()?);
    let big_end_cksum = match magic {
        0x377f_0682 => false,
        0x377f_0683 => true,
        _ => {
            return Some(WalShmInspection {
                db: db_hdr,
                wal: None,
                shm: shm_info,
                frames: Vec::new(),
                committed_frames: 0,
                uncommitted_frames: 0,
                prior_salt_frames: 0,
                commit_count: 0,
            });
        }
    };
    let format_version = u32::from_be_bytes(wal_bytes[4..8].try_into().ok()?);
    let wal_page_size = u32::from_be_bytes(wal_bytes[8..12].try_into().ok()?) as usize;
    let page_size = if wal_page_size >= 512 && wal_page_size.is_power_of_two() {
        wal_page_size
    } else {
        db_hdr.page_size
    };
    let ckpt_seq = u32::from_be_bytes(wal_bytes[12..16].try_into().ok()?);
    let salt: [u8; 8] = wal_bytes[16..24].try_into().ok()?;
    let salt1 = u32::from_be_bytes(salt[0..4].try_into().ok()?);
    let salt2 = u32::from_be_bytes(salt[4..8].try_into().ok()?);
    let expected_hdr_cksum = (
        u32::from_be_bytes(wal_bytes[24..28].try_into().ok()?),
        u32::from_be_bytes(wal_bytes[28..32].try_into().ok()?),
    );
    let computed_hdr_cksum = wal_checksum(big_end_cksum, &wal_bytes[0..24], (0, 0));
    let hdr_cksum_valid = computed_hdr_cksum == expected_hdr_cksum;

    let wal_info = WalInfo {
        big_end_cksum,
        format_version,
        page_size,
        ckpt_seq,
        salt,
        salt1,
        salt2,
        hdr_cksum_valid,
    };

    // Determine effective SHM mxFrame cap only if SHM is valid and matches current WAL salt.
    let shm_mx_cap = shm_info
        .as_ref()
        .filter(|s| s.valid && s.salt1 == salt1 && s.salt2 == salt2 && s.mx_frame > 0)
        .map(|s| s.mx_frame);

    let frame_stride = 24 + page_size;
    let mut frames = Vec::new();
    let mut off = 32usize;
    let mut frame_no = 0u32;
    let mut rolling_cksum = computed_hdr_cksum;
    let mut chain_ok = hdr_cksum_valid;
    let mut pending: Vec<usize> = Vec::new();
    let mut commit_no = 0u32;

    while off + frame_stride <= wal_bytes.len() {
        frame_no += 1;
        let fh = &wal_bytes[off..off + 24];
        let page = &wal_bytes[off + 24..off + frame_stride];
        off += frame_stride;

        let pgno = u32::from_be_bytes(fh[0..4].try_into().unwrap());
        if pgno == 0 {
            continue;
        }
        let db_size = u32::from_be_bytes(fh[4..8].try_into().unwrap());
        let f_salt: [u8; 8] = fh[8..16].try_into().unwrap();
        let f_cksum = (
            u32::from_be_bytes(fh[16..20].try_into().unwrap()),
            u32::from_be_bytes(fh[20..24].try_into().unwrap()),
        );

        let idx = frames.len();
        if f_salt != salt {
            chain_ok = false;
            frames.push(WalFrame {
                frame_no,
                pgno,
                db_size,
                salt: f_salt,
                commit_no: 0,
                status: WalFrameStatus::PriorSaltResidual,
                page,
            });
            continue;
        }

        let mut frame_valid = false;
        if chain_ok {
            let c1 = wal_checksum(big_end_cksum, &fh[0..8], rolling_cksum);
            let c2 = wal_checksum(big_end_cksum, page, c1);
            if c2 == f_cksum {
                rolling_cksum = c2;
                frame_valid = true;
            } else {
                chain_ok = false;
            }
        }

        frames.push(WalFrame {
            frame_no,
            pgno,
            db_size,
            salt: f_salt,
            commit_no: 0,
            status: WalFrameStatus::UncommittedWal,
            page,
        });

        if frame_valid {
            pending.push(idx);
            if db_size > 0 {
                commit_no += 1;
                let within_shm = shm_mx_cap.is_none_or(|mx| frame_no <= mx);
                let tx_status = if within_shm {
                    WalFrameStatus::CommittedWal
                } else {
                    WalFrameStatus::UncommittedWal
                };
                for &p_idx in &pending {
                    frames[p_idx].commit_no = commit_no;
                    frames[p_idx].status = tx_status;
                }
                pending.clear();
            }
        }
    }

    let mut committed_frames = 0usize;
    let mut uncommitted_frames = 0usize;
    let mut prior_salt_frames = 0usize;
    for fr in &frames {
        match fr.status {
            WalFrameStatus::CommittedWal => committed_frames += 1,
            WalFrameStatus::UncommittedWal => uncommitted_frames += 1,
            WalFrameStatus::PriorSaltResidual => prior_salt_frames += 1,
            WalFrameStatus::BaseDb | WalFrameStatus::BtreeFreeblock => {}
        }
    }

    Some(WalShmInspection {
        db: db_hdr,
        wal: Some(wal_info),
        shm: shm_info,
        frames,
        committed_frames,
        uncommitted_frames,
        prior_salt_frames,
        commit_count: commit_no,
    })
}

pub fn read_varint(buf: &[u8], pos: usize) -> Option<(i64, usize)> {
    let mut val = 0u64;
    let mut p = pos;
    for i in 0..8 {
        let b = *buf.get(p)?;
        p += 1;
        val = (val << 7) | (b & 0x7f) as u64;
        if b < 0x80 {
            return Some((val as i64, p));
        }
        if i == 7 {
            let b9 = *buf.get(p)?;
            p += 1;
            val = (val << 8) | b9 as u64;
            return Some((val as i64, p));
        }
    }
    None
}

fn read_signed_be(bytes: &[u8]) -> i64 {
    let sign = if bytes[0] & 0x80 != 0 { -1i64 } else { 0i64 };
    let mut v = sign;
    for &b in bytes {
        v = (v << 8) | b as i64;
    }
    v
}

pub fn decode_record(payload: &[u8]) -> Option<Vec<SqlValue>> {
    let (hdr_len_i64, mut pos) = read_varint(payload, 0)?;
    if hdr_len_i64 <= 0 || hdr_len_i64 as usize > payload.len() {
        return None;
    }
    let hdr_end = hdr_len_i64 as usize;
    let mut serial_types = Vec::new();
    while pos < hdr_end {
        let (st, next) = read_varint(payload, pos)?;
        if st < 0 {
            return None;
        }
        serial_types.push(st as u64);
        pos = next;
    }
    if pos != hdr_end {
        return None;
    }

    let mut data_off = hdr_end;
    let mut values = Vec::with_capacity(serial_types.len());
    for st in serial_types {
        let val = match st {
            0 => SqlValue::Null,
            1 => {
                let b = *payload.get(data_off)? as i8;
                data_off += 1;
                SqlValue::Int(b as i64)
            }
            2 => {
                let b = payload.get(data_off..data_off + 2)?;
                data_off += 2;
                SqlValue::Int(i16::from_be_bytes([b[0], b[1]]) as i64)
            }
            3 => {
                let b = payload.get(data_off..data_off + 3)?;
                data_off += 3;
                SqlValue::Int(read_signed_be(b))
            }
            4 => {
                let b = payload.get(data_off..data_off + 4)?;
                data_off += 4;
                SqlValue::Int(i32::from_be_bytes(b.try_into().ok()?) as i64)
            }
            5 => {
                let b = payload.get(data_off..data_off + 6)?;
                data_off += 6;
                SqlValue::Int(read_signed_be(b))
            }
            6 => {
                let b = payload.get(data_off..data_off + 8)?;
                data_off += 8;
                SqlValue::Int(i64::from_be_bytes(b.try_into().ok()?))
            }
            7 => {
                let b = payload.get(data_off..data_off + 8)?;
                data_off += 8;
                let bits = u64::from_be_bytes(b.try_into().ok()?);
                SqlValue::Float(f64::from_bits(bits))
            }
            8 => SqlValue::Int(0),
            9 => SqlValue::Int(1),
            10 | 11 => return None,
            s if s >= 12 && s % 2 == 0 => {
                let len = ((s - 12) / 2) as usize;
                let slice = payload.get(data_off..data_off + len)?;
                data_off += len;
                SqlValue::Blob(slice.to_vec())
            }
            s => {
                let len = ((s - 13) / 2) as usize;
                let slice = payload.get(data_off..data_off + len)?;
                data_off += len;
                SqlValue::Text(String::from_utf8_lossy(slice).into_owned())
            }
        };
        values.push(val);
    }
    Some(values)
}

fn assemble_payload<'a, F>(
    page: &[u8],
    payload_start: usize,
    payload_len: usize,
    usable_size: usize,
    get_page: &F,
) -> Option<Vec<u8>>
where
    F: Fn(u32) -> Option<&'a [u8]>,
{
    if payload_len == 0 || usable_size <= 35 {
        return None;
    }
    let max_local = usable_size - 35;
    let min_local = ((usable_size - 12) * 32 / 255).saturating_sub(23);
    let local_len = if payload_len <= max_local {
        payload_len
    } else {
        let k = min_local + ((payload_len - min_local) % (usable_size - 4));
        if k <= max_local {
            k
        } else {
            min_local
        }
    };
    let local_slice = page.get(payload_start..payload_start + local_len)?;
    if local_len == payload_len {
        return Some(local_slice.to_vec());
    }
    let ov_bytes = page.get(payload_start + local_len..payload_start + local_len + 4)?;
    let mut next_pg = u32::from_be_bytes(ov_bytes.try_into().ok()?);
    let mut out = Vec::with_capacity(payload_len);
    out.extend_from_slice(local_slice);
    let mut hops = 0usize;
    let max_hops = payload_len / usable_size.saturating_sub(4).max(1) + 4;
    while out.len() < payload_len && next_pg > 0 && hops < max_hops {
        hops += 1;
        let ov_page = get_page(next_pg)?;
        if ov_page.len() < 4 {
            break;
        }
        next_pg = u32::from_be_bytes(ov_page[0..4].try_into().ok()?);
        let chunk_max = ov_page.len().min(usable_size).saturating_sub(4);
        let take = chunk_max.min(payload_len - out.len());
        out.extend_from_slice(&ov_page[4..4 + take]);
    }
    (out.len() == payload_len).then_some(out)
}

fn page_hdr_offset(pgno: u32) -> usize {
    if pgno == 1 {
        100
    } else {
        0
    }
}

fn decode_leaf_cells<'a, F>(
    pgno: u32,
    page: &[u8],
    usable_size: usize,
    get_page: &F,
) -> Vec<(i64, Vec<SqlValue>)>
where
    F: Fn(u32) -> Option<&'a [u8]>,
{
    let hdr = page_hdr_offset(pgno);
    if page.len() < hdr + 8 || page[hdr] != 0x0d {
        return Vec::new();
    }
    let num_cells = u16::from_be_bytes([page[hdr + 3], page[hdr + 4]]) as usize;
    let ptr_start = hdr + 8;
    let mut out = Vec::with_capacity(num_cells);
    for i in 0..num_cells {
        let ptr_off = ptr_start + i * 2;
        if ptr_off + 2 > page.len() {
            break;
        }
        let cell_off = u16::from_be_bytes([page[ptr_off], page[ptr_off + 1]]) as usize;
        if cell_off >= page.len() {
            continue;
        }
        let Some((payload_len_i64, p1)) = read_varint(page, cell_off) else {
            continue;
        };
        let Some((rowid, p2)) = read_varint(page, p1) else {
            continue;
        };
        if payload_len_i64 <= 0 || payload_len_i64 > 16 * 1024 * 1024 {
            continue;
        }
        let Some(payload) =
            assemble_payload(page, p2, payload_len_i64 as usize, usable_size, get_page)
        else {
            continue;
        };
        if let Some(cols) = decode_record(&payload) {
            out.push((rowid, cols));
        }
    }
    out
}

/// 从 B-Tree 叶子页的空闲块（Freeblock）链表中雕刻已删记录残片。
/// SQLite 删除记录时会用 4 字节 `[next_free: u16, size: u16]` 覆写该槽位头部；
/// 这里同时尝试紧邻 4 字节之后的记录体起点的序列化列验证，避免误识别。
fn carve_leaf_freeblocks(
    pgno: u32,
    page: &[u8],
    expected_cols: usize,
) -> Vec<(i64, Vec<SqlValue>)> {
    let hdr = page_hdr_offset(pgno);
    if page.len() < hdr + 8 || page[hdr] != 0x0d || expected_cols < 2 {
        return Vec::new();
    }
    let mut fb_off = u16::from_be_bytes([page[hdr + 1], page[hdr + 2]]) as usize;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    while fb_off > 0 && fb_off + 8 <= page.len() && seen.insert(fb_off) && seen.len() < 512 {
        let next = u16::from_be_bytes([page[fb_off], page[fb_off + 1]]) as usize;
        let size = u16::from_be_bytes([page[fb_off + 2], page[fb_off + 3]]) as usize;
        let block_end = (fb_off + size).min(page.len());
        if size >= 12 {
            // SQLite 4-byte freeblock header overwrites payload_len & rowid; record header begins cleanly around +3..+6
            'probe: for delta in 3..=6usize {
                let start = fb_off + delta;
                if start >= block_end {
                    break;
                }
                let slice = &page[start..block_end];
                if let Some(cols) = decode_record(slice) {
                    if (expected_cols.saturating_sub(3)..=expected_cols + 1).contains(&cols.len()) {
                        let pk = cols.first().and_then(SqlValue::as_i64).unwrap_or(-1);
                        out.push((pk, cols));
                        break 'probe;
                    }
                }
            }
        }
        fb_off = next;
    }
    out
}

fn collect_tree_pages<'a, F>(root_pg: u32, get_page: &F, visited: &mut HashSet<u32>)
where
    F: Fn(u32) -> Option<&'a [u8]>,
{
    let mut stack = vec![root_pg];
    while let Some(pgno) = stack.pop() {
        if pgno == 0 || !visited.insert(pgno) || visited.len() > 200_000 {
            continue;
        }
        let Some(page) = get_page(pgno) else {
            continue;
        };
        let hdr = page_hdr_offset(pgno);
        if page.len() < hdr + 12 || page[hdr] != 0x05 {
            continue;
        }
        let num_cells = u16::from_be_bytes([page[hdr + 3], page[hdr + 4]]) as usize;
        let right_child = u32::from_be_bytes(page[hdr + 8..hdr + 12].try_into().unwrap_or([0; 4]));
        if right_child > 0 {
            stack.push(right_child);
        }
        let ptr_start = hdr + 12;
        for i in 0..num_cells {
            let p = ptr_start + i * 2;
            if p + 2 > page.len() {
                break;
            }
            let cell_off = u16::from_be_bytes([page[p], page[p + 1]]) as usize;
            if cell_off + 4 <= page.len() {
                let left =
                    u32::from_be_bytes(page[cell_off..cell_off + 4].try_into().unwrap_or([0; 4]));
                if left > 0 {
                    stack.push(left);
                }
            }
        }
    }
}

pub fn parse_create_table_columns(sql: &str) -> (Vec<String>, Option<usize>) {
    let Some(open) = sql.find('(') else {
        return (Vec::new(), None);
    };
    let Some(close) = sql.rfind(')') else {
        return (Vec::new(), None);
    };
    if close <= open {
        return (Vec::new(), None);
    }
    let body = &sql[open + 1..close];
    let mut defs = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_quote: Option<char> = None;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            }
        } else {
            match ch {
                '\'' | '"' | '`' => in_quote = Some(ch),
                '[' => in_quote = Some(']'),
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    defs.push(body[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start < body.len() {
        defs.push(body[start..].trim());
    }

    let mut cols = Vec::new();
    let mut pk_idx = None;
    for def in defs {
        if def.is_empty() {
            continue;
        }
        let upper = def.to_ascii_uppercase();
        if upper.starts_with("PRIMARY KEY")
            || upper.starts_with("UNIQUE")
            || upper.starts_with("CHECK")
            || upper.starts_with("FOREIGN KEY")
            || upper.starts_with("CONSTRAINT")
        {
            continue;
        }
        let name = if let Some(rest) = def.strip_prefix('"') {
            rest.split('"').next().unwrap_or("").to_owned()
        } else if let Some(rest) = def.strip_prefix('[') {
            rest.split(']').next().unwrap_or("").to_owned()
        } else if let Some(rest) = def.strip_prefix('`') {
            rest.split('`').next().unwrap_or("").to_owned()
        } else {
            def.split_whitespace().next().unwrap_or("").to_owned()
        };
        if name.is_empty() {
            continue;
        }
        let idx = cols.len();
        if pk_idx.is_none() && upper.contains("INTEGER PRIMARY KEY") {
            pk_idx = Some(idx);
        }
        cols.push(name);
    }
    (cols, pk_idx)
}

pub fn load_table_meta(
    db: &[u8],
    inspection: &WalShmInspection<'_>,
    table_name: &str,
) -> Option<TableMeta> {
    let ps = inspection.db.page_size;
    let usable = inspection.db.usable_size;
    let mut live_pages: HashMap<u32, usize> = HashMap::new();
    for (idx, fr) in inspection.frames.iter().enumerate() {
        if fr.status == WalFrameStatus::CommittedWal {
            live_pages.insert(fr.pgno, idx);
        }
    }
    let get_page = |pgno: u32| -> Option<&[u8]> {
        if let Some(&idx) = live_pages.get(&pgno) {
            return Some(inspection.frames[idx].page);
        }
        if pgno == 0 {
            return None;
        }
        let start = (pgno as usize - 1) * ps;
        db.get(start..start + ps)
    };

    let mut master_pages = HashSet::new();
    collect_tree_pages(1, &get_page, &mut master_pages);
    for pgno in master_pages {
        let Some(page) = get_page(pgno) else {
            continue;
        };
        for (_rowid, cols) in decode_leaf_cells(pgno, page, usable, &get_page) {
            let kind = cols.first().and_then(SqlValue::as_str).unwrap_or("");
            let name = cols.get(1).and_then(SqlValue::as_str).unwrap_or("");
            let root = cols.get(3).and_then(SqlValue::as_i64).unwrap_or(0);
            let sql = cols.get(4).and_then(SqlValue::as_str).unwrap_or("");
            if kind.eq_ignore_ascii_case("table")
                && name.eq_ignore_ascii_case(table_name)
                && root > 0
            {
                let (columns, integer_pk_col) = parse_create_table_columns(sql);
                return Some(TableMeta {
                    root_page: root as u32,
                    columns,
                    integer_pk_col,
                });
            }
        }
    }
    None
}

fn map_row(rowid: i64, raw_cols: &[SqlValue], meta: &TableMeta) -> HashMap<String, SqlValue> {
    let mut map = HashMap::with_capacity(meta.columns.len() + 1);
    for (idx, col_name) in meta.columns.iter().enumerate() {
        let val = raw_cols.get(idx).cloned().unwrap_or(SqlValue::Null);
        let final_val =
            if Some(idx) == meta.integer_pk_col && matches!(val, SqlValue::Null) && rowid > 0 {
                SqlValue::Int(rowid)
            } else {
                val
            };
        map.insert(col_name.clone(), final_val);
    }
    map
}

/// 扫描一张表在基础库、已提交 WAL 历史、未提交尾部帧、旧 Salt 残留帧及叶子页空闲块里的全量历史行版本。
pub fn scan_table_history(
    db: &[u8],
    inspection: &WalShmInspection<'_>,
    meta: &TableMeta,
) -> Vec<HistoricalRow> {
    let ps = inspection.db.page_size;
    let usable = inspection.db.usable_size;

    let base_get = |pgno: u32| -> Option<&[u8]> {
        if pgno == 0 {
            return None;
        }
        let s = (pgno as usize - 1) * ps;
        db.get(s..s + ps)
    };

    let mut known_pages: HashSet<u32> = HashSet::new();
    collect_tree_pages(meta.root_page, &base_get, &mut known_pages);

    let mut out = Vec::new();
    let expected_cols = meta.columns.len();

    // 1. Scan Base DB pages & freeblocks
    let mut sorted_base: Vec<u32> = known_pages.iter().copied().collect();
    sorted_base.sort_unstable();
    for pgno in sorted_base {
        let Some(page) = base_get(pgno) else {
            continue;
        };
        for (rowid, cols) in decode_leaf_cells(pgno, page, usable, &base_get) {
            out.push(HistoricalRow {
                rowid,
                frame_no: 0,
                commit_no: 0,
                pgno,
                status: WalFrameStatus::BaseDb,
                is_freeblock: false,
                cols: map_row(rowid, &cols, meta),
            });
        }
        for (pk, cols) in carve_leaf_freeblocks(pgno, page, expected_cols) {
            out.push(HistoricalRow {
                rowid: pk,
                frame_no: 0,
                commit_no: 0,
                pgno,
                status: WalFrameStatus::BtreeFreeblock,
                is_freeblock: true,
                cols: map_row(pk, &cols, meta),
            });
        }
    }

    // 2. Group committed frames by commit_no and walk uncommitted/prior-salt frames sequentially.
    let mut live_pages: HashMap<u32, usize> = HashMap::new();
    let mut commits: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    let mut loose_indices: Vec<usize> = Vec::new();

    for (idx, fr) in inspection.frames.iter().enumerate() {
        match fr.status {
            WalFrameStatus::CommittedWal => {
                commits.entry(fr.commit_no).or_default().push(idx);
            }
            WalFrameStatus::UncommittedWal | WalFrameStatus::PriorSaltResidual => {
                loose_indices.push(idx);
            }
            WalFrameStatus::BaseDb | WalFrameStatus::BtreeFreeblock => {}
        }
    }

    for (cno, frame_indices) in commits {
        for &idx in &frame_indices {
            live_pages.insert(inspection.frames[idx].pgno, idx);
        }
        let get_page = |pgno: u32| -> Option<&[u8]> {
            if let Some(&idx) = live_pages.get(&pgno) {
                return Some(inspection.frames[idx].page);
            }
            base_get(pgno)
        };

        let touches_table = frame_indices.iter().any(|&idx| {
            let fr = &inspection.frames[idx];
            fr.pgno == meta.root_page || known_pages.contains(&fr.pgno)
        });
        if touches_table {
            collect_tree_pages(meta.root_page, &get_page, &mut known_pages);
        }

        for &idx in &frame_indices {
            let fr = &inspection.frames[idx];
            if !known_pages.contains(&fr.pgno) {
                continue;
            }
            let hdr = page_hdr_offset(fr.pgno);
            if fr.page.len() <= hdr {
                continue;
            }
            if fr.page[hdr] == 0x0d {
                for (rowid, cols) in decode_leaf_cells(fr.pgno, fr.page, usable, &get_page) {
                    out.push(HistoricalRow {
                        rowid,
                        frame_no: fr.frame_no,
                        commit_no: cno,
                        pgno: fr.pgno,
                        status: fr.status,
                        is_freeblock: false,
                        cols: map_row(rowid, &cols, meta),
                    });
                }
                for (pk, cols) in carve_leaf_freeblocks(fr.pgno, fr.page, expected_cols) {
                    out.push(HistoricalRow {
                        rowid: pk,
                        frame_no: fr.frame_no,
                        commit_no: cno,
                        pgno: fr.pgno,
                        status: WalFrameStatus::BtreeFreeblock,
                        is_freeblock: true,
                        cols: map_row(pk, &cols, meta),
                    });
                }
            }
        }
    }

    // 3. Scan uncommitted trailing & prior-salt residual frames using live_pages overlay + per-frame override
    let mut temp_overlay = live_pages;
    for idx in loose_indices {
        let fr = &inspection.frames[idx];
        temp_overlay.insert(fr.pgno, idx);
        let get_page = |pgno: u32| -> Option<&[u8]> {
            if let Some(&f_idx) = temp_overlay.get(&pgno) {
                return Some(inspection.frames[f_idx].page);
            }
            base_get(pgno)
        };
        if fr.pgno == meta.root_page || known_pages.contains(&fr.pgno) {
            collect_tree_pages(fr.pgno, &get_page, &mut known_pages);
        }
        if !known_pages.contains(&fr.pgno) {
            continue;
        }
        let hdr = page_hdr_offset(fr.pgno);
        if fr.page.len() > hdr && fr.page[hdr] == 0x0d {
            for (rowid, cols) in decode_leaf_cells(fr.pgno, fr.page, usable, &get_page) {
                out.push(HistoricalRow {
                    rowid,
                    frame_no: fr.frame_no,
                    commit_no: fr.commit_no,
                    pgno: fr.pgno,
                    status: fr.status,
                    is_freeblock: false,
                    cols: map_row(rowid, &cols, meta),
                });
            }
            for (pk, cols) in carve_leaf_freeblocks(fr.pgno, fr.page, expected_cols) {
                out.push(HistoricalRow {
                    rowid: pk,
                    frame_no: fr.frame_no,
                    commit_no: fr.commit_no,
                    pgno: fr.pgno,
                    status: WalFrameStatus::BtreeFreeblock,
                    is_freeblock: true,
                    cols: map_row(pk, &cols, meta),
                });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table_columns_with_integer_pk_and_nested_parens() {
        let sql = "CREATE TABLE ZASSET ( Z_PK INTEGER PRIMARY KEY, ZDURATION DECIMAL(10, 2), ZFILENAME VARCHAR, \"Z_OPT\" INTEGER )";
        let (cols, pk) = parse_create_table_columns(sql);
        assert_eq!(cols, vec!["Z_PK", "ZDURATION", "ZFILENAME", "Z_OPT"]);
        assert_eq!(pk, Some(0));
    }
}
