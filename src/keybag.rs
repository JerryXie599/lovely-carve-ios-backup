//! iOS 备份密码验证、数字口令爆破与文件级解密（标准 iOS 备份加密方案）。
//!
//! BackupKeyBag（TLV）含保护类密钥：`WRAP & 0x2`（或 `KTYP=0`）表示口令包裹（AES-KW）。
//! 派生方式：
//! - iOS 10.2+ 双重保护（`DPSL` + `DPIC`）：`PBKDF2-HMAC-SHA256(password, DPSL, DPIC, 32)` →
//!   再作为输入做 `PBKDF2-HMAC-SHA1(step1, SALT, ITER, 32)`；
//! - iOS 10.x 及更早（仅 `SALT` + `ITER`）：`PBKDF2-HMAC-SHA1(password, SALT, ITER, 32)`。
//!
//! 派生密钥用于 RFC 3394 AES-256 Key Unwrap 解开类密钥 `WPKY`，再用类密钥解开 `Files.file`
//! 中的单文件密钥（`EncryptionKey`），最后以全零 IV 做 AES-256-CBC 解密。

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Block;
use sha2::Digest;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

const MAX_PBKDF2_ROUNDS: u32 = 20_000_000;
const WRAP_PASSCODE: u32 = 0x2;

#[derive(Debug, Clone)]
pub struct ClassEntry {
    pub class_id: u32,
    pub wrap: u32,
    pub ktyp: u32,
    pub wpky: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct KeyBag {
    pub salt: Option<Vec<u8>>,
    pub iter: Option<u32>,
    pub dpsl: Option<Vec<u8>>,
    pub dpic: Option<u32>,
    pub classes: Vec<ClassEntry>,
    pub wrapped: Vec<(u32, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct UnlockOutcome {
    pub ok: bool,
    pub class_keys: Vec<(u32, [u8; 32])>,
    pub method: &'static str,
}

#[derive(Debug, Clone)]
pub struct CrackOutcome {
    pub password: Option<String>,
    pub class_keys: Vec<(u32, [u8; 32])>,
    pub tested_count: usize,
    pub elapsed_ms: u64,
    pub method: &'static str,
    pub cancelled: bool,
}

#[derive(Default)]
struct PartialClass {
    class_id: Option<u32>,
    wrap: Option<u32>,
    ktyp: Option<u32>,
    wpky: Option<Vec<u8>>,
}

impl PartialClass {
    fn is_empty(&self) -> bool {
        self.class_id.is_none() && self.wrap.is_none() && self.ktyp.is_none() && self.wpky.is_none()
    }

    fn into_entry(self) -> Option<ClassEntry> {
        Some(ClassEntry {
            class_id: self.class_id?,
            wrap: self.wrap.unwrap_or(0),
            ktyp: self.ktyp.unwrap_or(0),
            wpky: self.wpky.unwrap_or_default(),
        })
    }
}

/// 解析 BackupKeyBag TLV，准确区分头部 UUID/WRAP 与后续各保护类块。
pub fn parse(blob: &[u8]) -> Option<KeyBag> {
    if blob.len() < 8 || &blob[..4] != b"VERS" {
        return None;
    }
    let mut kb = KeyBag {
        salt: None,
        iter: None,
        dpsl: None,
        dpic: None,
        classes: Vec::new(),
        wrapped: Vec::new(),
    };
    let mut off = 0usize;
    let mut saw_bag_uuid = false;
    let mut current: Option<PartialClass> = None;

    while off + 8 <= blob.len() {
        let tag = &blob[off..off + 4];
        let len = u32::from_be_bytes(blob[off + 4..off + 8].try_into().ok()?) as usize;
        let end = off.checked_add(8)?.checked_add(len)?;
        let val = blob.get(off + 8..end)?;
        off = end;

        match tag {
            b"SALT" => kb.salt = Some(val.to_vec()),
            b"ITER" => kb.iter = be32(val),
            b"DPSL" => kb.dpsl = Some(val.to_vec()),
            b"DPIC" => kb.dpic = be32(val),
            b"UUID" => {
                if !saw_bag_uuid {
                    saw_bag_uuid = true;
                } else {
                    if let Some(prev) = current.take().and_then(PartialClass::into_entry) {
                        kb.classes.push(prev);
                    }
                    current = Some(PartialClass::default());
                }
            }
            b"CLAS" => {
                if current
                    .as_ref()
                    .is_some_and(|c| c.class_id.is_some() && !c.is_empty())
                {
                    if let Some(prev) = current.take().and_then(PartialClass::into_entry) {
                        kb.classes.push(prev);
                    }
                    current = Some(PartialClass::default());
                }
                let cls = current.get_or_insert_with(PartialClass::default);
                cls.class_id = be32(val);
            }
            b"WRAP" => {
                if let Some(cls) = current.as_mut() {
                    cls.wrap = be32(val);
                }
            }
            b"KTYP" => {
                let cls = current.get_or_insert_with(PartialClass::default);
                cls.ktyp = be32(val);
            }
            b"WPKY" => {
                let cls = current.get_or_insert_with(PartialClass::default);
                cls.wpky = Some(val.to_vec());
            }
            _ => {}
        }
    }
    if let Some(last) = current.take().and_then(PartialClass::into_entry) {
        kb.classes.push(last);
    }

    for c in &kb.classes {
        let is_passcode = (c.wrap & WRAP_PASSCODE != 0) || (c.wrap == 0 && c.ktyp == 0);
        if is_passcode && !c.wpky.is_empty() {
            kb.wrapped.push((c.class_id, c.wpky.clone()));
        }
    }
    (!kb.wrapped.is_empty()).then_some(kb)
}

fn be32(v: &[u8]) -> Option<u32> {
    v.get(0..4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_be_bytes)
}

/// RFC 3394 AES-256/192/128 Key Unwrap。
pub fn aes_kw_unwrap_for_test(kek: &[u8], wit: &[u8]) -> Option<Vec<u8>> {
    if wit.len() < 24
        || !wit.len().is_multiple_of(8)
        || !(kek.len() == 16 || kek.len() == 24 || kek.len() == 32)
    {
        return None;
    }
    let n = wit.len() / 8 - 1;
    let mut a = [0u8; 8];
    a.copy_from_slice(&wit[..8]);
    let mut r: Vec<[u8; 8]> = (0..n)
        .map(|i| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&wit[8 * (i + 1)..8 * (i + 2)]);
            b
        })
        .collect();

    macro_rules! unwrap_with {
        ($cipher:ty) => {{
            let cipher = <$cipher>::new_from_slice(kek).ok()?;
            for j in (0..6u64).rev() {
                for i in (1..=n as u64).rev() {
                    let t = n as u64 * j + i;
                    let tb = t.to_be_bytes();
                    for k in 0..8 {
                        a[k] ^= tb[k];
                    }
                    let mut block = Block::from([0u8; 16]);
                    block[..8].copy_from_slice(&a);
                    block[8..].copy_from_slice(&r[(i - 1) as usize]);
                    cipher.decrypt_block(&mut block);
                    a.copy_from_slice(&block[..8]);
                    r[(i - 1) as usize].copy_from_slice(&block[8..]);
                }
            }
        }};
    }

    match kek.len() {
        16 => unwrap_with!(aes::Aes128),
        24 => unwrap_with!(aes::Aes192),
        32 => unwrap_with!(aes::Aes256),
        _ => return None,
    }

    (a == [0xA6; 8]).then(|| r.concat())
}

fn hmac_block<const BLOCK: usize, const HASH_LEN: usize>(
    hasher: fn(&[u8]) -> [u8; HASH_LEN],
    key: &[u8],
    data: &[u8],
) -> [u8; HASH_LEN] {
    let mut k_pad = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = hasher(key);
        k_pad[..HASH_LEN].copy_from_slice(&hashed);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(BLOCK + data.len());
    for b in k_pad {
        inner.push(b ^ 0x36);
    }
    inner.extend_from_slice(data);
    let inner_hash = hasher(&inner);

    let mut outer = Vec::with_capacity(BLOCK + HASH_LEN);
    for b in k_pad {
        outer.push(b ^ 0x5c);
    }
    outer.extend_from_slice(&inner_hash);
    hasher(&outer)
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h = sha1::Sha1::new();
    sha1::Digest::update(&mut h, data);
    let out = sha1::Digest::finalize(h);
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&out);
    arr
}

fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut h = sha2::Sha256::new();
    Digest::update(&mut h, data);
    let out = Digest::finalize(h);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn pbkdf2_generic<const HASH_LEN: usize>(
    hasher: fn(&[u8]) -> [u8; HASH_LEN],
    password: &[u8],
    salt: &[u8],
    rounds: u32,
    dk_len: usize,
) -> Vec<u8> {
    if rounds == 0 || dk_len == 0 {
        return Vec::new();
    }
    let blocks = dk_len.div_ceil(HASH_LEN);
    let mut out = Vec::with_capacity(blocks * HASH_LEN);

    let mut k_pad = [0u8; 64];
    if password.len() > 64 {
        let hashed = hasher(password);
        k_pad[..HASH_LEN].copy_from_slice(&hashed);
    } else {
        k_pad[..password.len()].copy_from_slice(password);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k_pad[i];
        opad[i] ^= k_pad[i];
    }

    for block_idx in 1..=blocks as u32 {
        let mut init_msg = Vec::with_capacity(salt.len() + 4);
        init_msg.extend_from_slice(salt);
        init_msg.extend_from_slice(&block_idx.to_be_bytes());
        let mut u = hmac_block::<64, HASH_LEN>(hasher, password, &init_msg);
        let mut t = u;
        for _ in 1..rounds {
            let mut inner = Vec::with_capacity(64 + HASH_LEN);
            inner.extend_from_slice(&ipad);
            inner.extend_from_slice(&u);
            let ih = hasher(&inner);

            let mut outer = Vec::with_capacity(64 + HASH_LEN);
            outer.extend_from_slice(&opad);
            outer.extend_from_slice(&ih);
            u = hasher(&outer);

            for k in 0..HASH_LEN {
                t[k] ^= u[k];
            }
        }
        out.extend_from_slice(&t);
    }
    out.truncate(dk_len);
    out
}

