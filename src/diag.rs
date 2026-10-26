//! sysdiagnose 目录路由：蓝牙状态、BDC 电池（激活时间/健康/序列号）、
//! 系统版本、MobileBackup 预检体积。

use crate::util::fmt_bytes;
use lovelycarve_plugin::{FileRef, Run};
use plist::Value as PValue;

pub struct DiagResult {
    pub bt_mac: Option<String>,
    pub battery_activation: Option<String>, // "YYYY-MM-DD HH:MM:SS" 本地时间原文
    pub battery_serial: Option<String>,
    pub battery_health: Option<i64>,
    pub domain_sizes: Vec<(String, u64)>, // (bundle 或域名, bytes)
    pub anchor_unix: Option<i64>,          // 采集时间锚点（目录名/文件时间）
}

/// bluetooth_status.txt：`    MAC Address          : C0:A5:3E:82:06:E0`
pub fn bluetooth(run: &Run, file: &FileRef, res: &mut DiagResult) -> lovelycarve_plugin::Result<()> {
    let text = file.read_text(4 << 20)?;
    let mut n = 0usize;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() || v.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("MAC Address") {
            res.bt_mac = Some(v.to_string());
        }
        run.record_values(
            "ios.bluetooth_state",
            vec![k.to_string().into(), v.to_string().into(), "bluetooth_status.txt".into()],
        )
        .from(file)
        .emit()?;
        n += 1;
    }
    if n == 0 {
        run.warn_file(file, "BT_PARSE", "bluetooth_status.txt 未解析出键值")?;
    }
    Ok(())
}

/// BDC_Once_version1.6_2022-01-23_15_02_27.csv：首行数据 TimeStamp = 电池激活时间。
pub fn bdc_once(run: &Run, file: &FileRef, res: &mut DiagResult) -> lovelycarve_plugin::Result<()> {
    let text = file.read_text(4 << 20)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').map(|s| s.trim()).collect();
    let data: Vec<&str> = lines.next().unwrap_or("").split(',').map(|s| s.trim()).collect();
    for (i, key) in header.iter().enumerate() {
        let val = data.get(i).copied().unwrap_or("");
        let note = if *key == "TimeStamp" {
            res.battery_activation = Some(val.to_string());
            "电量计首次初始化时间（本地时间）＝电池激活时间"
        } else {
            ""
        };
        run.record_values(
            "ios.battery",
            vec![format!("BDC_Once.{key}").into(), val.into(), note.into()],
        )
        .from(file)
        .emit()?;
    }
    if data.is_empty() {
        run.warn_file(file, "BDC_ONCE_EMPTY", "BDC_Once CSV 无数据行")?;
    }
    Ok(())
}

/// logs/BatteryHealth/BatteryHealth.log：Maximum Capacity Percent。
pub fn battery_health(run: &Run, file: &FileRef, res: &mut DiagResult) -> lovelycarve_plugin::Result<()> {
    let text = file.read_text(1 << 20)?;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim().trim_matches('"');
        let v = v.trim().trim_matches(';');
        if k.is_empty() {
            continue;
        }
        if k == "Maximum Capacity Percent" {
            res.battery_health = v.parse().ok();
        }
        run.record_values("ios.battery", vec![format!("Health.{k}").into(), v.into(), "BatteryHealth.log".into()])
            .from(file)
            .emit()?;
    }
    Ok(())
}

/// com.apple.powerd.bdc.plist：电池序列号 + BDC_Once 最后记录时间。
pub fn bdc_plist(run: &Run, file: &FileRef, res: &mut DiagResult) -> lovelycarve_plugin::Result<()> {
    let bytes = file.read_all(8 << 20)?;
    let Ok(v) = PValue::from_reader(std::io::Cursor::new(&bytes)) else {
        run.warn_file(file, "PLIST_PARSE", "com.apple.powerd.bdc.plist 解析失败")?;
        return Ok(());
    };
    let Some(d) = v.as_dictionary() else { return Ok(()) };
    for (k, val) in d {
        let s = match val {
            PValue::String(s) => Some(s.clone()),
            PValue::Date(d) => Some(d.to_xml_format()),
            PValue::Integer(i) => Some(i.to_string()),
            PValue::Real(f) => Some(f.to_string()),
            PValue::Boolean(b) => Some(b.to_string()),
            _ => None,
        };
        let Some(s) = s else { continue };
        let note = if k == "BatterySerialNumber" {
            res.battery_serial = Some(s.clone());
            "电池序列号"
        } else if k == "BDC_Once_LastLoggedTimeStamp" {
            "BDC_Once 最后记录（与激活时间佐证）"
        } else {
            ""
        };
        run.record_values("ios.battery", vec![format!("powerd.{k}").into(), s.into(), note.into()])
            .from(file)
            .emit()?;
    }
    Ok(())
}

