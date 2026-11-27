//! 加密 iOS 备份（Manifest.db 与 AES-256-CBC 加密分片）内存解密与核心取证库解析。
//!
//! 仅在提供正确 `backup_password` 或通过 `auto_crack_password` 成功爆破出 PIN 码后触发；
//! 全部解密在内存中完成，仅将目标 SQLite 写入临时文件以只读打开复用 `backupdb` 解析逻辑。

use crate::backup::BackupInfo;
use crate::{backupdb, keybag};
use lovelycarve_plugin::{FileRef, Run};
use plist::Value as PValue;
use rusqlite::Connection;
use std::io::Cursor;

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
const MAX_BLOB_READ: u64 = 256 << 20;

struct EncMeta {
    size: Option<usize>,
    protection_class: Option<u32>,
    encryption_key: Option<Vec<u8>>,
}

fn resolve_plist<'a>(objects: &'a [PValue], v: &'a PValue) -> Option<&'a PValue> {
    match v {
        PValue::Uid(u) => objects.get(u.get() as usize),
        other => Some(other),
    }
}

/// 解析 `Files.file` 中嵌在 `$objects` 内的 `NSKeyedArchiver` 元数据（Size / ProtectionClass / EncryptionKey）。
fn parse_files_archive(blob: &[u8]) -> Option<EncMeta> {
    let val = PValue::from_reader(Cursor::new(blob)).ok()?;
    let dict = val.as_dictionary()?;
    let objects = dict.get("$objects")?.as_array()?;
    let root_uid = dict
        .get("$top")?
        .as_dictionary()?
        .get("root")?
        .as_uid()?
        .get() as usize;
    let root = objects.get(root_uid)?.as_dictionary()?;

    let size = root
        .get("Size")
        .and_then(|v| resolve_plist(objects, v))
        .and_then(PValue::as_unsigned_integer)
        .map(|s| s as usize);
    let protection_class = root
        .get("ProtectionClass")
        .and_then(|v| resolve_plist(objects, v))
        .and_then(PValue::as_unsigned_integer)
        .map(|c| c as u32);
    let encryption_key = root
        .get("EncryptionKey")
        .and_then(|v| resolve_plist(objects, v))
        .and_then(|v| {
            if let Some(d) = v.as_data() {
                return Some(d.to_vec());
            }
            let d_dict = v.as_dictionary()?;
            d_dict
                .get("NS.data")
                .and_then(|inner| resolve_plist(objects, inner))
                .and_then(PValue::as_data)
                .map(<[u8]>::to_vec)
        });

    Some(EncMeta {
        size,
        protection_class,
        encryption_key,
    })
}

fn find_view_file(
    run: &Run,
    files: &[(String, FileRef)],
    prefix: &str,
    rel: &str,
    max_bytes: u64,
) -> Option<(FileRef, Vec<u8>)> {
    if let Some(view) = run.view() {
        let candidates = [
            format!("{prefix}{rel}"),
            rel.to_string(),
            format!("part:0/{rel}"),
            format!("part:1/{rel}"),
            format!("root/{rel}"),
        ];
        for p in &candidates {
            if let Ok(Some(f)) = view.open(p) {
                if let Ok(bytes) = f.read_all(max_bytes) {
                    return Some((f, bytes));
                }
            }
        }
    }
    let rel_lower = rel.to_ascii_lowercase();
    for (p, f) in files {
        let p_low = p.to_ascii_lowercase();
        if p_low == rel_lower || p_low.ends_with(&format!("/{rel_lower}")) {
            if let Ok(bytes) = f.read_all(max_bytes) {
                return Some((f.clone(), bytes));
            }
        }
    }
    None
}

fn open_decrypted_blob(
    run: &Run,
    files: &[(String, FileRef)],
    binfo: &BackupInfo,
    file_id: &str,
    meta: &EncMeta,
) -> Option<(FileRef, tempfile::TempDir, Connection)> {
    if file_id.len() < 4 || !file_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rel = format!("{}/{file_id}", &file_id[..2]);
    let (blob_file, raw) = find_view_file(run, files, &binfo.manifest_prefix, &rel, MAX_BLOB_READ)?;

    let plain = if let (Some(pclass), Some(enc_key)) =
        (meta.protection_class, meta.encryption_key.as_deref())
    {
        let file_key = keybag::unwrap_file_key(&binfo.class_keys, pclass, enc_key)?;
        keybag::decrypt_aes256_cbc_zero_iv(&file_key, &raw, meta.size)?
    } else if raw.starts_with(SQLITE_MAGIC) {
        raw
    } else {
        return None;
    };

    if !plain.starts_with(SQLITE_MAGIC) {
        return None;
    }
    let (tmp, conn) = backupdb::open_db_from_bytes(&plain).ok()?;
    Some((blob_file, tmp, conn))
}