pub fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], rounds: u32, dk_len: usize) -> Vec<u8> {
    pbkdf2_generic::<20>(sha1_digest, password, salt, rounds, dk_len)
}

pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], rounds: u32, dk_len: usize) -> Vec<u8> {
    pbkdf2_generic::<32>(sha256_digest, password, salt, rounds, dk_len)
}

/// 根据 Apple iOS 备份规范派生 32 字节 Backup Key。
pub fn derive_backup_key(password: &[u8], kb: &KeyBag) -> Option<([u8; 32], &'static str)> {
    let salt = kb.salt.as_deref()?;
    let iter = kb.iter?;
    if iter == 0 || iter > MAX_PBKDF2_ROUNDS {
        return None;
    }

    let (dk, method) = match (kb.dpsl.as_deref(), kb.dpic) {
        (Some(dpsl), Some(dpic)) => {
            if dpic == 0 || dpic > MAX_PBKDF2_ROUNDS {
                return None;
            }
            let first = pbkdf2_hmac_sha256(password, dpsl, dpic, 32);
            let final_key = pbkdf2_hmac_sha1(&first, salt, iter, 32);
            (final_key, "PBKDF2-HMAC-SHA256 + SHA1 (iOS 10.2+ 双重保护)")
        }
        _ => (
            pbkdf2_hmac_sha1(password, salt, iter, 32),
            "PBKDF2-HMAC-SHA1 (ITER+SALT)",
        ),
    };

    let arr: [u8; 32] = dk.try_into().ok()?;
    Some((arr, method))
}

