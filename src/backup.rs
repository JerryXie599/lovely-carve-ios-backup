//! iOS 备份控制文件解析：Manifest.plist / Status.plist / Info.plist / iDevice_info.txt + BackupKeyBag。
//! 输出 iOS 备份信息与去重对齐的设备属性记录。

use crate::util::{iphone_model, ts_plus8, ts_utc};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;
use std::collections::{HashMap, HashSet};

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
    pub resolved_password: Option<String>,
    pub password_source: Option<&'static str>,
    pub class_keys: Vec<(u32, [u8; 32])>,
    pub manifest_key: Option<Vec<u8>>,
    pub manifest_prefix: String,
}

#[derive(Default)]
pub struct KeyBag {
    pub attrs: Vec<(String, String, String)>, // (key, value, note)
    pub class_count: usize,
    pub ktyp0: usize,
}

#[derive(Default, Debug, Clone)]
pub struct IDeviceInfo {
    pub raw: HashMap<String, String>,
    pub imei1: Option<String>,
    pub imei2: Option<String>,
    pub meid: Option<String>,
    pub iccid: Option<String>,
    pub imsi: Option<String>,
    pub serial_number: Option<String>,
    pub udid: Option<String>,
    pub product_type: Option<String>,
    pub product_version: Option<String>,
    pub build_version: Option<String>,
    pub device_name: Option<String>,
    pub model_number: Option<String>,
    pub region_info: Option<String>,
    pub wifi_mac: Option<String>,
    pub bluetooth_mac: Option<String>,
    pub ethernet_mac: Option<String>,
    pub carrier: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Default, Debug, Clone)]
pub struct InfoPlistData {
    pub raw: HashMap<String, String>,
    pub imei: Option<String>,
    pub meid: Option<String>,
    pub iccid: Option<String>,
    pub phone: Option<String>,
    pub serial_number: Option<String>,
    pub udid: Option<String>,
    pub product_type: Option<String>,
    pub product_version: Option<String>,
    pub build_version: Option<String>,
    pub device_name: Option<String>,
    pub display_name: Option<String>,
    pub guid: Option<String>,
}

#[derive(Default, Debug, Clone)]
pub struct StatusPlistData {
    pub raw: HashMap<String, String>,
    pub backup_state: Option<String>,
    pub snapshot_state: Option<String>,
    pub is_full: Option<bool>,
    pub version: Option<String>,
    pub uuid: Option<String>,
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
    v.get(0..4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_be_bytes)
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
            "VERS" if u.is_some() => kb.attrs.push((
                "VERS".into(),
                u.unwrap().to_string(),
                "密钥包格式版本".into(),
            )),
            "TYPE" if u.is_some() => kb.attrs.push((
                "TYPE".into(),
                u.unwrap().to_string(),
                if u == Some(1) {
                    "密钥包类型=1（设备密钥包）"
                } else {
                    "密钥包类型"
                }
                .into(),
            )),
            "UUID" if root_uuid => kb
                .attrs
                .push(("UUID".into(), hex(val), "密钥包 UUID".into())),
            "HMCK" => {}
            "SALT" => kb
                .attrs
                .push(("SALT".into(), hex(val), "PBKDF2 盐（hex）".into())),
            "ITER" if u.is_some() => kb.attrs.push((
                "ITER".into(),
                u.unwrap().to_string(),
                "PBKDF2 迭代次数（老式口令派生）".into(),
            )),
            "DPWT" if u.is_some() => kb.attrs.push((
                "DPWT".into(),
                u.unwrap().to_string(),
                "口令派生方式标记".into(),
            )),
            "DPIC" if u.is_some() => kb.attrs.push((
                "DPIC".into(),
                u.unwrap().to_string(),
                "口令迭代次数（DPKF，现代口令派生）".into(),
            )),
            "DPSL" => kb
                .attrs
                .push(("DPSL".into(), hex(val), "口令派生盐（hex）".into())),
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

/// 解析 iDevice_info.txt 文件（ideviceinfo 硬件/在线提取格式）。
fn parse_idevice_info_text(content: &str) -> IDeviceInfo {
    let mut info = IDeviceInfo::default();
    let mut in_carrier = false;
    let mut carrier_id = String::new();
    let mut mcc = String::new();
    let mut mnc = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 若遇到钥匙串提取段，退出设备属性解析
        if trimmed.starts_with("keyChain_Begin") || trimmed.starts_with("<< = Row") {
            break;
        }
        if trimmed.starts_with("CarrierBundleInfoArray:") {
            in_carrier = true;
            continue;
        }
        if in_carrier && (trimmed.starts_with("CertID:") || trimmed.starts_with("ChipID:")) {
            in_carrier = false;
        }

        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.is_empty() || val.is_empty() {
                continue;
            }

            if in_carrier {
                match key.as_str() {
                    "CFBundleIdentifier" => carrier_id = val.clone(),
                    "MCC" => mcc = val.clone(),
                    "MNC" => mnc = val.clone(),
                    _ => {}
                }
            }

            match key.as_str() {
                "InternationalMobileEquipmentIdentity" => info.imei1 = Some(val.clone()),
                "InternationalMobileEquipmentIdentity2" => info.imei2 = Some(val.clone()),
                "MobileEquipmentIdentifier" => info.meid = Some(val.clone()),
                "IntegratedCircuitCardIdentity" => {
                    if info.iccid.is_none() {
                        info.iccid = Some(val.clone());
                    }
                }
                "InternationalMobileSubscriberIdentity" => {
                    if info.imsi.is_none() {
                        info.imsi = Some(val.clone());
                    }
                }
                "SerialNumber" => info.serial_number = Some(val.clone()),
                "UniqueDeviceID" => info.udid = Some(val.clone()),
                "ProductType" => info.product_type = Some(val.clone()),
                "ProductVersion" => info.product_version = Some(val.clone()),
                "BuildVersion" => info.build_version = Some(val.clone()),
                "DeviceName" => info.device_name = Some(val.clone()),
                "ModelNumber" => info.model_number = Some(val.clone()),
                "RegionInfo" => info.region_info = Some(val.clone()),
                "WiFiAddress" => info.wifi_mac = Some(val.clone()),
                "BluetoothAddress" => info.bluetooth_mac = Some(val.clone()),
                "EthernetAddress" => info.ethernet_mac = Some(val.clone()),
                "TimeZone" => info.timezone = Some(val.clone()),
                _ => {}
            }

            info.raw.insert(key, val);
        }
    }

    if !carrier_id.is_empty() {
        let carrier_name = if carrier_id.contains("CMCC") {
            "中国移动香港 (CMCC_hk)"
        } else if carrier_id.contains("UNICOM") || carrier_id.contains("Unicom") {
            "中国联通 (China Unicom)"
        } else if carrier_id.contains("CT") || carrier_id.contains("Telecom") {
            "中国电信 (China Telecom)"
        } else if carrier_id.contains("CSL") {
            "香港 CSL"
        } else if carrier_id.contains("SmarTone") {
            "香港数码通 (SmarTone)"
        } else if carrier_id.contains("Three") || carrier_id.contains("3_") {
            "香港 3HK"
        } else {
            &carrier_id
        };

        if !mcc.is_empty() && !mnc.is_empty() {
            info.carrier = Some(format!("{carrier_name} (MCC:{mcc} MNC:{mnc})"));
        } else {
            info.carrier = Some(carrier_name.to_string());
        }
    }

    info
}

