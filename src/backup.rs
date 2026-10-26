//! iOS 备份控制文件解析：Manifest.plist / Status.plist / Info.plist + BackupKeyBag。
//! 输出 iOS 备份信息记录（字段沿用 iOS 备份领域通用命名）。

use crate::util::{iphone_model, ts_plus8, ts_utc};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;

pub struct BackupInfo {
    pub device_name: String,
    pub display_name: String,
    pub product_name: String,
    pub product_type: String,
    pub product_version: String,
    pub build_version: String,
    pub serial: String,
    pub udid: String,
    pub imei: String,
    pub iccid: String,
    pub phone: String,
    pub backup_unix: Option<i64>,
    pub backup_raw: String,
    pub version: String,
    pub system_domains: String,
    pub encrypt: bool,
    pub passcode_set: bool,
    pub app_count: i64,
    pub lockdown_keys: Vec<(String, String)>,
    pub keybag: Option<KeyBag>,
}

#[derive(Default)]
pub struct KeyBag {
    pub attrs: Vec<(String, String, String)>, // (key, value, note)
    pub class_count: usize,
    pub ktyp0: usize,
}

fn pstr(d: &plist::Dictionary, k: &str) -> Option<String> {
    d.get(k).and_then(PValue::as_string).map(|s| s.to_string())
}

fn pdate_unix(d: &plist::Dictionary, k: &str) -> (Option<i64>, String) {
    match d.get(k).and_then(PValue::as_date) {
        Some(date) => {
            let secs = std::time::SystemTime::from(date)
                .duration_since(std::time::UNIX_EPOCH)
                .map(|du| du.as_secs() as i64)
                .unwrap_or(0);
            (Some(secs), date.to_xml_format())
        }
        None => (None, String::new()),
    }
}

fn be32(v: &[u8]) -> Option<u32> {
    v.get(0..4).and_then(|s| <[u8;4]>::try_from(s).ok()).map(u32::from_be_bytes)
}