/// 尝试用给定口令解开 KeyBag 中的所有保护类密钥。
pub fn unlock_with_password(kb: &KeyBag, password: &str) -> UnlockOutcome {
    let Some((kek, method)) = derive_backup_key(password.as_bytes(), kb) else {
        return UnlockOutcome {
            ok: false,
            class_keys: Vec::new(),
            method: "",
        };
    };

    let mut class_keys = Vec::new();
    for (cls, wrapped) in &kb.wrapped {
        if let Some(unwrapped) = aes_kw_unwrap_for_test(&kek, wrapped) {
            if let Ok(arr) = <[u8; 32]>::try_from(unwrapped.as_slice()) {
                class_keys.push((*cls, arr));
            }
        }
    }

    let ok = !class_keys.is_empty();
    UnlockOutcome {
        ok,
        class_keys,
        method: if ok { method } else { "" },
    }
}

/// 验证口令并返回 `(是否正确, 解开类密钥数, 派生方式)`。
pub fn verify(kb: &KeyBag, password: &str) -> (bool, usize, &'static str) {
    let outcome = unlock_with_password(kb, password);
    (outcome.ok, outcome.class_keys.len(), outcome.method)
}

/// 用类密钥解开文件加密密钥（支持 `[u32_le class][40B wrapped]` 与裸 `40B wrapped` 两种格式）。
pub fn unwrap_file_key(
    class_keys: &[(u32, [u8; 32])],
    protection_class: u32,
    encryption_key: &[u8],
) -> Option<[u8; 32]> {
    let wrapped = if encryption_key.len() == 44 {
        &encryption_key[4..]
    } else if encryption_key.len() == 40 {
        encryption_key
    } else {
        return None;
    };

    let class_key = class_keys
        .iter()
        .find(|(cls, _)| *cls == protection_class)
        .map(|(_, k)| k)?;

    let unwrapped = aes_kw_unwrap_for_test(class_key, wrapped)?;
    unwrapped.try_into().ok()
}

/// 全零 IV 的 AES-256-CBC 解密。若给出 `expected_size` 则严格截断至真实长度；
/// 否则移除合法 PKCS#7 填充（例如解密 `Manifest.db` 自身）。
pub fn decrypt_aes256_cbc_zero_iv(
    key: &[u8; 32],
    ciphertext: &[u8],
    expected_size: Option<usize>,
) -> Option<Vec<u8>> {
    if ciphertext.is_empty() {
        return (expected_size == Some(0)).then(Vec::new);
    }
    if !ciphertext.len().is_multiple_of(16) {
        return None;
    }

    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    let mut out = Vec::with_capacity(ciphertext.len());
    let mut prev = [0u8; 16];

    for chunk in ciphertext.as_chunks::<16>().0 {
        let mut block = Block::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }

    if let Some(sz) = expected_size {
        out.truncate(sz.min(out.len()));
        return Some(out);
    }

    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || out.len() < pad {
        return None;
    }
    if out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

fn check_single_candidate(kb: &KeyBag, first_wrapped: &(u32, Vec<u8>), pwd: &str) -> bool {
    let Some((kek, _)) = derive_backup_key(pwd.as_bytes(), kb) else {
        return false;
    };
    aes_kw_unwrap_for_test(&kek, &first_wrapped.1).is_some()
}

fn parallel_search_strings(
    kb: &KeyBag,
    first_wrapped: &(u32, Vec<u8>),
    candidates: &[String],
    is_cancelled: &(dyn Fn() -> bool + Sync),
    tested: &AtomicUsize,
) -> (Option<String>, bool) {
    if candidates.is_empty() {
        return (None, false);
    }
    let found = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let cursor = AtomicUsize::new(0);
    let winner = std::sync::Mutex::new(None::<String>);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);
    let high_cost = kb.dpic.unwrap_or(0) >= 100_000 || kb.iter.unwrap_or(0) >= 100_000;
    let batch = if high_cost { 2 } else { 128 };

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if found.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
                    break;
                }
                if is_cancelled() {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                let start = cursor.fetch_add(batch, Ordering::Relaxed);
                if start >= candidates.len() {
                    break;
                }
                let end = (start + batch).min(candidates.len());
                for pwd in &candidates[start..end] {
                    if found.load(Ordering::Relaxed) {
                        break;
                    }
                    if is_cancelled() {
                        cancelled.store(true, Ordering::Relaxed);
                        break;
                    }
                    tested.fetch_add(1, Ordering::Relaxed);
                    if check_single_candidate(kb, first_wrapped, pwd) {
                        if let Ok(mut guard) = winner.lock() {
                            *guard = Some(pwd.clone());
                        }
                        found.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }
    });

    let matched = winner.into_inner().ok().flatten();
    (matched, cancelled.load(Ordering::Relaxed))
}