/// 解析 Info.plist 字典。
fn parse_info_plist_dict(d: &plist::Dictionary) -> InfoPlistData {
    let mut data = InfoPlistData::default();
    for (k, v) in d {
        let val_str = if let Some(s) = v.as_string() {
            s.to_string()
        } else if let Some(b) = v.as_boolean() {
            b.to_string()
        } else if let Some(i) = v.as_signed_integer() {
            i.to_string()
        } else if let Some(u) = v.as_unsigned_integer() {
            u.to_string()
        } else if let Some(date) = v.as_date() {
            date.to_xml_format()
        } else {
            continue;
        };

        match k.as_str() {
            "IMEI" => data.imei = Some(val_str.clone()),
            "MEID" => data.meid = Some(val_str.clone()),
            "ICCID" => data.iccid = Some(val_str.clone()),
            "Phone Number" => data.phone = Some(val_str.clone()),
            "Serial Number" => data.serial_number = Some(val_str.clone()),
            "Target Identifier" | "Unique Identifier" => {
                if data.udid.is_none() {
                    data.udid = Some(val_str.clone());
                }
            }
            "Product Type" => data.product_type = Some(val_str.clone()),
            "Product Version" => data.product_version = Some(val_str.clone()),
            "Build Version" => data.build_version = Some(val_str.clone()),
            "Device Name" => data.device_name = Some(val_str.clone()),
            "Display Name" => data.display_name = Some(val_str.clone()),
            "GUID" => data.guid = Some(val_str.clone()),
            _ => {}
        }
        data.raw.insert(k.clone(), val_str);
    }
    data
}

/// 解析 Status.plist 字典。
fn parse_status_plist_dict(d: &plist::Dictionary) -> StatusPlistData {
    let mut data = StatusPlistData::default();
    for (k, v) in d {
        let val_str = if let Some(s) = v.as_string() {
            s.to_string()
        } else if let Some(b) = v.as_boolean() {
            b.to_string()
        } else {
            continue;
        };
        match k.as_str() {
            "BackupState" => data.backup_state = Some(val_str.clone()),
            "SnapshotState" => data.snapshot_state = Some(val_str.clone()),
            "IsFullBackup" => data.is_full = v.as_boolean(),
            "Version" => data.version = Some(val_str.clone()),
            "UUID" => data.uuid = Some(val_str.clone()),
            _ => {}
        }
        data.raw.insert(k.clone(), val_str);
    }
    data
}

