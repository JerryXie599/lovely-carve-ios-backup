//! iCloud 服务状态：查找（Find My）系列、iCloud 照片服务与同步状态、
//! iCloud 照片库（store.cloudphotodb）、iCloud Drive 文件域。
//!
//! 仅解析检材中真实存在的配置/数据库文件，键名做一层点号展开
//! （如 `cloudAssetCountPerType.public.image`），时间值统一换算 UTC+8。

use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;

const MAX_BYTES: u64 = 32 << 20;
const MAX_KEYS: usize = 300;

/// 文件名 → (记录类型, 服务名)。
pub fn service_of(lower_name: &str) -> Option<(&'static str, &'static str)> {
    Some(match lower_name {
        "com.apple.cloudphotod.plist" => ("ios.icloud_photo", "iCloud 照片服务"),
        "com.apple.icloud.findmydeviced.plist" => ("ios.icloud_findmy", "查找（Find My 设备）"),
        "com.apple.icloud.findmydeviced.postwipe.plist" => {
            ("ios.icloud_findmy", "查找（Find My 设备，抹除后）")
        }
        "com.apple.icloud.fmfd.plist" => ("ios.icloud_findmy", "查找（Find My 朋友）"),
        "com.apple.icloud.fmflocatord.plist" => ("ios.icloud_findmy", "查找（Find My 定位）"),
        "com.apple.icloud.searchpartyd.plist" => {
            ("ios.icloud_findmy", "查找网络（Find My network）")
        }
        "cloudphotos-1.0.plist"
        | "mobilecpl.plist"
        | "syncstatus.plist"
        | "lastsyncafterlaunch.plist"
        | "downloadcounts.plist" => ("ios.icloud_photo", "iCloud 照片同步"),
        "domains.plist" => ("ios.icloud_drive", "iCloud Drive 文件域"),
        "server-conflig.plist" | "pkg_extensions.plist" => {
            ("ios.icloud_drive", "iCloud Drive（CloudDocs）")
        }
        _ => return None,
    })
}

/// 该文件是否按 iCloud 服务配置解析。
pub fn is_icloud_pref(path: &str, name: &str) -> bool {
    let lower = name.to_lowercase();
    if service_of(&lower).is_some() {
        return true;
    }
    let p = path.to_lowercase();
    lower == "domains.plist" && p.contains("fileprovider")
}

fn emit(
    run: &Run,
    f: &FileRef,
    record: &str,
    service: &str,
    key: &str,
    value: Value,
    time: Option<Timestamp>,
) -> lovelycarve_plugin::Result<()> {
    run.record_values(
        record,
        vec![
            service.into(),
            key.into(),
            value,
            time.map(|t| Value::from(t.as_str().to_string()))
                .unwrap_or(Value::Nil),
            f.name().into(),
        ],
    )
    .source(crate::util::node_source(f))
    .emit()
}

/// plist 递归导出：dict 展开一层点号路径；数组为字符串时拼接，否则摘要。
fn walk(
    run: &Run,
    f: &FileRef,
    record: &str,
    service: &str,
    prefix: &str,
    v: &PValue,
    n: &mut usize,
) -> lovelycarve_plugin::Result<()> {
    if *n >= MAX_KEYS {
        return Ok(());
    }
    match v {
        PValue::Dictionary(d) => {
            for (k, val) in d {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                walk(run, f, record, service, &key, val, n)?;
            }
        }
        PValue::Boolean(b) => {
            emit(
                run,
                f,
                record,
                service,
                prefix,
                (*b).to_string().into(),
                None,
            )?;
            *n += 1;
        }
        PValue::Integer(i) => {
            emit(run, f, record, service, prefix, i.to_string().into(), None)?;
            *n += 1;
        }
        PValue::Real(x) => {
            emit(run, f, record, service, prefix, x.to_string().into(), None)?;
            *n += 1;
        }
        PValue::Date(dt) => {
            let time = std::time::SystemTime::from(*dt)
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|u| crate::util::ts_plus8(u.as_secs() as i64));
            emit(run, f, record, service, prefix, Value::Nil, time)?;
            *n += 1;
        }
        PValue::String(s) => {
            let s = s.trim();
            if !s.is_empty() {
                emit(
                    run,
                    f,
                    record,
                    service,
                    prefix,
                    s.chars().take(500).collect::<String>().into(),
                    None,
                )?;
                *n += 1;
            }
        }
        PValue::Array(a) => {
            let strs: Vec<String> = a
                .iter()
                .filter_map(|x| x.as_string().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            let text = if strs.len() == a.len() && !strs.is_empty() {
                strs.join("；")
            } else {
                format!("[{} 项]", a.len())
            };
            emit(
                run,
                f,
                record,
                service,
                prefix,
                text.chars().take(500).collect::<String>().into(),
                None,
            )?;
            *n += 1;
        }
        PValue::Data(b) => {
            // 魔数识别；bplist 直接解析提取可读内容（如 CloudKit 账户缓存）
            emit(
                run,
                f,
                record,
                service,
                prefix,
                crate::util::describe_blob(b).into(),
                None,
            )?;
            *n += 1;
        }
        _ => {}
    }
    Ok(())
}

/// iCloud 服务配置 / 同步状态 plist。
pub fn prefs(
    run: &Run,
    f: &FileRef,
    target: (&'static str, &'static str),
) -> lovelycarve_plugin::Result<usize> {
    let (record, service) = target;
    let bytes = match f.read_all(MAX_BYTES) {
        Ok(b) => b,
        Err(e) => {
            run.warn_file(f, "ICLOUD_READ", format!("{service} 配置读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let Some(root) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", format!("{service} 配置解析失败"))?;
        return Ok(0);
    };
    let mut n = 0usize;
    walk(run, f, record, service, "", &root, &mut n)?;
    Ok(n)
}

/// store.cloudphotodb：iCloud 照片库记录概览（cloudCache/idMapping/scopes）。
pub fn cloudphotodb(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(f, "DB_OPEN", format!("iCloud 照片库打开失败：{e}"))?;
            return Ok(0);
        }
    };
    let mut n = 0usize;
    for (table, label) in [
        ("cloudCache", "云照片记录数"),
        ("idMapping", "云/本 地标识映射数"),
        ("scopes", "同步域数"),
    ] {
        if let Ok(c) = conn.query_row(&format!("SELECT COUNT(*) FROM [{table}]"), [], |r| {
            r.get::<_, i64>(0)
        }) {
            emit(
                run,
                f,
                "ios.icloud_photo",
                "iCloud 照片库",
                label,
                Value::from(c.to_string()),
                None,
            )?;
            n += 1;
        }
    }
    // 同步域的建立/更新时间
    let ok = conn.prepare(
        "SELECT [scopeIdentifier], [creationDate], [lastLibraryInfoUpdate] FROM [scopes] LIMIT 8",
    );
    if let Ok(mut stmt) = ok {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let id: Option<String> = r.get(0).ok().flatten();
                let created: Option<f64> = r.get(1).ok().flatten();
                let updated: Option<f64> = r.get(2).ok().flatten();
                let time = created.or(updated).and_then(|v| {
                    let secs = if v.abs() > 1e14 {
                        (v / 1e9) as i64
                    } else {
                        v as i64
                    };
                    crate::util::ts_plus8(crate::util::APPLE_EPOCH + secs)
                });
                emit(
                    run,
                    f,
                    "ios.icloud_photo",
                    "iCloud 照片库",
                    &format!("同步域 {}", id.unwrap_or_default()),
                    Value::Nil,
                    time,
                )?;
                n += 1;
            }
        }
    }
    Ok(n)
}