fn parallel_search_numeric_range(
    kb: &KeyBag,
    first_wrapped: &(u32, Vec<u8>),
    range: (u32, u32, usize),
    skip_set: &HashSet<String>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    tested: &AtomicUsize,
) -> (Option<String>, bool) {
    let (start_val, end_excl, width) = range;
    let found = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let cursor = AtomicU32::new(start_val);
    let winner = std::sync::Mutex::new(None::<String>);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);
    let high_cost = kb.dpic.unwrap_or(0) >= 100_000 || kb.iter.unwrap_or(0) >= 100_000;
    let batch = if high_cost { 2 } else { 256 };

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if found.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
                    break;
                }
                if is_cancelled() {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                let s = cursor.fetch_add(batch, Ordering::Relaxed);
                if s >= end_excl {
                    break;
                }
                let e = s.saturating_add(batch).min(end_excl);
                for n in s..e {
                    if found.load(Ordering::Relaxed) {
                        break;
                    }
                    if is_cancelled() {
                        cancelled.store(true, Ordering::Relaxed);
                        break;
                    }
                    let pin = if width == 4 {
                        format!("{n:04}")
                    } else {
                        format!("{n:06}")
                    };
                    if skip_set.contains(&pin) {
                        continue;
                    }
                    tested.fetch_add(1, Ordering::Relaxed);
                    if check_single_candidate(kb, first_wrapped, &pin) {
                        if let Ok(mut guard) = winner.lock() {
                            *guard = Some(pin);
                        }
                        found.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }
    });

    let matched = winner.into_inner().ok().flatten();
    (matched, cancelled.load(Ordering::Relaxed))
}

