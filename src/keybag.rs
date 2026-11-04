//! 备份密码验证与类密钥解密（标准 iOS 备份加密方案）。
//!
//! BackupKeyBag（TLV）含保护类密钥：KTYP=0 表示口令包裹（AES-KW）。
//! 派生方式两代并存：
//! - 旧式（iOS 10.x 前备份）：ITER+SALT，PBKDF2-HMAC-SHA1(1000+轮)，派生 32 字节
//! - 现代式：DPSL+DPIC，PBKDF2-HMAC-SHA256(DPIC 轮)，派生 32 字节
//! 派生密钥按 UTF-8 密码直接用于 AES-256 Key Wrap 解开 WPKY；
//! 任一类密钥解开成功即认为密码正确。

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Block;
use sha2::{Digest, Sha256};

pub struct KeyBag {
    pub salt: Option<Vec<u8>>,
    pub iter: Option<u32>,
    pub dpsl: Option<Vec<u8>>,
    pub dpic: Option<u32>,
    pub wrapped: Vec<(u32, Vec<u8>)>, // (CLAS, WPKY)
}

/// 解析 BackupKeyBag TLV。
pub fn parse(blob: &[u8]) -> Option<KeyBag> {
    if blob.len() < 8 || &blob[..4] != b"VERS" {
        return None;
    }
    let mut kb = KeyBag {
        salt: None,
        iter: None,
        dpsl: None,
        dpic: None,
        wrapped: Vec::new(),
    };
    let mut off = 0usize;
    let mut ktyp: Option<u32> = None;
    let mut clas: Option<u32> = None;
    while off + 8 <= blob.len() {
        let tag = &blob[off..off + 4];
        let len = u32::from_be_bytes(blob[off + 4..off + 8].try_into().ok()?) as usize;
        let val = blob.get(off + 8..off + 8 + len)?;
        off += 8 + len;
        match tag {
            b"SALT" => kb.salt = Some(val.to_vec()),
            b"ITER" => kb.iter = be32(val),
            b"DPSL" => kb.dpsl = Some(val.to_vec()),
            b"DPIC" => kb.dpic = be32(val),
            b"CLAS" => clas = be32(val),
            b"KTYP" => ktyp = be32(val),
            b"WPKY" => {
                if ktyp == Some(0) {
                    if let Some(c) = clas {
                        kb.wrapped.push((c, val.to_vec()));
                    }
                }
            }
            _ => {}
        }
    }
    (!kb.wrapped.is_empty()).then_some(kb)
}

fn be32(v: &[u8]) -> Option<u32> {
    v.get(0..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes)
}

/// RFC 3394 AES Key Unwrap。
pub fn aes_kw_unwrap_for_test(kek: &[u8], wit: &[u8]) -> Option<Vec<u8>> {
    if wit.len() < 24 || wit.len() % 8 != 0 || !(kek.len() == 16 || kek.len() == 24 || kek.len() == 32) {
        return None;
    }
    let cipher = aes::Aes256::new_from_slice(kek).ok()?;
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
    for j in (0..6u64).rev() {
        for i in (1..=n as u64).rev() {
            let t = n as u64 * j + i;
            let mut block = Block::from([0u8; 16]);
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(&r[(i - 1) as usize]);
            cipher.decrypt_block(&mut block);
            a.copy_from_slice(&block[..8]);
            let tb = t.to_be_bytes();
            for k in 0..8 {
                a[k] ^= tb[k];
            }
            r[(i - 1) as usize].copy_from_slice(&block[8..]);
        }
    }
    (a == [0xA6; 8]).then(|| r.concat())
}

fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32, hash: u32) -> Vec<u8> {
    // 手写 PBKDF2：hash 0=SHA1(20字节块), 1=SHA256(32字节块)
    match hash {
        0 => pbkdf2_sha1(password, salt, rounds),
        _ => pbkdf2_sha256(password, salt, rounds),
    }
}

fn pbkdf2_sha1(password: &[u8], salt: &[u8], rounds: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let block_index = 1u32;
    let mut u = {
        let mut ip = password.to_vec();
        ip.extend_from_slice(salt);
        ip.extend_from_slice(&block_index.to_be_bytes());
        let mut h = sha1::Sha1::new();
        sha1::Digest::update(&mut h, &ip);
        sha1::Digest::finalize(h).to_vec()
    };
    let mut t = u.clone();
    for _ in 1..rounds {
        let mut h = sha1::Sha1::new();
        sha1::Digest::update(&mut h, &u);
        u = sha1::Digest::finalize(h).to_vec();
        for k in 0..t.len() {
            t[k] ^= u[k];
        }
    }
    out.extend(t);
    out
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32) -> Vec<u8> {
    let block_index = 1u32;
    let mut u = {
        let mut ip = password.to_vec();
        ip.extend_from_slice(salt);
        ip.extend_from_slice(&block_index.to_be_bytes());
        let mut h = Sha256::new();
        Digest::update(&mut h, &ip);
        Digest::finalize(h).to_vec()
    };
    let mut t = u.clone();
    for _ in 1..rounds {
        let mut h = Sha256::new();
        Digest::update(&mut h, &u);
        u = Digest::finalize(h).to_vec();
        for k in 0..t.len() {
            t[k] ^= u[k];
        }
    }
    t
}

/// 验证密码：对每种类密钥尝试两代 KDF；解开任一即成功。
/// 返回 (是否正确, 解开的类密钥数, 使用的派生方式)。
pub fn verify(kb: &KeyBag, password: &str) -> (bool, usize, &'static str) {
    // 方式一：旧式 ITER+SALT SHA1
    let mut unlocked = 0usize;
    let mut method = "";
    if let (Some(salt), Some(iters)) = (&kb.salt, kb.iter) {
        let dk = pbkdf2_sha1(password.as_bytes(), salt, iters);
        if dk.len() >= 32 {
            let kek = &dk[..32];
            let ok = kb
                .wrapped
                .iter()
                .filter(|(_, w)| aes_kw_unwrap_for_test(kek, w).is_some())
                .count();
            if ok > 0 {
                unlocked = ok;
                method = "PBKDF2-HMAC-SHA1（ITER+SALT，旧式）";
            }
        }
    }
    // 方式二：现代式 DPSL+DPIC SHA256
    if unlocked == 0 {
        if let (Some(salt), Some(iters)) = (&kb.dpsl, kb.dpic) {
            if iters > 0 && iters <= 20_000_000 {
                let dk = pbkdf2_sha256(password.as_bytes(), salt, iters);
                let kek = &dk[..32.min(dk.len())];
                if kek.len() == 32 {
                    let ok = kb
                        .wrapped
                        .iter()
                        .filter(|(_, w)| aes_kw_unwrap_for_test(kek, w).is_some())
                        .count();
                    if ok > 0 {
                        unlocked = ok;
                        method = "PBKDF2-HMAC-SHA256（DPSL+DPIC，现代式）";
                    }
                }
            }
        }
    }
    (unlocked > 0, unlocked, method)
}