/// SystemVersion.plist。
pub fn system_version(run: &Run, file: &FileRef) -> lovelycarve_plugin::Result<()> {
    let bytes = file.read_all(1 << 20)?;
    let Ok(v) = PValue::from_reader(std::io::Cursor::new(&bytes)) else { return Ok(()) };
    let Some(d) = v.as_dictionary() else { return Ok(()) };
    for (k, val) in d {
        if let Some(s) = val.as_string() {
            run.record_values(
                "ios.device_info",
                vec![format!("SystemVersion.{k}").into(), k.clone().into(), s.to_string().into(), "logs/SystemVersion".into()],
            )
            .from(file)
            .emit()?;
        }
    }
    Ok(())
}

/// com.apple.MobileBackup.plist 的 PreflightSizing：AppDomain-<bundle> → 字节。
pub fn mobile_backup_sizing(run: &Run, file: &FileRef, res: &mut DiagResult) -> lovelycarve_plugin::Result<()> {
    let bytes = file.read_all(32 << 20)?;
    let Ok(v) = PValue::from_reader(std::io::Cursor::new(&bytes)) else { return Ok(()) };
    let Some(d) = v.as_dictionary() else { return Ok(()) };
    let Some(sizing) = d.get("PreflightSizing").and_then(PValue::as_dictionary) else {
        run.warn_file(file, "PREFLIGHT_MISSING", "MobileBackup.plist 无 PreflightSizing")?;
        return Ok(());
    };
    for (domain, val) in sizing {
        let Some(n) = val.as_signed_integer() else { continue };
        let n = n.max(0) as u64;
        let note = if *domain == "_TotalSize" {
            "备份预检总体积"
        } else if domain.starts_with("AppDomain-") {
            "应用数据域体积"
        } else if domain.starts_with("AppDomainPlaceholder-") {
            "应用占位域体积（应用已卸载或未完整备份）"
        } else if domain.starts_with("AppDomainGroup-") {
            "应用组共享域体积"
        } else {
            "系统域体积"
        };
        res.domain_sizes.push((domain.clone(), n));
        run.record_values(
            "ios.device_info",
            vec![domain.clone().into(), note.into(), format!("{}（{n} 字节）", fmt_bytes(n)).into(), "PreflightSizing".into()],
        )
        .from(file)
        .emit()?;
    }
    Ok(())
}

/// 从 sysdiagnose 目录名（sysdiagnose_2026.09.23_14-23-51+0800_...）取采集时间锚点。
pub fn anchor_from_dirname(name: &str) -> Option<i64> {
    // sysdiagnose_2026.09.23_14-23-51+0800_iPhone-OS_...
    let start = name.find("sysdiagnose_")? + 12;
    let rest = &name[start..];
    let mut parts = rest.split('_');
    let date = parts.next()?;
    let time = parts.next()?;
    let off = parts.next().unwrap_or("+0800");
    let ymd: Vec<&str> = date.split('.').collect();
    if ymd.len() != 3 {
        return None;
    }
    let hms: Vec<&str> = time.split('-').collect();
    if hms.len() != 3 {
        return None;
    }
    let (y, mo, d, h, mi, sec): (i32, u32, u32, u32, u32, u32) = (
        ymd[0].parse().ok()?, ymd[1].parse().ok()?, ymd[2].parse().ok()?,
        hms[0].parse().ok()?, hms[1].parse().ok()?, hms[2].parse().ok()?,
    );
    let off_min = parse_offset(off)?;
    let naive = chrono::NaiveDateTime::new(
        chrono::NaiveDate::from_ymd_opt(y, mo, d)?,
        chrono::NaiveTime::from_hms_opt(h, mi, sec)?,
    );
    Some(naive.and_utc().timestamp() - off_min as i64 * 60)
}

fn parse_offset(s: &str) -> Option<i32> {
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let rest = &s[1..];
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    Some(sign * (h.parse::<i32>().ok()? * 60 + m.parse::<i32>().ok()?))
}