/// 解析 BackupKeyBag 的 TLV 结构（VERS/TYPE/SALT/ITER/DPWT/DPIC/DPSL/UUID/CLAS/KTYP/WPKY）。
fn parse_keybag(blob: &[u8]) -> Option<KeyBag> {
    if blob.len() < 8 || &blob[..4] != b"VERS" {
        return None;
    }
    let mut kb = KeyBag::default();
    let mut off = 0usize;
    let mut ktyp: Option<u32> = None;
    let mut cls_count = 0usize;
    let mut root_uuid = true;
    while off + 8 <= blob.len() {
        let tag = String::from_utf8_lossy(&blob[off..off + 4]).to_string();
        let len = u32::from_be_bytes(blob[off + 4..off + 8].try_into().ok()?) as usize;
        let val = blob.get(off + 8..off + 8 + len)?;
        off += 8 + len;
        let u = be32(val);
        match tag.as_str() {
            "VERS" if u.is_some() => kb.attrs.push(("VERS".into(), u.unwrap().to_string(), "密钥包格式版本".into())),
            "TYPE" if u.is_some() => kb.attrs.push((
                "TYPE".into(),
                u.unwrap().to_string(),
                if u == Some(1) { "密钥包类型=1（设备密钥包）" } else { "密钥包类型" }.into(),
            )),
            "UUID" if root_uuid => kb.attrs.push(("UUID".into(), hex(val), "密钥包 UUID".into())),
            "HMCK" => {}
            "SALT" => kb.attrs.push(("SALT".into(), hex(val), "PBKDF2 盐（hex）".into())),
            "ITER" if u.is_some() => kb.attrs.push(("ITER".into(), u.unwrap().to_string(), "PBKDF2 迭代次数（老式口令派生）".into())),
            "DPWT" if u.is_some() => kb.attrs.push(("DPWT".into(), u.unwrap().to_string(), "口令派生方式标记".into())),
            "DPIC" if u.is_some() => kb.attrs.push(("DPIC".into(), u.unwrap().to_string(), "口令迭代次数（DPKF，现代口令派生）".into())),
            "DPSL" => kb.attrs.push(("DPSL".into(), hex(val), "口令派生盐（hex）".into())),
            "CLAS" => {
                ktyp = None;
                cls_count += 1;
                root_uuid = false;
            }
            "KTYP" => ktyp = u,
            "WPKY" => {
                if ktyp == Some(0) {
                    kb.ktyp0 += 1;
                }
            }
            _ => {}
        }
    }
    kb.class_count = cls_count;
    Some(kb)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 解析 Manifest.plist / Info.plist / Status.plist，产出记录。返回 (BackupInfo 摘要)。
pub fn analyze(run: &Run, files: &[(String, FileRef)]) -> lovelycarve_plugin::Result<Option<BackupInfo>> {
    let manifest = files
        .iter()
        .find(|(p, _)| p.rsplit('/').next().map(|n| n.eq_ignore_ascii_case("Manifest.plist")).unwrap_or(false))
        .or_else(|| files.iter().find(|(p, f)| p.ends_with("Manifest.plist") || f.name().eq_ignore_ascii_case("Manifest.plist")));
    let manifest = match manifest {
        Some((p, f)) => (p.clone(), f),
        None => return Ok(None),
    };
    let bytes = manifest.1.read_all(64 << 20)?;
    let plist = match PValue::from_reader(std::io::Cursor::new(&bytes)) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(manifest.1, "PLIST_PARSE", format!("Manifest.plist 解析失败：{e}"))?;
            return Ok(None);
        }
    };
    let d = match plist.as_dictionary() {
        Some(d) => d,
        None => return Ok(None),
    };

    let lockdown: plist::Dictionary = d
        .get("Lockdown")
        .and_then(PValue::as_dictionary)
        .cloned()
        .unwrap_or_default();
    let apps = d.get("Applications").and_then(PValue::as_dictionary);

    let (backup_unix, backup_raw) = pdate_unix(d, "Date");
    let mut info = BackupInfo {
        device_name: pstr(&lockdown, "DeviceName").unwrap_or_default(),
        display_name: pstr(&lockdown, "DisplayName").unwrap_or_default(),
        product_name: pstr(&lockdown, "ProductName").unwrap_or_else(|| "iPhone OS".into()),
        product_type: pstr(&lockdown, "ProductType").unwrap_or_default(),
        product_version: pstr(&lockdown, "ProductVersion").unwrap_or_default(),
        build_version: pstr(&lockdown, "BuildVersion")
            .or_else(|| pstr(d, "BuildVersion"))
            .unwrap_or_default(),
        serial: pstr(&lockdown, "SerialNumber").unwrap_or_default(),
        udid: pstr(&lockdown, "UniqueDeviceID").unwrap_or_default(),
        imei: pstr(&lockdown, "InternationalMobileEquipmentIdentity").unwrap_or_default(),
        iccid: pstr(&lockdown, "IntegratedCircuitCardIdentity").unwrap_or_default(),
        phone: pstr(&lockdown, "PhoneNumber").unwrap_or_default(),
        backup_unix,
        backup_raw: backup_raw.clone(),
        version: pstr(d, "Version").unwrap_or_default(),
        system_domains: pstr(d, "SystemDomainsVersion").unwrap_or_default(),
        encrypt: d.get("IsEncrypted").and_then(PValue::as_boolean).unwrap_or(false),
        passcode_set: d.get("WasPasscodeSet").and_then(PValue::as_boolean).unwrap_or(false),
        app_count: apps.map(|a| a.len() as i64).unwrap_or(0),
        lockdown_keys: lockdown
            .iter()
            .filter(|(_, v)| v.as_string().is_some())
            .map(|(k, v)| (k.to_string(), v.as_string().unwrap_or("").to_string()))
            .collect(),
        keybag: d.get("BackupKeyBag").and_then(PValue::as_data).and_then(parse_keybag),
    };

    // ---- ios.backup ----
    let model = iphone_model(&info.product_type).map(|m| m.to_string()).unwrap_or_default();
    let mut v: Vec<Value> = vec![
        info.device_name.clone().into(),
        info.display_name.clone().into(),
        info.product_name.clone().into(),
        info.product_type.clone().into(),
        model.into(),
        info.product_version.clone().into(),
        info.build_version.clone().into(),
        info.serial.clone().into(),
        info.udid.clone().into(),
        info.imei.clone().into(),
        info.iccid.clone().into(),
        info.phone.clone().into(),
    ];
    v.push(match info.backup_unix {
        Some(u) => Value::from(ts_plus8(u).map(|t: Timestamp| t.as_str().to_string()).unwrap_or_default()),
        None => Value::Nil,
    });
    v.push(match info.backup_unix {
        Some(u) => Value::from(ts_utc(u).as_str().to_string()),
        None => Value::Nil,
    });
    v.push(info.backup_raw.clone().into());
    v.push(info.version.clone().into());
    v.push(info.system_domains.clone().into());
    v.push(info.encrypt.into());
    v.push(info.passcode_set.into());
    v.push(info.app_count.into());
    v.push(manifest.0.clone().into());
    run.record_values("ios.backup", v)
        .from(manifest.1)
        .note("Manifest.plist")
        .emit()?;

    // ---- ios.device_info：Lockdown 全部字符串键 ----
    for (k, val) in &info.lockdown_keys {
        run.record_values(
            "ios.device_info",
            vec![
                k.clone().into(),
                human_key(k).into(),
                val.clone().into(),
                "Manifest.plist/Lockdown".into(),
            ],
        )
        .from(manifest.1)
        .emit()?;
    }
    // ---- 口令验证（用户输入密码选项） ----
    if let Some(pwd) = run.options().text("backup_password").filter(|p| !p.is_empty()) {
        if let Some(blob) = d.get("BackupKeyBag").and_then(PValue::as_data) {
            if let Some(kb) = crate::keybag::parse(blob) {
                let (ok, count, method) = crate::keybag::verify(&kb, pwd);
                run.record_values(
                    "ios.keybag",
                    vec![
                        "口令验证".into(),
                        if ok { "正确".into() } else { "错误".into() },
                        Value::from(format!(
                            "密码{}；派生 {}；解开类密钥 {}/{} 个",
                            if ok { "正确" } else { "错误" },
                            method,
                            count,
                            kb.wrapped.len()
                        )),
                    ],
                )
                .from(manifest.1)
                .emit()?;
            }
        }
    }
    if let Some(kb) = &info.keybag {
        for (k, val, note) in std::iter::once((
            "ClassKeys".to_string(),
            format!("{} 个（其中口令包裹 KTYP=0 共 {} 个）", kb.class_count, kb.ktyp0),
            "保护类密钥统计".to_string(),
        ))
        .chain(kb.attrs.iter().cloned())
        {
            run.record_values("ios.keybag", vec![k.into(), val.into(), note.into()])
                .from(manifest.1)
                .emit()?;
        }
    } else {
        run.record_values(
            "ios.keybag",
            vec![
                "BackupKeyBag".into(),
                "未找到或非 TLV 格式".into(),
                "Manifest.plist 无 BackupKeyBag 数据".into(),
            ],
        )
        .emit()?;
    }

    // ---- Status.plist / Info.plist（若检材带有完整备份根） ----
    for (want, note) in [("Status.plist", "备份状态"), ("Info.plist", "备份设备信息")] {
        if let Some((p, f)) = files.iter().find(|(p, _)| p.ends_with(want)) {
            if let Ok(bytes) = f.read_all(16 << 20) {
                if let Ok(PValue::Dictionary(d2)) = PValue::from_reader(std::io::Cursor::new(&bytes)) {
                    for (k, val) in d2.iter().filter(|(_, v)| v.as_string().is_some() || v.as_boolean().is_some()) {
                        let s = val
                            .as_string()
                            .map(|s| s.to_string())
                            .or_else(|| val.as_boolean().map(|b| b.to_string()))
                            .unwrap_or_default();
                        run.record_values(
                            "ios.device_info",
                            vec![format!("{want}/{k}").into(), note.into(), s.into(), p.clone().into()],
                        )
                        .from(f)
                        .emit()?;
                    }
                }
            }
        }
    }

    Ok(Some(info))
}