fn target_handler(domain: &str, rel_path: &str) -> Option<&'static str> {
    let d = domain.to_ascii_lowercase();
    let r = rel_path.to_ascii_lowercase();
    if d == "homedomain" && r == "library/sms/sms.db" {
        Some("sms")
    } else if d == "homedomain" && r == "library/addressbook/addressbook.sqlitedb" {
        Some("contact")
    } else if d == "homedomain"
        && (r == "library/callhistorydb/callhistory.storedata"
            || r == "library/callhistory/call_history.db")
    {
        Some("call")
    } else if d == "homedomain" && r == "library/safari/history.db" {
        Some("safari")
    } else if d == "homedomain" && r == "library/accounts/accounts3.sqlite" {
        Some("account")
    } else if (d == "homedomain" && r == "library/notes/notes.sqlite")
        || (d.contains("com.apple.notes") && r.ends_with("notestore.sqlite"))
    {
        Some("note")
    } else {
        None
    }
}

/// 当已获取 `class_keys` 时，解密 `Manifest.db` 与核心取证数据库并输出标准记录。
pub fn analyze_encrypted_backup(
    run: &Run,
    binfo: &BackupInfo,
    files: &[(String, FileRef)],
) -> lovelycarve_plugin::Result<()> {
    if binfo.class_keys.is_empty() {
        return Ok(());
    }
    let Some((manifest_file, manifest_raw)) = find_view_file(
        run,
        files,
        &binfo.manifest_prefix,
        "Manifest.db",
        MAX_BLOB_READ,
    ) else {
        return Ok(());
    };

    let manifest_plain = if manifest_raw.starts_with(SQLITE_MAGIC) {
        manifest_raw
    } else {
        let Some(mk) = binfo.manifest_key.as_deref() else {
            return Ok(());
        };
        if mk.len() < 44 {
            run.warn_file(
                &manifest_file,
                "MANIFEST_KEY",
                "ManifestKey 长度异常，无法解密 Manifest.db",
            )?;
            return Ok(());
        }
        let pclass = u32::from_le_bytes([mk[0], mk[1], mk[2], mk[3]]);
        let Some(file_key) = keybag::unwrap_file_key(&binfo.class_keys, pclass, mk) else {
            run.warn_file(
                &manifest_file,
                "MANIFEST_DECRYPT",
                "无法使用类密钥解开 ManifestKey",
            )?;
            return Ok(());
        };
        let Some(decrypted) = keybag::decrypt_aes256_cbc_zero_iv(&file_key, &manifest_raw, None)
        else {
            run.warn_file(
                &manifest_file,
                "MANIFEST_DECRYPT",
                "Manifest.db AES-CBC 解密失败",
            )?;
            return Ok(());
        };
        if !decrypted.starts_with(SQLITE_MAGIC) {
            run.warn_file(
                &manifest_file,
                "MANIFEST_DECRYPT",
                "解密后的 Manifest.db 非有效 SQLite 头",
            )?;
            return Ok(());
        }
        decrypted
    };

    let Ok((_mtmp, mconn)) = backupdb::open_db_from_bytes(&manifest_plain) else {
        run.warn_file(&manifest_file, "MANIFEST_SQL", "无法打开 Manifest.db")?;
        return Ok(());
    };

    let total_rows: i64 = mconn
        .query_row("SELECT COUNT(*) FROM Files", [], |r| r.get(0))
        .unwrap_or(0);

    let Ok(mut stmt) = mconn.prepare(
        "SELECT fileID, domain, relativePath, file FROM Files WHERE flags = 1 AND file IS NOT NULL",
    ) else {
        return Ok(());
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(());
    };

    let mut decrypted_dbs = 0usize;
    let mut emitted_records = 0usize;

    while let Ok(Some(row)) = rows.next() {
        run.check_cancelled()?;
        let Ok(fid) = row.get::<_, String>(0) else {
            continue;
        };
        let Ok(domain) = row.get::<_, String>(1) else {
            continue;
        };
        let Ok(rel_path) = row.get::<_, String>(2) else {
            continue;
        };
        let Some(handler) = target_handler(&domain, &rel_path) else {
            continue;
        };
        let Ok(file_blob) = row.get::<_, Vec<u8>>(3) else {
            continue;
        };
        let Some(meta) = parse_files_archive(&file_blob) else {
            continue;
        };
        let Some((blob_file, _btmp, bconn)) = open_decrypted_blob(run, files, binfo, &fid, &meta)
        else {
            continue;
        };
        decrypted_dbs += 1;
        let count = match handler {
            "sms" => backupdb::sms_conn(run, &blob_file, &bconn)?,
            "contact" => backupdb::contact_conn(run, &blob_file, &bconn)?,
            "call" => backupdb::calllog_conn(run, &blob_file, &bconn)?,
            "safari" => backupdb::web_history_conn(run, &blob_file, &bconn)?,
            "account" => backupdb::account_conn(run, &blob_file, &bconn)?,
            "note" => backupdb::note_conn(run, &blob_file, &bconn)?,
            _ => 0,
        };
        emitted_records += count;
    }

    run.record_values(
        "ios.finding",
        vec![
            "加密备份清单与核心库解密".into(),
            format!(
                "已解密 Manifest.db（{total_rows} 条记录），成功解出并解析 {decrypted_dbs} 个核心库，产出 {emitted_records} 条取证记录"
            )
            .into(),
            "Manifest.plist BackupKeyBag + AES-256-CBC 内存解密".into(),
        ],
    )
    .from(&manifest_file)
    .emit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Block;
    use lovelycarve_plugin::testing::{Fixture, Harness};
    use lovelycarve_plugin::Value;
    use std::collections::BTreeMap;

    fn aes256_kw_wrap(kek: &[u8; 32], key: &[u8; 32]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(kek).expect("aes256 key");
        let n = 4u64;
        let mut a = [0xA6u8; 8];
        let mut r = [[0u8; 8]; 4];
        for i in 0..4 {
            r[i].copy_from_slice(&key[8 * i..8 * (i + 1)]);
        }
        for j in 0..6u64 {
            for i in 1..=n {
                let mut block = Block::from([0u8; 16]);
                block[..8].copy_from_slice(&a);
                block[8..].copy_from_slice(&r[(i - 1) as usize]);
                cipher.encrypt_block(&mut block);
                let t = n * j + i;
                a.copy_from_slice(&block[..8]);
                let tb = t.to_be_bytes();
                for k in 0..8 {
                    a[k] ^= tb[k];
                }
                r[(i - 1) as usize].copy_from_slice(&block[8..]);
            }
        }
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(&a);
        for chunk in &r {
            out.extend_from_slice(chunk);
        }
        out
    }

    fn aes256_cbc_encrypt_zero_iv_pkcs7(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(key).expect("aes256 key");
        let pad_len = 16 - (plain.len() % 16);
        let mut padded = plain.to_vec();
        padded.resize(plain.len() + pad_len, pad_len as u8);

        let mut out = Vec::with_capacity(padded.len());
        let mut prev = [0u8; 16];
        for chunk in padded.chunks_exact(16) {
            let mut block = Block::from([0u8; 16]);
            for i in 0..16 {
                block[i] = chunk[i] ^ prev[i];
            }
            cipher.encrypt_block(&mut block);
            out.extend_from_slice(&block);
            prev.copy_from_slice(&block);
        }
        out
    }

    fn tlv(tag: &[u8; 4], val: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + val.len());
        out.extend_from_slice(tag);
        out.extend_from_slice(&(val.len() as u32).to_be_bytes());
        out.extend_from_slice(val);
        out
    }

    fn build_encrypted_backup_fixture(pin: &str, phone: &str, body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let salt = b"0123456789abcdef";
        let dpsl = b"fedcba9876543210";
        let iter = 1u32;
        let dpic = 1u32;

        let step1 = crate::keybag::pbkdf2_hmac_sha256(pin.as_bytes(), dpsl, dpic, 32);
        let kek_vec = crate::keybag::pbkdf2_hmac_sha1(&step1, salt, iter, 32);
        let kek: [u8; 32] = kek_vec.try_into().expect("32-byte kek");

        let class_key = [0x42u8; 32];
        let wpky = aes256_kw_wrap(&kek, &class_key);

        let mut keybag_blob = Vec::new();
        keybag_blob.extend_from_slice(&tlv(b"VERS", &3u32.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"TYPE", &1u32.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"UUID", &[0x11u8; 16]));
        keybag_blob.extend_from_slice(&tlv(b"SALT", salt));
        keybag_blob.extend_from_slice(&tlv(b"ITER", &iter.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"DPSL", dpsl));
        keybag_blob.extend_from_slice(&tlv(b"DPIC", &dpic.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"UUID", &[0x22u8; 16]));
        keybag_blob.extend_from_slice(&tlv(b"CLAS", &3u32.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"WRAP", &2u32.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"KTYP", &0u32.to_be_bytes()));
        keybag_blob.extend_from_slice(&tlv(b"WPKY", &wpky));

        let manifest_file_key = [0x33u8; 32];
        let mut manifest_key = Vec::with_capacity(44);
        manifest_key.extend_from_slice(&3u32.to_le_bytes());
        manifest_key.extend_from_slice(&aes256_kw_wrap(&class_key, &manifest_file_key));

        let mut manifest_dict = plist::Dictionary::new();
        manifest_dict.insert("IsEncrypted".into(), plist::Value::Boolean(true));
        manifest_dict.insert("Version".into(), plist::Value::String("10.0".into()));
        manifest_dict.insert("BackupKeyBag".into(), plist::Value::Data(keybag_blob));
        manifest_dict.insert("ManifestKey".into(), plist::Value::Data(manifest_key));
        let mut lockdown = plist::Dictionary::new();
        lockdown.insert(
            "SerialNumber".into(),
            plist::Value::String("SN99887766".into()),
        );
        lockdown.insert(
            "DeviceName".into(),
            plist::Value::String("Test iPhone".into()),
        );
        manifest_dict.insert("Lockdown".into(), plist::Value::Dictionary(lockdown));
        plist::Value::Dictionary(manifest_dict)
            .to_file_binary(dir.path().join("Manifest.plist"))
            .expect("write Manifest.plist");

        // 构造 sms.db 并用单文件密钥加密
        let sms_plain_path = dir.path().join("plain_sms.db");
        {
            let conn = rusqlite::Connection::open(&sms_plain_path).expect("create sms.db");
            conn.execute_batch(
                "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
                 CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, date INTEGER, is_from_me INTEGER, is_read INTEGER, service TEXT, subject TEXT, handle_id INTEGER);
                 INSERT INTO handle (ROWID, id) VALUES (1, '');
                 INSERT INTO message (ROWID, text, date, is_from_me, is_read, service, subject, handle_id) VALUES (1, '', 700000000, 1, 1, 'SMS', NULL, 1);",
            )
            .expect("init sms schema");
            conn.execute("UPDATE handle SET id = ?1 WHERE ROWID = 1", [phone])
                .expect("update phone");
            conn.execute("UPDATE message SET text = ?1 WHERE ROWID = 1", [body])
                .expect("update body");
        }
        let sms_plain = std::fs::read(&sms_plain_path).expect("read sms_plain");
        let _ = std::fs::remove_file(&sms_plain_path);

        let sms_file_key = [0x55u8; 32];
        let mut sms_enc_key = Vec::with_capacity(44);
        sms_enc_key.extend_from_slice(&3u32.to_le_bytes());
        sms_enc_key.extend_from_slice(&aes256_kw_wrap(&class_key, &sms_file_key));
        let sms_cipher = aes256_cbc_encrypt_zero_iv_pkcs7(&sms_file_key, &sms_plain);

        let sms_fid = "3d0d7e5fb2ce288813306e4d4636395e047a3d28";
        std::fs::create_dir_all(dir.path().join("3d")).expect("create shard dir");
        std::fs::write(dir.path().join("3d").join(sms_fid), &sms_cipher)
            .expect("write encrypted sms shard");

        // 构造 NSKeyedArchiver Files.file blob
        let mut top = plist::Dictionary::new();
        top.insert("root".into(), plist::Value::Uid(plist::Uid::new(1)));
        let mut root_obj = plist::Dictionary::new();
        root_obj.insert(
            "Size".into(),
            plist::Value::Integer((sms_plain.len() as u64).into()),
        );
        root_obj.insert("ProtectionClass".into(), plist::Value::Integer(3u64.into()));
        root_obj.insert(
            "EncryptionKey".into(),
            plist::Value::Uid(plist::Uid::new(2)),
        );
        let mut nsdata_dict = plist::Dictionary::new();
        nsdata_dict.insert("NS.data".into(), plist::Value::Data(sms_enc_key));
        let mut archive = plist::Dictionary::new();
        archive.insert("$top".into(), plist::Value::Dictionary(top));
        archive.insert(
            "$objects".into(),
            plist::Value::Array(vec![
                plist::Value::String("$null".into()),
                plist::Value::Dictionary(root_obj),
                plist::Value::Dictionary(nsdata_dict),
            ]),
        );
        let mut archive_bytes = Vec::new();
        plist::Value::Dictionary(archive)
            .to_writer_binary(&mut archive_bytes)
            .expect("encode NSKeyedArchiver");

        // 构造 Manifest.db 并用 manifest_file_key 加密
        let manifest_plain_path = dir.path().join("plain_manifest.db");
        {
            let conn =
                rusqlite::Connection::open(&manifest_plain_path).expect("create Manifest.db");
            conn.execute_batch(
                "CREATE TABLE Files (fileID TEXT PRIMARY KEY, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);",
            )
            .expect("create Files");
            conn.execute(
                "INSERT INTO Files (fileID, domain, relativePath, flags, file) VALUES (?1, ?2, ?3, 1, ?4)",
                rusqlite::params![sms_fid, "HomeDomain", "Library/SMS/sms.db", archive_bytes],
            )
            .expect("insert sms record");
        }
        let manifest_plain = std::fs::read(&manifest_plain_path).expect("read manifest_plain");
        let _ = std::fs::remove_file(&manifest_plain_path);
        let manifest_cipher = aes256_cbc_encrypt_zero_iv_pkcs7(&manifest_file_key, &manifest_plain);
        std::fs::write(dir.path().join("Manifest.db"), &manifest_cipher)
            .expect("write encrypted Manifest.db");

        dir
    }

    #[test]
    fn ios_backup_auto_cracks_pin_and_decrypts_manifest_and_sms() {
        let dir = build_encrypted_backup_fixture("0451", "+8613800138000", "自动爆破解密成功");
        let mut options = BTreeMap::new();
        options.insert("auto_crack_password".into(), Value::Boolean(true));

        let run = Harness::new(crate::plugin)
            .run_with_inputs(
                "ios_backup",
                Fixture::evidence(&[("part:1", dir.path())]),
                &[
                    "Manifest.plist",
                    "Manifest.db",
                    "**/Manifest.plist",
                    "**/Manifest.db",
                ],
                options,
                BTreeMap::new(),
            )
            .expect("ios_backup run");

        let hit = run
            .of_type("ios.keybag")
            .into_iter()
            .find(|r| r.text("key") == Some("自动爆破结果"))
            .expect("crack result record");
        assert_eq!(hit.text("value"), Some("0451"));

        let sms = run.of_type("ios.sms");
        assert_eq!(sms.len(), 1);
        assert_eq!(sms[0].text("phone"), Some("+8613800138000"));
        assert_eq!(sms[0].text("content"), Some("自动爆破解密成功"));
    }

    #[test]
    fn ios_backup_password_unlocks_and_decrypts_manifest_and_sms() {
        let dir = build_encrypted_backup_fixture("8291", "+8613900001111", "密码解密成功");
        let mut options = BTreeMap::new();
        options.insert("backup_password".into(), Value::from("8291"));

        let run = Harness::new(crate::plugin)
            .run_with_inputs(
                "ios_backup",
                Fixture::evidence(&[("part:1", dir.path())]),
                &[
                    "Manifest.plist",
                    "Manifest.db",
                    "**/Manifest.plist",
                    "**/Manifest.db",
                ],
                options,
                BTreeMap::new(),
            )
            .expect("ios_backup run");

        let verify = run
            .of_type("ios.keybag")
            .into_iter()
            .find(|r| r.text("key") == Some("口令验证"))
            .expect("verify record");
        assert_eq!(verify.text("value"), Some("正确"));

        let sms = run.of_type("ios.sms");
        assert_eq!(sms.len(), 1);
        assert_eq!(sms[0].text("phone"), Some("+8613900001111"));
        assert_eq!(sms[0].text("content"), Some("密码解密成功"));
    }
}