/// 自动爆破 Manifest.plist BackupKeyBag 备份密码（按高频弱口令 -> 4位全量 -> 6位日期 -> 6位全量纯数字顺序并行探测）。
pub fn crack_numeric_pin(
    kb: &KeyBag,
    serial: Option<&str>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> CrackOutcome {
    let start = Instant::now();
    let Some(first_wrapped) = kb.wrapped.first() else {
        return CrackOutcome {
            password: None,
            class_keys: Vec::new(),
            tested_count: 0,
            elapsed_ms: 0,
            method: "",
            cancelled: false,
        };
    };

    let tested = AtomicUsize::new(0);

    // 阶段 0：空密码
    tested.fetch_add(1, Ordering::Relaxed);
    if check_single_candidate(kb, first_wrapped, "") {
        let unlocked = unlock_with_password(kb, "");
        return CrackOutcome {
            password: Some(String::new()),
            class_keys: unlocked.class_keys,
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: unlocked.method,
            cancelled: false,
        };
    }

    // 阶段 1：高频 4/6 位纯数字弱口令 + 序列号尾号（若全数字）
    let mut seen = HashSet::new();
    let mut priority = Vec::new();
    let mut push_pri = |s: &str| {
        if !s.is_empty() && seen.insert(s.to_string()) {
            priority.push(s.to_string());
        }
    };

    if let Some(sn) = serial.map(str::trim).filter(|s| !s.is_empty()) {
        if sn.len() >= 4 {
            let tail4 = &sn[sn.len() - 4..];
            if tail4.bytes().all(|b| b.is_ascii_digit()) {
                push_pri(tail4);
            }
        }
        if sn.len() >= 6 {
            let tail6 = &sn[sn.len() - 6..];
            if tail6.bytes().all(|b| b.is_ascii_digit()) {
                push_pri(tail6);
            }
        }
    }

    for weak in [
        "1234", "0000", "1111", "8888", "6666", "2580", "0852", "1212", "1122", "9999", "7777",
        "5555", "4321", "123456", "000000", "111111", "888888", "666666", "123123", "654321",
        "112233", "999999", "520131",
    ] {
        push_pri(weak);
    }

    let (hit, was_cancelled) =
        parallel_search_strings(kb, first_wrapped, &priority, is_cancelled, &tested);
    if let Some(pw) = hit {
        let unlocked = unlock_with_password(kb, &pw);
        return CrackOutcome {
            password: Some(pw),
            class_keys: unlocked.class_keys,
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: unlocked.method,
            cancelled: false,
        };
    }
    if was_cancelled {
        return CrackOutcome {
            password: None,
            class_keys: Vec::new(),
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: "",
            cancelled: true,
        };
    }

    // 阶段 2：4 位全量纯数字（0000..=9999）
    let (hit4, cancel4) = parallel_search_numeric_range(
        kb,
        first_wrapped,
        (0, 10_000, 4),
        &seen,
        is_cancelled,
        &tested,
    );
    if let Some(pw) = hit4 {
        let unlocked = unlock_with_password(kb, &pw);
        return CrackOutcome {
            password: Some(pw),
            class_keys: unlocked.class_keys,
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: unlocked.method,
            cancelled: false,
        };
    }
    if cancel4 {
        return CrackOutcome {
            password: None,
            class_keys: Vec::new(),
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: "",
            cancelled: true,
        };
    }

    // 阶段 3：常见 6 位出生年月/连续日期组合（1970..=2026）
    let mut dates = Vec::new();
    for y in 1970..=2026u32 {
        for m in 1..=12u32 {
            let d = format!("{y:04}{m:02}");
            if seen.insert(d.clone()) {
                dates.push(d);
            }
        }
        let d00 = format!("{y:04}00");
        if seen.insert(d00.clone()) {
            dates.push(d00);
        }
    }
    let (hit_d, cancel_d) =
        parallel_search_strings(kb, first_wrapped, &dates, is_cancelled, &tested);
    if let Some(pw) = hit_d {
        let unlocked = unlock_with_password(kb, &pw);
        return CrackOutcome {
            password: Some(pw),
            class_keys: unlocked.class_keys,
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: unlocked.method,
            cancelled: false,
        };
    }
    if cancel_d {
        return CrackOutcome {
            password: None,
            class_keys: Vec::new(),
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: "",
            cancelled: true,
        };
    }

    // 阶段 4：6 位全量纯数字（000000..=999999）
    let (hit6, cancel6) = parallel_search_numeric_range(
        kb,
        first_wrapped,
        (0, 1_000_000, 6),
        &seen,
        is_cancelled,
        &tested,
    );
    if let Some(pw) = hit6 {
        let unlocked = unlock_with_password(kb, &pw);
        return CrackOutcome {
            password: Some(pw),
            class_keys: unlocked.class_keys,
            tested_count: tested.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis() as u64,
            method: unlocked.method,
            cancelled: false,
        };
    }

    CrackOutcome {
        password: None,
        class_keys: Vec::new(),
        tested_count: tested.load(Ordering::Relaxed),
        elapsed_ms: start.elapsed().as_millis() as u64,
        method: "",
        cancelled: cancel6,
    }
}