fn human_key(k: &str) -> &'static str {
    match k {
        "DeviceName" => "设备名称",
        "DisplayName" => "显示名称",
        "ProductName" => "系统名称",
        "ProductType" => "系统类型",
        "ProductVersion" => "系统版本",
        "BuildVersion" => "构建版本",
        "SerialNumber" => "序列号",
        "UniqueDeviceID" => "UDID",
        "InternationalMobileEquipmentIdentity" => "IMEI",
        "IntegratedCircuitCardIdentity" => "ICCID",
        "PhoneNumber" => "手机号码",
        _ => "",
    }
}

/// Manifest Applications + MobileBackup.plist PreflightSizing 合并的应用清单。
pub fn apps_from_manifest(run: &Run, files: &[(String, FileRef)]) -> lovelycarve_plugin::Result<Vec<(String, String, String, bool)>> {
    // (bundle, name, placeholder)
    let mut out = Vec::new();
    let Some((mp, mf)) = files
        .iter()
        .find(|(p, _)| p.ends_with("Manifest.plist"))
        .map(|(p, f)| (p.clone(), f))
    else {
        return Ok(out);
    };
    let Ok(bytes) = mf.read_all(64 << 20) else { return Ok(out) };
    let Ok(PValue::Dictionary(d)) = PValue::from_reader(std::io::Cursor::new(&bytes)) else {
        return Ok(out);
    };
    let Some(apps) = d.get("Applications").and_then(PValue::as_dictionary) else {
        return Ok(out);
    };
    for (bundle, v) in apps {
        let a = v.as_dictionary();
        let name = a
            .and_then(|a| a.get("CFBundleDisplayName").or_else(|| a.get("CFBundleName")))
            .and_then(PValue::as_string)
            .unwrap_or("")
            .to_string();
        let placeholder = a
            .and_then(|a| a.get("Placeholder"))
            .and_then(PValue::as_boolean)
            .unwrap_or(false);
        out.push((bundle.clone(), name, String::new(), placeholder));
    }
    let _ = (mp, run);
    Ok(out)
}