/// 解析 Manifest.plist / Info.plist / Status.plist / iDevice_info.txt，产出记录。返回 (BackupInfo 摘要)。
pub fn analyze(
    run: &Run,
    files: &[(String, FileRef)],
) -> lovelycarve_plugin::Result<Option<BackupInfo>> {
    let manifest = files
        .iter()
        .find(|(p, _)| {
            p.rsplit('/')
                .next()
                .map(|n| n.eq_ignore_ascii_case("Manifest.plist"))
                .unwrap_or(false)
        })
        .or_else(|| {
            files.iter().find(|(p, f)| {
                p.ends_with("Manifest.plist") || f.name().eq_ignore_ascii_case("Manifest.plist")
            })
        });
    let manifest = match manifest {
        Some((p, f)) => (p.clone(), f),
        None => return Ok(None),
    };
    let bytes = manifest.1.read_all(64 << 20)?;
    let Some(plist) = crate::util::read_plist(&bytes) else {
        run.warn_file(
            manifest.1,
            "PLIST_PARSE",
            "Manifest.plist 解析失败".to_string(),
        )?;
        return Ok(None);
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

    // ---- 1. 读取 iDevice_info.txt（若存在） ----
    let idevice_file = files.iter().find(|(p, f)| {
        p.ends_with("iDevice_info.txt") || f.name().eq_ignore_ascii_case("iDevice_info.txt")
    });
    let (idevice, idevice_ref) = if let Some((p, f)) = idevice_file {
        if let Ok(b) = f.read_all(16 << 20) {
            (
                parse_idevice_info_text(&String::from_utf8_lossy(&b)),
                Some((p.clone(), f.clone())),
            )
        } else {
            (IDeviceInfo::default(), None)
        }
    } else if let Some(view) = run.view() {
        let try_paths = ["iDevice_info.txt", "root/iDevice_info.txt"];
        let mut found = None;
        for tp in try_paths {
            if let Ok(Some(text)) = view.read_text(tp, 16 << 20) {
                found = Some(parse_idevice_info_text(&text));
                break;
            }
        }
        (found.unwrap_or_default(), None)
    } else {
        (IDeviceInfo::default(), None)
    };

    // ---- 2. 读取 Info.plist（若存在） ----
    let infoplist_file = files
        .iter()
        .find(|(p, f)| p.ends_with("Info.plist") || f.name().eq_ignore_ascii_case("Info.plist"));
    let (infoplist, infoplist_ref) = if let Some((p, f)) = infoplist_file {
        if let Ok(b) = f.read_all(16 << 20) {
            if let Some(PValue::Dictionary(d2)) = crate::util::read_plist(&b) {
                (parse_info_plist_dict(&d2), Some((p.clone(), f.clone())))
            } else {
                (InfoPlistData::default(), None)
            }
        } else {
            (InfoPlistData::default(), None)
        }
    } else {
        (InfoPlistData::default(), None)
    };

    // ---- 3. 读取 Status.plist（若存在） ----
    let (statusplist, statusplist_ref) = if let Some((p, f)) = status_file(files) {
        if let Ok(b) = f.read_all(16 << 20) {
            if let Some(PValue::Dictionary(d2)) = crate::util::read_plist(&b) {
                (parse_status_plist_dict(&d2), Some((p.clone(), f.clone())))
            } else {
                (StatusPlistData::default(), None)
            }
        } else {
            (StatusPlistData::default(), None)
        }
    } else {
        (StatusPlistData::default(), None)
    };

    // ---- 4. 多源核验与权威认定 ----
    let serial = idevice
        .serial_number
        .clone()
        .or_else(|| pstr(&lockdown, "SerialNumber"))
        .or_else(|| infoplist.serial_number.clone())
        .unwrap_or_default();

    let udid = idevice
        .udid
        .clone()
        .or_else(|| pstr(&lockdown, "UniqueDeviceID"))
        .or_else(|| infoplist.udid.clone())
        .unwrap_or_default();

    let product_type = pstr(&lockdown, "ProductType")
        .or_else(|| idevice.product_type.clone())
        .or_else(|| infoplist.product_type.clone())
        .unwrap_or_default();

    let product_version = pstr(&lockdown, "ProductVersion")
        .or_else(|| idevice.product_version.clone())
        .or_else(|| infoplist.product_version.clone())
        .unwrap_or_default();

    let build_version = pstr(&lockdown, "BuildVersion")
        .or_else(|| idevice.build_version.clone())
        .or_else(|| infoplist.build_version.clone())
        .or_else(|| pstr(d, "BuildVersion"))
        .unwrap_or_default();

    let device_name = idevice
        .device_name
        .clone()
        .or_else(|| pstr(&lockdown, "DeviceName"))
        .or_else(|| infoplist.device_name.clone())
        .unwrap_or_default();

    let display_name = pstr(&lockdown, "DisplayName")
        .or_else(|| infoplist.display_name.clone())
        .unwrap_or_default();

    let imei = match (&idevice.imei1, &idevice.imei2, &infoplist.imei) {
        (Some(i1), Some(i2), _) if !i1.is_empty() && !i2.is_empty() => {
            format!("{i1} / {i2} (双卡/eSIM)")
        }
        (Some(i1), _, _) if !i1.is_empty() => i1.clone(),
        (_, _, Some(info_i)) if !info_i.is_empty() => info_i.clone(),
        _ => String::new(),
    };

    let iccid = idevice
        .iccid
        .clone()
        .or_else(|| infoplist.iccid.clone())
        .unwrap_or_default();

    let phone = infoplist
        .phone
        .clone()
        .or_else(|| idevice.carrier.clone())
        .unwrap_or_default();

    let base_model = iphone_model(&product_type).unwrap_or("iPhone");
    let model = if let Some(mn) = &idevice.model_number {
        format!("{base_model} ({mn})")
    } else {
        base_model.to_string()
    };

    let manifest_prefix = manifest
        .0
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/"))
        .unwrap_or_default();
    let manifest_key = d
        .get("ManifestKey")
        .and_then(PValue::as_data)
        .map(<[u8]>::to_vec);

    let mut info = BackupInfo {
        device_name: device_name.clone(),
        display_name: display_name.clone(),
        product_name: pstr(&lockdown, "ProductName").unwrap_or_else(|| "iPhone OS".into()),
        product_type: product_type.clone(),
        product_version: product_version.clone(),
        build_version: build_version.clone(),
        serial: serial.clone(),
        udid: udid.clone(),
        imei: imei.clone(),
        iccid: iccid.clone(),
        phone: phone.clone(),
        backup_unix,
        backup_raw: backup_raw.clone(),
        version: pstr(d, "Version").unwrap_or_default(),
        system_domains: pstr(d, "SystemDomainsVersion").unwrap_or_default(),
        encrypt: d
            .get("IsEncrypted")
            .and_then(PValue::as_boolean)
            .unwrap_or(false),
        passcode_set: d
            .get("WasPasscodeSet")
            .and_then(PValue::as_boolean)
            .unwrap_or(false),
        app_count: apps.map(|a| a.len() as i64).unwrap_or(0),
        lockdown_keys: lockdown
            .iter()
            .filter(|(_, v)| v.as_string().is_some())
            .map(|(k, v)| (k.to_string(), v.as_string().unwrap_or("").to_string()))
            .collect(),
        keybag: d
            .get("BackupKeyBag")
            .and_then(PValue::as_data)
            .and_then(parse_keybag),
        resolved_password: None,
        password_source: None,
        class_keys: Vec::new(),
        manifest_key,
        manifest_prefix,
    };

    // ---- 5. 产出 ios.backup（备份信息完整卡片） ----
    let mut v: Vec<Value> = vec![
        info.device_name.clone().into(),
        info.display_name.clone().into(),
        info.product_name.clone().into(),
        info.product_type.clone().into(),
        model.clone().into(),
        info.product_version.clone().into(),
        info.build_version.clone().into(),
        info.serial.clone().into(),
        info.udid.clone().into(),
        info.imei.clone().into(),
        info.iccid.clone().into(),
        info.phone.clone().into(),
    ];
    v.push(match info.backup_unix {
        Some(u) => Value::from(
            ts_plus8(u)
                .map(|t: Timestamp| t.as_str().to_string())
                .unwrap_or_default(),
        ),
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
        .note("Manifest.plist + iDevice_info + Info.plist")
        .emit()?;

    // ---- 6. 产出去重对齐、权威认定的 ios.device_info ----
    // Part A: 核心权威认定与多源交叉核验（置顶）
    let primary_imei = idevice.imei1.clone().or_else(|| infoplist.imei.clone());
    if let Some(pi) = primary_imei {
        let source_note = if idevice.imei1.is_some() && infoplist.imei.is_some() {
            "多源一致权威认定 (iDevice_info.txt 在线采集 + Info.plist 备份摘要)"
        } else if idevice.imei1.is_some() {
            "iDevice_info.txt (设备在线硬件采集)"
        } else {
            "Info.plist (备份设备信息摘要)"
        };
        run.record_values(
            "ios.device_info",
            vec![
                "01_IMEI_1".into(),
                "【权威认定】主卡槽 IMEI (IMEI 1)".into(),
                pi.into(),
                source_note.into(),
            ],
        )
        .emit()?;
    }

    if let Some(i2) = &idevice.imei2 {
        run.record_values(
            "ios.device_info",
            vec![
                "02_IMEI_2".into(),
                "【硬件独有】副卡槽 / eSIM IMEI (IMEI 2)".into(),
                i2.clone().into(),
                "iDevice_info.txt (基带在线提取，备份通常缺失副卡)".into(),
            ],
        )
        .emit()?;
    }

    let resolved_meid = idevice.meid.clone().or_else(|| infoplist.meid.clone());
    if let Some(m) = resolved_meid {
        let note = if idevice.meid.is_some() && infoplist.meid.is_some() {
            "多源一致权威认定 (iDevice_info.txt + Info.plist)"
        } else {
            "iDevice_info.txt / Info.plist"
        };
        run.record_values(
            "ios.device_info",
            vec![
                "03_MEID".into(),
                "【权威认定】移动设备识别码 (MEID)".into(),
                m.into(),
                note.into(),
            ],
        )
        .emit()?;
    }

    let resolved_iccid = idevice.iccid.clone().or_else(|| infoplist.iccid.clone());
    if let Some(ic) = resolved_iccid {
        let note = if idevice.iccid.is_some() && infoplist.iccid.is_some() {
            "多源一致权威认定 (iDevice_info.txt + Info.plist)"
        } else {
            "iDevice_info.txt / Info.plist"
        };
        run.record_values(
            "ios.device_info",
            vec![
                "04_ICCID".into(),
                "【权威认定】SIM 卡集成电路卡标识 (ICCID)".into(),
                ic.into(),
                note.into(),
            ],
        )
        .emit()?;
    }

    if let Some(im) = &idevice.imsi {
        run.record_values(
            "ios.device_info",
            vec![
                "05_IMSI".into(),
                "【SIM卡标识】国际移动用户识别码 (IMSI)".into(),
                im.clone().into(),
                "iDevice_info.txt (SIM卡签约数据)".into(),
            ],
        )
        .emit()?;
    }

    if let Some(cb) = &idevice.carrier {
        run.record_values(
            "ios.device_info",
            vec![
                "06_Carrier".into(),
                "【运营商网络】蜂窝网络运营商配置".into(),
                cb.clone().into(),
                "iDevice_info.txt (CarrierBundle)".into(),
            ],
        )
        .emit()?;
    }

    if !serial.is_empty() {
        let note = if idevice.serial_number.is_some() && pstr(&lockdown, "SerialNumber").is_some() {
            "三方一致最高权威 (iDevice_info + Manifest + Info.plist)"
        } else {
            "Manifest.plist / Info.plist"
        };
        run.record_values(
            "ios.device_info",
            vec![
                "07_SerialNumber".into(),
                "【权威认定】设备硬件序列号 (SerialNumber)".into(),
                serial.into(),
                note.into(),
            ],
        )
        .emit()?;
    }

    if !udid.is_empty() {
        run.record_values(
            "ios.device_info",
            vec![
                "08_UniqueDeviceID".into(),
                "【权威认定】设备唯一识别码 (UDID)".into(),
                udid.into(),
                "三方一致 (iDevice_info + Manifest + Info.plist)".into(),
            ],
        )
        .emit()?;
    }

    let model_desc = if let Some(mn) = &idevice.model_number {
        format!("{base_model} ({product_type}，型号代码 {mn})")
    } else {
        format!("{base_model} ({product_type})")
    };
    run.record_values(
        "ios.device_info",
        vec![
            "09_ProductModel".into(),
            "【设备型号】硬件型号与营销名称".into(),
            model_desc.into(),
            "综合核验 (iDevice_info + Manifest.plist)".into(),
        ],
    )
    .emit()?;

    if let Some(reg) = &idevice.region_info {
        let reg_desc = if reg == "ZA/A" || reg.starts_with("ZA") {
            format!("{reg} (港版/澳门版/特定地区)")
        } else if reg == "CH/A" || reg.starts_with("CH") {
            format!("{reg} (国行版)")
        } else if reg == "LL/A" || reg.starts_with("LL") {
            format!("{reg} (美版)")
        } else {
            reg.clone()
        };
        run.record_values(
            "ios.device_info",
            vec![
                "10_RegionInfo".into(),
                "【销售区域】设备发行地区代码 (RegionInfo)".into(),
                reg_desc.into(),
                "iDevice_info.txt 硬件采集".into(),
            ],
        )
        .emit()?;
    }

    if !product_version.is_empty() {
        run.record_values(
            "ios.device_info",
            vec![
                "11_ProductVersion".into(),
                "【操作系统】系统版本与构建号".into(),
                format!("iOS {product_version} (Build {build_version})").into(),
                "三方一致 (iDevice_info + Manifest + Info.plist)".into(),
            ],
        )
        .emit()?;
    }

    if let Some(mac) = &idevice.wifi_mac {
        run.record_values(
            "ios.device_info",
            vec![
                "12_WiFiAddress".into(),
                "【网络硬件】Wi-Fi 网卡物理地址 (MAC)".into(),
                mac.clone().into(),
                "iDevice_info.txt 在线硬件提取".into(),
            ],
        )
        .emit()?;
    }

    if let Some(bmac) = &idevice.bluetooth_mac {
        run.record_values(
            "ios.device_info",
            vec![
                "13_BluetoothAddress".into(),
                "【网络硬件】蓝牙芯片物理地址 (MAC)".into(),
                bmac.clone().into(),
                "iDevice_info.txt 在线硬件提取".into(),
            ],
        )
        .emit()?;
    }

    if let Some(emac) = &idevice.ethernet_mac {
        run.record_values(
            "14_EthernetAddress".into(),
            vec![
                "14_EthernetAddress".into(),
                "【网络硬件】以太网物理地址 (MAC)".into(),
                emac.clone().into(),
                "iDevice_info.txt 在线硬件提取".into(),
            ],
        )
        .emit()?;
    }

    // Part B: 备份安全与状态控制
    run.record_values(
        "ios.device_info",
        vec![
            "20_Backup_IsEncrypted".into(),
            "【备份安全】备份加密状态 (IsEncrypted)".into(),
            if info.encrypt {
                "已加密（包含 BackupKeyBag 密钥包）"
            } else {
                "未加密"
            }
            .into(),
            "Manifest.plist".into(),
        ],
    )
    .from(manifest.1)
    .emit()?;

    run.record_values(
        "ios.device_info",
        vec![
            "21_Backup_WasPasscodeSet".into(),
            "【备份安全】设备锁屏密码状态 (WasPasscodeSet)".into(),
            if info.passcode_set {
                "是（设备已设置锁屏密码）"
            } else {
                "否"
            }
            .into(),
            "Manifest.plist".into(),
        ],
    )
    .from(manifest.1)
    .emit()?;

    let backup_time_str = match info.backup_unix {
        Some(u) => ts_plus8(u)
            .map(|t: Timestamp| t.as_str().to_string())
            .unwrap_or(info.backup_raw.clone()),
        None => info.backup_raw.clone(),
    };
    run.record_values(
        "ios.device_info",
        vec![
            "22_Backup_Time".into(),
            "【备份时间】备份生成时间 (BackupTime UTC+8)".into(),
            backup_time_str.into(),
            "Manifest.plist (Date)".into(),
        ],
    )
    .from(manifest.1)
    .emit()?;

    if statusplist.backup_state.is_some() || statusplist.snapshot_state.is_some() {
        let bstate = statusplist.backup_state.as_deref().unwrap_or("-");
        let sstate = statusplist.snapshot_state.as_deref().unwrap_or("-");
        run.record_values(
            "ios.device_info",
            vec![
                "23_Backup_State".into(),
                "【备份状态】备份任务与快照状态".into(),
                format!("BackupState: {bstate}, SnapshotState: {sstate}").into(),
                "Status.plist".into(),
            ],
        )
        .emit()?;
    }

    if let Some(full) = statusplist.is_full {
        run.record_values(
            "ios.device_info",
            vec![
                "24_Backup_IsFull".into(),
                "【备份类型】完整备份标记 (IsFullBackup)".into(),
                if full {
                    "是 (完整备份)"
                } else {
                    "否 (增量/快速备份)"
                }
                .into(),
                "Status.plist".into(),
            ],
        )
        .emit()?;
    }

    // Part C: 原始补充明细（已过滤已在上方权威认定的重复字段）
    let suppressed_keys: HashSet<&str> = [
        "SerialNumber",
        "Serial Number",
        "UniqueDeviceID",
        "Unique Identifier",
        "Target Identifier",
        "ProductType",
        "Product Type",
        "ProductVersion",
        "Product Version",
        "BuildVersion",
        "Build Version",
        "DeviceName",
        "Device Name",
        "DisplayName",
        "Display Name",
        "InternationalMobileEquipmentIdentity",
        "InternationalMobileEquipmentIdentity2",
        "IMEI",
        "MobileEquipmentIdentifier",
        "MEID",
        "IntegratedCircuitCardIdentity",
        "ICCID",
        "InternationalMobileSubscriberIdentity",
        "ModelNumber",
        "RegionInfo",
        "WiFiAddress",
        "BluetoothAddress",
        "EthernetAddress",
        "IsEncrypted",
        "WasPasscodeSet",
        "Date",
        "BackupState",
        "SnapshotState",
        "IsFullBackup",
    ]
    .into_iter()
    .collect();

    // 来自 iDevice_info.txt 的其他底层硬件属性
    for (k, val) in &idevice.raw {
        if suppressed_keys.contains(k.as_str()) {
            continue;
        }
        let mut r = run.record_values(
            "ios.device_info",
            vec![
                format!("iDevice_info/{k}").into(),
                human_idevice_key(k).into(),
                val.clone().into(),
                "iDevice_info.txt (硬件采集)".into(),
            ],
        );
        if let Some((_, f)) = &idevice_ref {
            r = r.from(f);
        }
        r.emit()?;
    }

    // 来自 Manifest.plist/Lockdown 的其他属性
    for (k, val) in &info.lockdown_keys {
        if suppressed_keys.contains(k.as_str()) {
            continue;
        }
        run.record_values(
            "ios.device_info",
            vec![
                format!("Lockdown/{k}").into(),
                human_key(k).into(),
                val.clone().into(),
                "Manifest.plist/Lockdown".into(),
            ],
        )
        .from(manifest.1)
        .emit()?;
    }

    // 来自 Info.plist 的其他属性
    for (k, val) in &infoplist.raw {
        if suppressed_keys.contains(k.as_str()) {
            continue;
        }
        let mut r = run.record_values(
            "ios.device_info",
            vec![
                format!("Info.plist/{k}").into(),
                "备份设备配置摘要".into(),
                val.clone().into(),
                "Info.plist".into(),
            ],
        );
        if let Some((_, f)) = &infoplist_ref {
            r = r.from(f);
        }
        r.emit()?;
    }

    // 来自 Status.plist 的其他属性
    for (k, val) in &statusplist.raw {
        if suppressed_keys.contains(k.as_str()) {
            continue;
        }
        let mut r = run.record_values(
            "ios.device_info",
            vec![
                format!("Status.plist/{k}").into(),
                "备份状态属性".into(),
                val.clone().into(),
                "Status.plist".into(),
            ],
        );
        if let Some((_, f)) = &statusplist_ref {
            r = r.from(f);
        }
        r.emit()?;
    }

    // ---- 口令验证或自动爆破 ----
    let pwd_opt = run
        .options()
        .text("backup_password")
        .filter(|p| !p.is_empty());
    let auto_crack = run.options().flag("auto_crack_password");

    if let Some(pwd) = pwd_opt {
        if let Some(blob) = d.get("BackupKeyBag").and_then(PValue::as_data) {
            if let Some(kb) = crate::keybag::parse(blob) {
                let outcome = crate::keybag::unlock_with_password(&kb, pwd);
                if outcome.ok {
                    info.resolved_password = Some(pwd.to_string());
                    info.password_source = Some("手动输入");
                    info.class_keys = outcome.class_keys.clone();
                }
                run.record_values(
                    "ios.keybag",
                    vec![
                        "口令验证".into(),
                        if outcome.ok {
                            "正确".into()
                        } else {
                            "错误".into()
                        },
                        Value::from(format!(
                            "密码{}；派生 {}；解开类密钥 {}/{} 个",
                            if outcome.ok { "正确" } else { "错误" },
                            if outcome.ok { outcome.method } else { "PBKDF2" },
                            outcome.class_keys.len(),
                            kb.wrapped.len()
                        )),
                    ],
                )
                .from(manifest.1)
                .emit()?;
            }
        }
    } else if auto_crack && info.encrypt {
        if let Some(blob) = d.get("BackupKeyBag").and_then(PValue::as_data) {
            if let Some(kb) = crate::keybag::parse(blob) {
                run.progress(
                    1,
                    Some(8),
                    Some("正在自动爆破 Manifest.plist 备份密码（4/6位纯数字）…"),
                )?;
                let cracked = crate::keybag::crack_numeric_pin(&kb, Some(&info.serial), &|| {
                    run.check_cancelled().is_err()
                });
                if let Some(pw) = cracked.password {
                    let display_pw = if pw.is_empty() {
                        "<空密码>".to_string()
                    } else {
                        pw.clone()
                    };
                    info.resolved_password = Some(pw);
                    info.password_source = Some("自动爆破");
                    info.class_keys = cracked.class_keys.clone();
                    run.record_values(
                        "ios.keybag",
                        vec![
                            "自动爆破结果".into(),
                            display_pw.clone().into(),
                            Value::from(format!(
                                "成功命中数字备份密码「{display_pw}」；派生 {}；解开类密钥 {}/{} 个；共测试 {} 次（耗时 {}ms）",
                                cracked.method,
                                cracked.class_keys.len(),
                                kb.wrapped.len(),
                                cracked.tested_count,
                                cracked.elapsed_ms
                            )),
                        ],
                    )
                    .from(manifest.1)
                    .emit()?;
                } else {
                    let msg = if cracked.cancelled {
                        format!(
                            "自动爆破已取消（已测试 {} 次，耗时 {}ms）",
                            cracked.tested_count, cracked.elapsed_ms
                        )
                    } else {
                        format!(
                            "未命中 4/6 位纯数字备份密码（共测试 {} 次，耗时 {}ms）",
                            cracked.tested_count, cracked.elapsed_ms
                        )
                    };
                    run.record_values(
                        "ios.keybag",
                        vec!["自动爆破结果".into(), "未命中".into(), msg.into()],
                    )
                    .from(manifest.1)
                    .emit()?;
                }
            }
        }
    }
    if let Some(kb) = &info.keybag {
        for (k, val, note) in std::iter::once((
            "ClassKeys".to_string(),
            format!(
                "{} 个（其中口令包裹 KTYP=0 共 {} 个）",
                kb.class_count, kb.ktyp0
            ),
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

    Ok(Some(info))
}

fn status_file<'a>(files: &'a [(String, FileRef)]) -> Option<(&'a String, &'a FileRef)> {
    files
        .iter()
        .find(|(p, f)| p.ends_with("Status.plist") || f.name().eq_ignore_ascii_case("Status.plist"))
        .map(|(p, f)| (p, f))
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
        _ => "Lockdown扩展属性",
    }
}

fn human_idevice_key(k: &str) -> &'static str {
    match k {
        "ActivationState" => "激活状态",
        "BasebandVersion" => "基带固件版本",
        "BasebandChipID" => "基带芯片ID",
        "BasebandSerialNumber" => "基带芯片序列号",
        "BasebandStatus" => "基带运行状态",
        "BoardId" => "主板ID",
        "BootSessionID" => "引导会话UUID",
        "CPUArchitecture" => "CPU架构",
        "ChipID" => "应用处理器ChipID",
        "ChipSerialNo" => "芯片序列号",
        "DeviceClass" => "设备类别",
        "DeviceColor" => "设备机身颜色",
        "DieID" => "晶圆DieID",
        "FirmwareVersion" => "iBoot引导固件版本",
        "HardwareModel" => "硬件主板型号代号",
        "HardwarePlatform" => "硬件平台架构",
        "MLBSerialNumber" => "主逻辑板(MLB)序列号",
        "TimeZone" => "系统时区设置",
        "UniqueChipID" => "唯一芯片识别码(ECID)",
        _ => "底层硬件参数",
    }
}

/// Manifest Applications + Info.plist iTunesMetadata 合并的应用清单。
/// 返回 (bundle, 桌面名, 版本, 构建号, App Store 存储名(itemName), placeholder)。
pub struct AppEntry {
    pub bundle: String,
    pub name: String,
    pub short: String,
    pub build: String,
    pub item: String,
    pub placeholder: bool,
}

pub fn apps_from_manifest(
    run: &Run,
    files: &[(String, FileRef)],
) -> lovelycarve_plugin::Result<Vec<AppEntry>> {
    let mut out: Vec<AppEntry> = Vec::new();
    let Some((mp, mf)) = files
        .iter()
        .find(|(p, _)| p.ends_with("Manifest.plist"))
        .map(|(p, f)| (p.clone(), f))
    else {
        return Ok(out);
    };
    let Ok(bytes) = mf.read_all(64 << 20) else {
        return Ok(out);
    };
    let Some(PValue::Dictionary(d)) = crate::util::read_plist(&bytes) else {
        return Ok(out);
    };
    let Some(apps) = d.get("Applications").and_then(PValue::as_dictionary) else {
        return Ok(out);
    };
    for (bundle, v) in apps {
        let a = v.as_dictionary();
        let mut name = a
            .and_then(|a| {
                a.get("CFBundleDisplayName")
                    .or_else(|| a.get("CFBundleName"))
            })
            .and_then(PValue::as_string)
            .unwrap_or("")
            .to_string();
        if name.trim().is_empty() {
            if let Some(path_str) = a.and_then(|a| a.get("Path")).and_then(PValue::as_string) {
                if let Some(app_dir) = path_str.split('/').find(|seg| seg.ends_with(".app")) {
                    name = app_dir.trim_end_matches(".app").to_string();
                }
            }
        }
        let short = a
            .and_then(|a| a.get("CFBundleShortVersionString"))
            .and_then(PValue::as_string)
            .unwrap_or("")
            .to_string();
        let build = a
            .and_then(|a| a.get("CFBundleVersion"))
            .and_then(PValue::as_string)
            .unwrap_or("")
            .to_string();
        let placeholder = a
            .and_then(|a| a.get("Placeholder"))
            .and_then(PValue::as_boolean)
            .unwrap_or(false);
        out.push(AppEntry {
            bundle: bundle.clone(),
            name,
            short,
            build,
            item: String::new(),
            placeholder,
        });
    }
    // Info.plist 的 Applications 字典带 iTunesMetadata（内嵌二进制 plist），
    // 版本字段比 Manifest 更接近 App Store 展示值。
    for (_p, f) in files.iter().filter(|(p, _)| p.ends_with("Info.plist")) {
        let Ok(b) = f.read_all(32 << 20) else {
            continue;
        };
        let Some(PValue::Dictionary(d)) = crate::util::read_plist(&b) else {
            continue;
        };
        let Some(apps) = d.get("Applications").and_then(PValue::as_dictionary) else {
            continue;
        };
        for (bundle, v) in apps {
            let Some(meta) = v
                .as_dictionary()
                .and_then(|a| a.get("iTunesMetadata"))
                .and_then(PValue::as_data)
            else {
                continue;
            };
            let Some(PValue::Dictionary(m)) = crate::util::read_plist(meta) else {
                continue;
            };
            let short = m
                .get("bundleShortVersionString")
                .and_then(PValue::as_string)
                .unwrap_or("")
                .to_string();
            let build = m
                .get("bundleVersion")
                .and_then(PValue::as_string)
                .unwrap_or("")
                .to_string();
            let item = m
                .get("itemName")
                .and_then(PValue::as_string)
                .unwrap_or("")
                .to_string();
            if short.is_empty() && build.is_empty() && item.is_empty() {
                continue;
            }
            if let Some(row) = out.iter_mut().find(|r| r.bundle == *bundle) {
                if !short.is_empty() {
                    row.short = short;
                }
                if !build.is_empty() {
                    row.build = build;
                }
                if !item.is_empty() {
                    row.item = item;
                }
            } else {
                out.push(AppEntry {
                    bundle: bundle.clone(),
                    name: String::new(),
                    short,
                    build,
                    item,
                    placeholder: false,
                });
            }
        }
    }
    let _ = (mp, run);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_idevice_info_text() {
        let sample = r#"
ActivationState: Activated
IntegratedCircuitCardIdentity: 89852122206020989954
InternationalMobileEquipmentIdentity: 351845975701643
InternationalMobileEquipmentIdentity2: 351845975521967
InternationalMobileSubscriberIdentity: 454120645098995
MobileEquipmentIdentifier: 35184597570164
SerialNumber: DX3HK0HLN741
UniqueDeviceID: 00008030-0019513134F8C02E
ProductType: iPhone12,1
ProductVersion: 17.5.1
BuildVersion: 21F90
ModelNumber: MHEV3
RegionInfo: ZA/A
WiFiAddress: e8:1c:d8:51:47:49
BluetoothAddress: e8:1c:d8:53:d1:b7
CarrierBundleInfoArray: 
0: 
CFBundleIdentifier: com.apple.CMCC_hk
MCC: 454
MNC: 12
CertID: 524245983
keyChain_Begin
Some: Trash
"#;
        let info = parse_idevice_info_text(sample);
        assert_eq!(info.imei1.as_deref(), Some("351845975701643"));
        assert_eq!(info.imei2.as_deref(), Some("351845975521967"));
        assert_eq!(info.meid.as_deref(), Some("35184597570164"));
        assert_eq!(info.iccid.as_deref(), Some("89852122206020989954"));
        assert_eq!(info.imsi.as_deref(), Some("454120645098995"));
        assert_eq!(info.serial_number.as_deref(), Some("DX3HK0HLN741"));
        assert_eq!(info.udid.as_deref(), Some("00008030-0019513134F8C02E"));
        assert_eq!(info.model_number.as_deref(), Some("MHEV3"));
        assert_eq!(info.region_info.as_deref(), Some("ZA/A"));
        assert_eq!(info.wifi_mac.as_deref(), Some("e8:1c:d8:51:47:49"));
        assert_eq!(info.bluetooth_mac.as_deref(), Some("e8:1c:d8:53:d1:b7"));
        assert!(info
            .carrier
            .as_deref()
            .unwrap_or("")
            .contains("中国移动香港"));
        assert!(!info.raw.contains_key("Some"));
    }
}
