//! LovelyCarve iOS 备份与 Sysdiagnose 取证插件。
//!
//! 两个分析器：
//! - `ios_backup`（evidence）：Manifest/Status/Info + KeyBag + sysdiagnose（蓝牙/电池/
//!   安装日志/系统版本/预检体积）+ powerlog（充满/解锁/时长/每日），并汇总 ios.finding。
//! - `powerlog_files`（files）：选中 .PLSQL 的深度归纳（复用同一 powerlog 引擎）。

pub mod airdrop;
pub mod appmap;
pub mod appprefs;
pub mod apps;
pub mod apps_route;
pub mod apps_route_glued;
pub mod backup;
pub mod backupdb;
pub mod coreduet;
pub mod datausage;
pub mod deviceid;
pub mod diag;
pub mod encrypted_backup;
pub mod filesapp;
pub mod icloud;
pub mod installlog;
pub mod keybag;
pub mod keychain;
pub mod kvjson;
pub mod media_exif;
pub mod photos;
pub mod photos_enums;
pub mod powerlog;
pub mod pushkey;
pub mod realm;
pub mod safari;
pub mod specs;
pub mod sqlite_wal;
pub mod unifiedlog;
pub mod util;
pub mod wa_proto;

use diag::DiagResult;
use lovelycarve_plugin::{export_plugin, FileRef, Plugin, Run, Value};
use util::{fmt_bytes, fmt_plus8};

pub struct Ctx {
    pub files: Vec<(String, FileRef)>,
    pub anchor: Option<i64>,
    pub findings: Vec<(String, String, String)>,
}

fn anchor_from_inputs(files: &[(String, FileRef)]) -> Option<i64> {
    for (p, _) in files {
        for seg in p.split('/') {
            if seg.starts_with("sysdiagnose_") {
                if let Some(a) = diag::anchor_from_dirname(seg) {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// 应用容器索引：bundle → 检材内数据目录（最多 3 个，"；"连接）。
/// 覆盖两种检材形态：文件级提取（var/mobile/Applications/<bundle>/）与
/// 标准 iTunes 备份（Containers/Data/Application/<UUID>/ + 容器元数据 plist）。
pub fn app_container_index(run: &Run) -> std::collections::HashMap<String, String> {
    let Some(view) = run.view() else {
        return Default::default();
    };
    let mut raw: std::collections::HashMap<String, Vec<String>> = Default::default();
    fn add(raw: &mut std::collections::HashMap<String, Vec<String>>, bundle: String, path: String) {
        if !bundle.contains('.') || bundle.starts_with('$') || bundle.ends_with(".plist") {
            return;
        }
        // 元数据里可能带团队 ID 前缀（TeamID.bundle.id）
        let bundle = {
            let segs: Vec<&str> = bundle.splitn(2, '.').collect();
            if segs.len() == 2
                && segs[0].len() == 10
                && segs[0]
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            {
                segs[1].to_string()
            } else {
                bundle
            }
        };
        let e = raw.entry(bundle).or_default();
        if !e.contains(&path) && e.len() < 4 {
            e.push(path);
        }
    }
    // 挂载根可能带前缀（如 Evidence 视图 "part:0/User/..." 或 "root/private/var/..."）：
    // 收集 1 级和 2 级目录作为候选前缀。
    let mut prefixes: Vec<String> = Vec::new();
    let top_dirs: Vec<String> = view
        .list_or_empty("")
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.is_dir())
        .map(|n| n.name)
        .collect();
    for top in top_dirs {
        for sub in view
            .list_or_empty(&top)
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.is_dir())
            .take(32)
        {
            prefixes.push(format!("{top}/{}", sub.name));
        }
        prefixes.push(top);
    }
    let bases = |suffix: &str| -> Vec<(String, String)> {
        let mut suffixes = vec![suffix];
        if let Some(rel) = suffix.strip_prefix("var/mobile/") {
            suffixes.push(rel);
        }
        let mut out = Vec::new();
        let mut cands: Vec<String> = Vec::new();
        for s in &suffixes {
            for t in &prefixes {
                cands.push(format!("{t}/{s}"));
            }
            cands.push((*s).to_string());
        }
        for p in cands {
            if !out.iter().any(|(full, _)| *full == p) && view.is_dir(&p).unwrap_or(false) {
                let disp = p.clone();
                out.push((p, disp));
            }
        }
        out
    };
    // ① 文件级提取 / 应用目录直挂：Applications/<bundle 或 group 容器>/
    for (full, disp) in bases("var/mobile/Applications") {
        for node in view.list_or_empty(&full).unwrap_or_default() {
            if !node.is_dir() {
                continue;
            }
            let name = node.name;
            let path = format!("{disp}/{name}");
            // 组容器形如 "243LU875E5.groups.com.apple.podcasts"（团队ID.groups.<组ID>），
            // 同时按 "group.<组ID>" 与 "<组ID>" 归档，便于映射到归属应用。
            if let Some(pos) = name.find(".groups.") {
                let group_id = name[pos + ".groups.".len()..].to_string();
                add(&mut raw, format!("group.{group_id}"), path.clone());
                add(&mut raw, group_id, path.clone());
            }
            add(&mut raw, name, path);
            if raw.len() > 2000 {
                break;
            }
        }
    }
    // ② 标准 iTunes 备份容器：<base>/<UUID>/.com.apple.mobile_container_manager.metadata.plist
    for suffix in [
        "var/mobile/Containers/Data/Application",
        "var/mobile/Containers/Shared/AppGroup",
    ] {
        for (full, disp) in bases(suffix) {
            for node in view.list_or_empty(&full).unwrap_or_default() {
                if !node.is_dir() {
                    continue;
                }
                run.check_cancelled().ok();
                let meta = format!(
                    "{full}/{}/.com.apple.mobile_container_manager.metadata.plist",
                    node.name
                );
                if let Ok(Some(bytes)) = view.read(&meta, 1 << 20) {
                    if let Some(v) = crate::util::read_plist(&bytes) {
                        if let Some(b) = plist_bundle(&v) {
                            add(&mut raw, b, format!("{disp}/{}", node.name));
                        }
                    }
                }
                if raw.len() > 4000 {
                    break;
                }
            }
        }
    }
    // ③ 系统自带组件的数据在 HomeDomain（var/mobile/Library、var/mobile/Media），
    //    不在应用容器里；按 iOS 备份的固定布局补全，仅当路径真实存在时登记。
    const SYSTEM_PATHS: &[(&str, &str)] = &[
        ("com.apple.mobilesafari", "var/mobile/Library/Safari"),
        ("com.apple.mobileslideshow", "var/mobile/Media/PhotoData"),
        ("com.apple.mobileslideshow", "var/mobile/Media/DCIM"),
        ("com.apple.camera", "var/mobile/Media/DCIM"),
        ("com.apple.MobileSMS", "var/mobile/Library/SMS"),
        ("com.apple.mobilephone", "var/mobile/Library/CallHistoryDB"),
        ("com.apple.Notes", "var/mobile/Library/Notes"),
        ("com.apple.mobilecal", "var/mobile/Library/Calendar"),
        ("com.apple.mobilemail", "var/mobile/Library/Mail"),
        ("com.apple.accountsd", "var/mobile/Library/Accounts"),
        ("com.apple.Preferences", "var/mobile/Library/Preferences"),
        (
            "com.apple.DocumentsApp",
            "var/mobile/Applications/com.apple.FileProvider.LocalStorage",
        ),
    ];
    for (bundle, path) in SYSTEM_PATHS {
        let mut candidates = vec![*path];
        if let Some(rel) = path.strip_prefix("var/mobile/") {
            candidates.push(rel);
        }
        'outer: for cand in candidates {
            for suffix_top in prefixes
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(""))
            {
                let full = if suffix_top.is_empty() {
                    cand.to_string()
                } else {
                    format!("{suffix_top}/{cand}")
                };
                if view.exists(&full).unwrap_or(false) {
                    add(&mut raw, (*bundle).to_string(), full);
                    break 'outer;
                }
            }
        }
    }
    raw.into_iter().map(|(k, v)| (k, v.join("；"))).collect()
}

fn plist_bundle(v: &plist::Value) -> Option<String> {
    fn walk(v: &plist::Value, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        match v {
            plist::Value::String(s) => {
                let s = s.trim();
                if s.contains('.')
                    && !s.starts_with('$')
                    && !s.ends_with(".plist")
                    && !s.contains("NSKeyed")
                    && s.len() > 3
                {
                    *out = Some(s.to_string());
                }
            }
            plist::Value::Array(a) => {
                for x in a {
                    walk(x, out);
                    if out.is_some() {
                        break;
                    }
                }
            }
            plist::Value::Dictionary(d) => {
                for x in d.values() {
                    walk(x, out);
                    if out.is_some() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = None;
    walk(v, &mut out);
    out
}

fn finding(
    run: &Run,
    item: &str,
    conclusion: &str,
    meaning: &str,
) -> lovelycarve_plugin::Result<()> {
    run.record_values(
        "ios.finding",
        vec![
            item.clone().into(),
            conclusion.clone().into(),
            meaning.clone().into(),
        ],
    )
    .emit()
}

/// evidence 作用域：一站式分析。
pub fn ios_backup(run: &Run) -> lovelycarve_plugin::Result<()> {
    let inputs: Vec<(String, FileRef)> = run
        .inputs()
        .iter()
        .map(|f| (f.view_path().unwrap_or(f.name()).to_string(), f.clone()))
        .collect();
    if inputs.is_empty() {
        return Err(lovelycarve_plugin::Error::invalid_params(
            "未找到任何 iOS 备份/日志输入（需要 Manifest.plist 或 sysdiagnose 目录）",
        ));
    }
    let mut ctx = Ctx {
        anchor: anchor_from_inputs(&inputs),
        files: inputs,
        findings: Vec::new(),
    };
    run.progress(1, Some(8), Some("解析备份清单"))?;

    // 1. Manifest.plist / Status / Info
    let binfo = backup::analyze(run, &ctx.files)?;
    let apps_manifest = backup::apps_from_manifest(run, &ctx.files)?;
    run.progress(2, Some(8), Some("解析 sysdiagnose"))?;

    // 2. sysdiagnose 路由
    let mut d = DiagResult {
        bt_mac: None,
        battery_activation: None,
        battery_serial: None,
        battery_health: None,
        domain_sizes: Vec::new(),
        anchor_unix: ctx.anchor,
    };
    let mut install_first: installlog::FirstInstall = Default::default();
    let mut plsqls: Vec<(String, FileRef)> = Vec::new();
    let all_files: Vec<FileRef> = ctx.files.iter().map(|(_, f)| f.clone()).collect();
    for (p, f) in &ctx.files {
        run.check_cancelled()?;
        let name = f.name();
        let lower = name.to_lowercase();
        if lower == "bluetooth_status.txt" {
            diag::bluetooth(run, f, &mut d, false)?;
        } else if lower.starts_with("bdc_once") && lower.ends_with(".csv") {
            diag::bdc_once(run, f, &mut d, false)?;
        } else if lower == "batteryhealth.log" {
            diag::battery_health(run, f, &mut d, false)?;
        } else if lower == "com.apple.powerd.bdc.plist" {
            diag::bdc_plist(run, f, &mut d, false)?;
        } else if lower == "systemversion.plist" {
            diag::system_version(run, f)?;
        } else if lower == "com.apple.mobilebackup.plist" {
            diag::mobile_backup_sizing(run, f, &mut d)?;
        } else if lower.starts_with("mobile_installation.log") {
            let first = installlog::parse(run, f, false)?;
            for (b, e) in first {
                let prev = install_first.get(&b);
                let take = match prev {
                    Some(old) => {
                        (e.year, e.month, e.day, e.hour, e.minute)
                            < (old.year, old.month, old.day, old.hour, old.minute)
                    }
                    None => true,
                };
                if take {
                    install_first.insert(b, e);
                }
            }
        } else if lower.ends_with(".plsql") {
            plsqls.push((p.clone(), f.clone()));
        }
    }

    run.progress(4, Some(8), Some("解析 powerlog"))?;
    // 4. powerlog
    let mut plout: Option<powerlog::PowerlogOut> = None;
    for (p, f) in &plsqls {
        run.check_cancelled()?;
        match powerlog::summarize(run, f, &all_files, ctx.anchor) {
            Ok(o) => {
                if plout.is_none() {
                    plout = Some(o);
                }
            }
            Err(e) => {
                run.warn_file(f, "POWERLOG_FAIL", format!("{p}: {e}"))?;
            }
        }
    }

    run.progress(6, Some(8), Some("汇总取证发现"))?;
    // 5. findings
    if let Some(b) = &binfo {
        // 区分两种形态：完整备份（有 Manifest.db，IsEncrypted=true 时文件确为密文）
        // 与文件级提取（无 Manifest.db，域目录数据为明文，解析无需口令）
        let manifest_db = run
            .view()
            .map(|v| v.exists("Manifest.db").unwrap_or(false))
            .unwrap_or(false);
        let conclusion = if !b.encrypt {
            "备份未加密".to_string()
        } else if let Some(pw) = &b.resolved_password {
            let src = b.password_source.unwrap_or("口令解锁");
            let disp = if pw.is_empty() {
                "<空密码>"
            } else {
                pw.as_str()
            };
            format!("备份已解锁（{src}），备份密码为：{disp}")
        } else if manifest_db {
            "备份已加密（IsEncrypted=true），请输入备份密码或开启自动爆破（4/6位纯数字）"
                .to_string()
        } else {
            "Manifest 标记 IsEncrypted=true，但检材为文件级提取（无 Manifest.db，数据为明文），本插件解析无需口令；如需解密完整备份文件本体仍需口令".to_string()
        };
        finding(
            run,
            "备份密码（口令）",
            &conclusion,
            &format!(
                "Manifest.plist；密钥包参数见 ios.keybag（{}）",
                b.keybag
                    .as_ref()
                    .map(|k| format!("类密钥 {} 个", k.class_count))
                    .unwrap_or_else(|| "未见 BackupKeyBag".into())
            ),
        )?;
        if !b.class_keys.is_empty() && b.encrypt {
            encrypted_backup::analyze_encrypted_backup(run, b, &ctx.files)?;
        }
        finding(
            run,
            "备份时间（UTC+8）",
            &format!(
                "{}（UTC+8；UTC 原值 {}）",
                b.backup_unix.map(fmt_plus8).unwrap_or_default(),
                b.backup_raw
            ),
            "Manifest.plist / Date 字段为 UTC，已换算 UTC+8",
        )?;
        finding(
            run,
            "设备序列号",
            &b.serial,
            "Manifest.plist / Lockdown / SerialNumber",
        )?;
        finding(
            run,
            "设备与系统",
            &format!(
                "{} {}（{} {} / iOS {} / {}）",
                b.device_name,
                util::iphone_model(&b.product_type).unwrap_or(&b.product_type),
                b.product_type,
                b.build_version,
                b.product_version,
                if b.passcode_set {
                    "设有锁屏密码"
                } else {
                    "无锁屏密码"
                }
            ),
            "Manifest.plist Lockdown + ProductType 型号映射",
        )?;
        if !b.udid.is_empty() {
            finding(run, "UDID", &b.udid, "Lockdown UniqueDeviceID")?;
        }
    }
    if let Some(mac) = &d.bt_mac {
        finding(
            run,
            "蓝牙 MAC 地址",
            mac,
            "sysdiagnose logs/Bluetooth/CoreCapture/bluetooth_status.txt",
        )?;
    }
    if let Some(act) = &d.battery_activation {
        finding(
            run,
            "电池激活时间（UTC+8）",
            act,
            "BDC_Once_*.csv 首行 TimeStamp（电量计首次初始化，设备本地时间；本检材设备时区 UTC+8，见 sysdiagnose 目录名 +0800）",
        )?;
    }
    if let Some(ser) = &d.battery_serial {
        finding(
            run,
            "电池序列号",
            ser,
            "com.apple.powerd.bdc.plist / BatterySerialNumber",
        )?;
    }
    if let (Some(h), Some(cyc)) = (d.battery_health, plout.as_ref().and_then(|o| o.cycle_count)) {
        finding(
            run,
            "电池健康度",
            &format!("{h}%（循环 {cyc:.0} 次）"),
            "BatteryHealth.log + powerlog CycleCount",
        )?;
    }
    if let Some(o) = &plout {
        if let Some((raw, unix)) = o.charge_last_reach {
            finding(
                run,
                "最近一次充至 100%（UTC+8）",
                &format!("原始时间戳 {raw}（= {} UTC+8）", fmt_plus8(unix)),
                "powerlog PLBatteryAgent_EventBackward_Battery：最后一段 Level=100 会话首条采样",
            )?;
        }
        if let Some((unix, app)) = &o.unlock_first {
            finding(
                run,
                "首次解锁屏幕（UTC+8）",
                &format!(
                    "{}（UTC+8）{}",
                    fmt_plus8(*unix),
                    if app.is_empty() {
                        String::new()
                    } else {
                        format!("，其后首个前台应用 {app}")
                    }
                ),
                "powerlog SBLock Locked 1→0 首次变化 + ScreenState bundleID 关联（±15s 窗口）",
            )?;
        }
        if let Some(e) = o.epoch {
            finding(
                run,
                "powerlog 时间基准判别",
                match e {
                    util::Epoch::Unix => "Unix 秒（与 sysdiagnose 采集时间吻合）",
                    util::Epoch::Apple => "Apple epoch（2001-01-01 基准）",
                },
                "按库内中值时间与 sysdiagnose 目录名锚点自动判别",
            )?;
        }
    }
    for (bundle, e) in &install_first {
        let disp = appmap::name(bundle)
            .map(|n| format!("{n}（{bundle}）"))
            .unwrap_or_else(|| bundle.clone());
        finding(
            run,
            &format!("首次安装请求：{disp}"),
            &format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}（UTC+8，{}）",
                e.year, e.month, e.day, e.hour, e.minute, e.second, e.action
            ),
            &format!(
                "mobile_installation.log：Install of …/{}.app（{}）；日志行首为设备本地时间=UTC+8",
                e.app_name, e.detail
            ),
        )?;
    }
    finding(
        run,
        "业务数据库",
        "短信/通话/通讯录/蓝牙/WiFi/账户/相册/笔记及第三方应用数据由对应分组分析器输出（避免重复）",
        "运行 iOS通讯信息 / iOS设备连接 / iOS媒体 / iOS应用信息 查看明细",
    )?;
    if let Some(total) = d
        .domain_sizes
        .iter()
        .find(|(k, _)| k == "_TotalSize")
        .map(|(_, n)| *n)
    {
        finding(
            run,
            "备份预检总体积",
            &fmt_bytes(total),
            "MobileBackup.plist PreflightSizing/_TotalSize",
        )?;
    }

    // 6. overview（给默认摘要视图）
    let rows = ctx.findings.len();
    let _ = rows;
    for (item, conclusion, meaning) in [
        (
            "输入文件",
            &format!(
                "{} 个（Manifest {} / 安装日志 / sysdiagnose / powerlog {}）",
                ctx.files.len(),
                if binfo.is_some() { "有" } else { "无" },
                plsqls.len()
            ),
            "按 plugin.toml inputs 模式自动收集",
        ),
        (
            "应用清单",
            &format!(
                "Manifest Applications {} 个；安装事件日志含首装请求 {} 个应用",
                apps_manifest.len(),
                install_first.len()
            ),
            "Manifest.plist Applications 与 mobile_installation.log 分别统计",
        ),
    ] {
        run.record_values(
            "ios.overview",
            vec![
                item.clone().into(),
                conclusion.clone().into(),
                meaning.clone().into(),
            ],
        )
        .emit()?;
    }

    // 7. 画像事实
    if let Some(b) = &binfo {
        if !b.serial.is_empty() {
            run.provide("serial_number", b.serial.clone())?;
        }
        if !b.product_version.is_empty() {
            run.provide("ios_version", format!("iOS {}", b.product_version))?;
        }
        if !b.imei.is_empty() {
            run.provide("imei", b.imei.clone())?;
        }
        if !b.iccid.is_empty() {
            run.provide("iccid", b.iccid.clone())?;
        }
        if let Some(u) = b.backup_unix {
            run.provide("backup_time", fmt_plus8(u))?;
        }
        run.provide("device_product", b.product_type.clone())?;
    }
    run.provide("ios_analyzed", "done")?;
    run.set_summary(format!(
        "iOS备份取证：{} 项发现；设备 {} 序列号 {}；powerlog {}; 通讯/应用/连接/电池/媒体各分组将自动运行",
        10 + install_first.len(),
        binfo.as_ref().map(|b| b.product_type.clone()).unwrap_or_default(),
        binfo.as_ref().map(|b| b.serial.clone()).unwrap_or_default(),
        if plout.is_some() { "已解析" } else { "未发现" },
    ));
    Ok(())
}

/// files 作用域：选中的 PLSQL 深度归纳。
fn plower_of(f: &FileRef) -> String {
    f.view_path().unwrap_or(f.name()).to_lowercase()
}

fn is_app_pref(path: &str, name: &str) -> bool {
    name.ends_with(".plist") && path.contains("/library/preferences/")
}

fn is_media_file(name: &str) -> bool {
    [
        ".jpg", ".jpeg", ".png", ".gif", ".heic", ".mov", ".mp4", ".pdf",
    ]
    .iter()
    .any(|e| name.ends_with(e))
}

/// 分组分析器共用的输入路由。
fn route_group(run: &Run, group: &str) -> lovelycarve_plugin::Result<usize> {
    let files: Vec<(String, FileRef)> = run
        .inputs()
        .iter()
        .map(|f| (f.view_path().unwrap_or(f.name()).to_string(), f.clone()))
        .collect();
    let all: Vec<FileRef> = files.iter().map(|(_, f)| f.clone()).collect();
    let anchor = anchor_from_inputs(&files);
    let secrets = keychain::secrets(&files);
    let mut wifi_nets: std::collections::HashMap<String, backupdb::WifiNet> = Default::default();
    let mut wifi_file: Option<FileRef> = None;
    let container_index = if group == "appinfo" {
        app_container_index(run)
    } else {
        Default::default()
    };
    let mut display_map: std::collections::HashMap<String, String> = Default::default();
    let mut version_map: std::collections::HashMap<String, String> = Default::default();
    let mut install_map: std::collections::HashMap<String, lovelycarve_plugin::Timestamp> =
        Default::default();
    // 显示名/版本/安装时间需在应用清单发射前构建（各数据源独立于输入顺序）
    if group == "appinfo" {
        for (_p, f) in files
            .iter()
            .filter(|(_, f)| f.name().to_lowercase() == "photos.sqlite")
        {
            for (bid, name) in backupdb::import_name_map(run, f, &all)? {
                display_map.entry(bid).or_insert(name);
            }
        }
        for (_p, f) in files
            .iter()
            .filter(|(_, f)| f.name().to_lowercase() == "datausage.sqlite")
        {
            let proc_names = datausage::extract_process_app_map(f, &all);
            for (bid, pname) in proc_names {
                appmap::register_dynamic(&bid, &pname);
                display_map.entry(bid).or_insert(pname);
            }
        }
        for (_p, f) in files.iter().filter(|(_, f)| {
            let n = f.name().to_lowercase();
            n.ends_with(".plist") && plower_of(f).contains("/library/preferences/")
        }) {
            let stem = f.name().trim_end_matches(".plist").to_string();
            let bundle = appprefs::bundle_from_stem(&stem);
            if let Some((ver, inst)) = appprefs::pref_meta(run, f)? {
                version_map.entry(bundle.clone()).or_insert(ver);
                if let Some(t) = inst {
                    install_map.entry(bundle.clone()).or_insert(t);
                }
            }
            // 配置值里的账号标识（如 WhatsApp 自身 JID）
            for id in appprefs::pref_identifiers(f) {
                run.record_values(
                    "ios.app_artifact",
                    vec![
                        bundle.clone().into(),
                        "账号标识（应用配置）".into(),
                        id.into(),
                        f.view_path().unwrap_or(f.name()).to_string().into(),
                        "应用配置文件".into(),
                    ],
                )
                .source(crate::util::node_source(f))
                .emit()?;
            }
        }
    }
    let mut n = 0usize;
    for (p, f) in &files {
        run.check_cancelled()?;
        match route_one(
            run,
            group,
            f,
            p,
            &files,
            &all,
            &secrets,
            &mut wifi_nets,
            &mut wifi_file,
            &container_index,
            &mut display_map,
            &version_map,
            &install_map,
            anchor,
        ) {
            Ok(k) => n += k,
            // 单个库解析失败只降级为告警，不拖垮整个分组分析器
            Err(e) => run.warn_file(f, "ROUTE_FAIL", format!("{e}"))?,
        }
    }
    // WiFi 记录跨文件合并后再发射（known-networks 与 private-mac 互为补充）。
    if !wifi_nets.is_empty() {
        if let Some(f) = &wifi_file {
            n += backupdb::wifi_emit(run, &wifi_nets, &secrets, f)?;
        }
    }
    Ok(n)
}

#[allow(clippy::too_many_arguments)]
fn route_one(
    run: &Run,
    group: &str,
    f: &FileRef,
    p: &str,
    files: &[(String, FileRef)],
    all: &[FileRef],
    secrets: &std::collections::HashMap<String, String>,
    wifi_nets: &mut std::collections::HashMap<String, backupdb::WifiNet>,
    wifi_file: &mut Option<FileRef>,
    container_index: &std::collections::HashMap<String, String>,
    display_map: &mut std::collections::HashMap<String, String>,
    version_map: &std::collections::HashMap<String, String>,
    install_map: &std::collections::HashMap<String, lovelycarve_plugin::Timestamp>,
    anchor: Option<i64>,
) -> lovelycarve_plugin::Result<usize> {
    let lower = f.name().to_lowercase();
    let plower = p.to_lowercase();
    let mut n = 0usize;
    match group {
        "comm" => {
            if lower == "sms.db" {
                n += backupdb::sms(run, f, &all)?;
            } else if lower == "addressbook.sqlitedb" || lower == "addressbook.sqlitedb.db" {
                n += backupdb::contact(run, f, &all)?;
            } else if lower == "callhistory.storedata"
                || lower == "callhistory.storedata.db"
                || lower == "call_history.db"
            {
                n += backupdb::calllog(run, f, &all)?;
            } else if lower == "deliverednotifications.plist" {
                n += pushkey::notifications(run, f)?;
            } else if lower == "apollo_db.sqlite" {
                n += kvjson::scan(run, f)?;
            } else if apps::is_app_db(&lower) {
                n += apps::route(run, f, &all)?;
            }
        }
        "appinfo" => {
            if lower == "photos.sqlite" {
                // 显示名映射已在循环前构建
            } else if lower.starts_with("mobile_installation.log") {
                installlog::parse(run, f, true)?;
                n += 1;
            } else if lower == "history.db" {
                n += backupdb::web_history(run, f, &all)?;
            } else if lower == "bookmarks.db" {
                n += safari::bookmarks(run, f, &all)?;
            } else if lower == "downloads.plist" {
                n += safari::downloads(run, f)?;
            } else if lower.ends_with(".realm") {
                n += realm::scan(run, f)?;
            } else if lower == "apollo_db.sqlite" {
                n += kvjson::scan(run, f)?;
            } else if is_app_pref(&plower, &lower) {
                n += appprefs::scan(run, f)?;
            } else if lower == "datausage.sqlite" {
                n += datausage::parse_data_usage(run, f, &all, &display_map)?;
            } else if lower == "manifest.plist" {
                let apps = backup::apps_from_manifest(run, &files)?;
                for a in &apps {
                    let (bundle, name, short, build, item, placeholder) = (
                        &a.bundle,
                        &a.name,
                        &a.short,
                        &a.build,
                        &a.item,
                        &a.placeholder,
                    );
                    if bundle.starts_with("com.apple.")
                        && name.is_empty()
                        && appmap::name(bundle).is_none()
                    {
                        continue;
                    }
                    // 显示名优先级：Photos 导入记录 > App Store itemName > Manifest > 进程交叉验证/内置映射
                    let display = display_map
                        .get(bundle)
                        .cloned()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| (!item.trim().is_empty()).then(|| item.clone()))
                        .or_else(|| (!name.trim().is_empty()).then(|| name.clone()))
                        .or_else(|| appmap::resolve_name(bundle))
                        .unwrap_or_default();
                    // 版本号优先级：应用配置文件（SPClientVersion/ds-app-version 等）
                    // > App Store 短版本 > Manifest；构建号仅在都缺失时兜底
                    let version = version_map
                        .get(bundle)
                        .cloned()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| (!short.trim().is_empty()).then(|| short.clone()))
                        .unwrap_or_else(|| build.clone());
                    run.record_values(
                        "ios.app",
                        vec![
                            bundle.clone().into(),
                            display.into(),
                            item.clone().into(),
                            version.into(),
                            build.clone().into(),
                            (if bundle.starts_with("com.apple.") {
                                "系统应用"
                            } else {
                                "第三方应用"
                            })
                            .into(),
                            Value::Boolean(*placeholder),
                            Value::Nil,
                            format!("AppDomain-{bundle}").into(),
                            container_index
                                .get(bundle)
                                .cloned()
                                .map(Value::from)
                                .unwrap_or(Value::Nil),
                            install_map
                                .get(bundle)
                                .map(|t| Value::from(t.as_str().to_string()))
                                .unwrap_or(Value::Nil),
                            "Manifest.plist Applications".into(),
                        ],
                    )
                    .source(crate::util::node_source(f))
                    .emit()?;
                    n += 1;
                }
            } else if lower.ends_with(".plsql") {
                let _ = powerlog::usage_only(run, f, &all)?;
                n += 1;
            }
        }
        "connect" => {
            if lower == "bluetooth_status.txt" {
                let mut d = diag::DiagResult {
                    bt_mac: None,
                    battery_activation: None,
                    battery_serial: None,
                    battery_health: None,
                    domain_sizes: Vec::new(),
                    anchor_unix: anchor,
                };
                diag::bluetooth(run, f, &mut d, false)?;
                n += 1;
            } else if lower == "com.apple.mobilebluetooth.ledevices.paired.db" {
                n += backupdb::bluetooth(run, f, &all, true)?;
            } else if lower == "com.apple.mobilebluetooth.ledevices.other.db" {
                n += backupdb::bluetooth(run, f, &all, false)?;
            } else if lower == "com.apple.wifi.plist"
                || lower == "com.apple.wifi-private-mac-networks.plist"
            {
                backupdb::wifi(run, f, wifi_nets)?;
                *wifi_file = Some(f.clone());
            } else if lower == "com.apple.wifi.known-networks.plist"
                || lower == "com.apple.wifi-networks.plist.backup"
            {
                backupdb::wifi(run, f, wifi_nets)?;
                *wifi_file = Some(f.clone());
            } else if lower == "accounts3.sqlite" {
                n += backupdb::account(run, f, &all)?;
            } else if lower == "keychain.json"
                || lower == "keychain-backup.plist"
                || lower == "comparablekeychainbackup.plist"
            {
                n += keychain::items(run, f)?;
            } else if lower == "idevice_info.txt" {
                n += deviceid::idevice_info(run, f)?;
            } else if lower == "com.apple.commcenter.plist" {
                n += deviceid::commcenter(run, f)?;
            } else if lower == "com.apple.mobileinternetsharing.plist"
                || lower == "com.apple.mobilehotspot.plist"
            {
                n += deviceid::hotspot(run, f)?;
            } else if lower == "com.apple.mobilebluetooth.devices.plist" {
                n += deviceid::bluetooth_plist(run, f)?;
            } else if apps::is_app_db(&lower) {
                n += apps::route(run, f, &all)?;
            }
        }
        "airdrop" => {
            // AirDrop 本机标识与配置（传输记录见「交互记录」栏）。
            if lower == "com.apple.sharingd.plist" {
                n += airdrop::identity(run, f)?;
            }
        }
        "coreduet" => {
            // CoreDuet 交互库：事件全量 + 联系人 + 传输工件 + 关键词 + 库元信息。
            if lower == "interactionc.db" {
                // coreduet::scan 内含 AirDrop 分享动作的权威记录解析。
                n += coreduet::scan(run, f, &all)?;
            }
        }
        "icloud" => {
            if lower == "com.apple.mobilebackup.plist" {
                n += diag::cloud_backup(run, f)?;
            } else if lower == "store.cloudphotodb" {
                n += icloud::cloudphotodb(run, f, &all)?;
            } else if let Some((record, svc)) = icloud::service_of(&lower) {
                n += icloud::prefs(run, f, (record, svc))?;
            } else if icloud::is_icloud_pref(&plower, &lower) {
                n += icloud::prefs(run, f, ("ios.icloud_drive", "iCloud Drive 文件域"))?;
            }
        }
        "whatsapp" => {
            if lower == "chatstorage.sqlite" {
                n += apps::whatsapp_msg(run, f, &all)?;
            } else if lower == "callhistory.sqlite" {
                n += apps::whatsapp_calls(run, f, &all)?;
            }
        }
        "power" => {
            if lower.starts_with("bdc_once") && lower.ends_with(".csv") {
                let mut d = diag::DiagResult {
                    bt_mac: None,
                    battery_activation: None,
                    battery_serial: None,
                    battery_health: None,
                    domain_sizes: Vec::new(),
                    anchor_unix: anchor,
                };
                diag::bdc_once(run, f, &mut d, false)?;
            } else if lower == "batteryhealth.log" {
                let mut d = diag::DiagResult {
                    bt_mac: None,
                    battery_activation: None,
                    battery_serial: None,
                    battery_health: None,
                    domain_sizes: Vec::new(),
                    anchor_unix: anchor,
                };
                diag::battery_health(run, f, &mut d, false)?;
            } else if lower == "com.apple.powerd.bdc.plist" {
                let mut d = diag::DiagResult {
                    bt_mac: None,
                    battery_activation: None,
                    battery_serial: None,
                    battery_health: None,
                    domain_sizes: Vec::new(),
                    anchor_unix: anchor,
                };
                diag::bdc_plist(run, f, &mut d, false)?;
            } else if lower.ends_with(".plsql") {
                let _ = powerlog::analyze_opts(run, f, &all, anchor, false)?;
                n += 1;
            }
        }
        "media" => {
            if lower == "photos.sqlite" {
                n += backupdb::photo(run, f, &all)?;
            } else if lower == "notestore.sqlite" || lower == "notes.sqlite" {
                n += backupdb::note(run, f, &all)?;
            } else if is_media_file(&lower) {
                n += media_exif::scan(run, f)?;
            } else if filesapp::matches(&plower) {
                n += filesapp::scan(run, f)?;
            }
        }
        _ => {}
    }
    Ok(n)
}

pub fn ios_comm(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "comm")?;
    run.set_summary(if n == 0 {
        "本检材无通讯类业务数据库（sms/通话/通讯录/通知）".into()
    } else {
        format!("通讯信息 {n} 项相关记录")
    });
    Ok(())
}
pub fn ios_apps(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "appinfo")?;
    run.set_summary(if n == 0 {
        "应用信息：本检材无对应数据文件".into()
    } else {
        format!("应用信息 {n} 项相关记录")
    });
    Ok(())
}
pub fn ios_connect(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "connect")?;
    run.set_summary(if n == 0 {
        "设备连接：本检材无对应数据文件".into()
    } else {
        format!("设备连接 {n} 项相关记录")
    });
    Ok(())
}
pub fn ios_power(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "power")?;
    run.set_summary(if n == 0 {
        "电池与屏幕：本检材无对应数据文件".into()
    } else {
        format!("电池与屏幕 {n} 项相关记录")
    });
    Ok(())
}
pub fn ios_whatsapp(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "whatsapp")?;
    run.set_summary(if n == 0 {
        "WhatsApp：本检材无 ChatStorage/CallHistory 数据库".into()
    } else {
        format!("WhatsApp {n} 项记录（会话/消息/通话/位置）")
    });
    Ok(())
}

pub fn ios_icloud(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "icloud")?;
    run.set_summary(if n == 0 {
        "iCloud：本检材无对应数据文件".into()
    } else {
        format!("iCloud {n} 项记录（备份/照片库/查找/云盘）")
    });
    Ok(())
}

pub fn ios_airdrop(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "airdrop")?;
    run.set_summary(if n == 0 {
        "AirDrop：本检材无分享/导入标记".into()
    } else {
        format!("AirDrop {n} 项记录（传输/标识）")
    });
    Ok(())
}

/// Photos 库完整解析（分类 fphotos）：资产 / 相册 / 成员 / 回忆时刻 /
/// 人物 / 场景 / iCloud 云端 / 分析状态。
pub fn ios_photos(run: &Run) -> lovelycarve_plugin::Result<()> {
    let inputs = run.inputs();
    let photos = inputs
        .iter()
        .find(|f| f.name().to_lowercase() == "photos.sqlite")
        .cloned();
    let Some(photos) = photos else {
        run.warn("NO_PHOTOS_DB", "证据中未发现 Photos.sqlite")?;
        return Ok(());
    };
    let mut n = 0usize;
    for file in inputs.iter() {
        if file.name().to_lowercase() != "photos.sqlite" {
            continue;
        }
        n += photos::scan(run, file, &inputs)?;
    }
    run.set_summary(if n == 0 {
        "照片库：本检材无 Photos 数据".into()
    } else {
        format!("照片库 {n} 项记录")
    });
    Ok(())
}

/// CoreDuet 交互库深度解析（分类 ginteraction）：interactionC.db 全表逐层解析。
pub fn ios_coreduet(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "coreduet")?;
    run.set_summary(if n == 0 {
        "交互记录：本检材无 interactionC.db".into()
    } else {
        format!("交互记录 {n} 项（事件/联系人/工件/库信息）")
    });
    Ok(())
}

pub fn ios_media(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "media")?;
    run.set_summary(if n == 0 {
        "媒体：本检材无对应数据文件".into()
    } else {
        format!("媒体 {n} 项相关记录")
    });
    Ok(())
}

/// evidence 作用域：tracev3 系统日志深度分析。

pub fn log_archive(run: &Run) -> lovelycarve_plugin::Result<()> {
    let inputs = run.inputs();
    let timesync_files: Vec<FileRef> = inputs
        .iter()
        .filter(|f| f.name().to_lowercase().ends_with(".timesync"))
        .cloned()
        .collect();
    let traces: Vec<FileRef> = inputs
        .iter()
        .filter(|f| f.name().to_lowercase().ends_with(".tracev3"))
        .cloned()
        .collect();
    if traces.is_empty() {
        // 检材不是 sysdiagnose（无 tracev3）时正常结束：这是文件级提取备份等场景
        run.set_summary(
            "本检材未包含系统日志（无 .tracev3）；sysdiagnose 类检材才有 system_logs.logarchive",
        );
        return Ok(());
    }
    let map = unifiedlog::load_timesync(&timesync_files);
    let kw = run.options().text("keyword").map(|s| s.to_string());
    let cap: usize = run
        .options()
        .text("limit")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(20000);
    let mut stats = unifiedlog::LogStats {
        total: 0,
        emitted: 0,
    };
    for f in &traces {
        run.check_cancelled()?;
        run.progress(0, Some(traces.len() as u64), Some(f.name()))?;
        if let Err(e) = unifiedlog::parse_one(run, f, &map, kw.as_deref(), cap, &mut stats) {
            run.warn_file(f, "TRACEV3_FAIL", e.to_string())?;
        }
        if stats.emitted >= cap {
            break;
        }
    }
    run.set_summary(format!(
        "日志条目共 {} 条，输出 {} 条（关键词 {:?}，上限 {}）",
        stats.total,
        stats.emitted,
        kw.unwrap_or_default(),
        cap
    ));
    Ok(())
}

pub fn powerlog_files(run: &Run) -> lovelycarve_plugin::Result<()> {
    let inputs = run.inputs();
    let mains = inputs
        .iter()
        .filter(|f| {
            let n = f.name().to_lowercase();
            n.ends_with(".plsql") || n.ends_with(".sqlite") || n.ends_with(".db")
        })
        .collect::<Vec<_>>();
    if mains.is_empty() {
        return Err(lovelycarve_plugin::Error::invalid_params(
            "请选择 Powerlog 主数据库（可同时选择同名 -wal/-shm）",
        ));
    }
    let all: Vec<FileRef> = inputs.to_vec();
    let mut n = 0usize;
    for f in &mains {
        run.check_cancelled()?;
        let out = powerlog::analyze(run, f, &all, None)?;
        n += 1;
        // powerlog.overview 总览
        if let (Some(cyc), Some(lvl)) = (out.cycle_count, out.level_last) {
            run.record_values(
                "powerlog.overview",
                vec![
                    "电池快照".into(),
                    format!("最新电量 {lvl:.0}%，循环 {cyc:.0} 次").into(),
                    f.name().to_string().into(),
                ],
            )
            .emit()?;
        }
        if let Some((raw, unix)) = out.charge_last_reach {
            run.record_values(
                "powerlog.overview",
                vec![
                    "充至100%".into(),
                    format!("{raw}（≈ {} UTC+8）", fmt_plus8(unix)).into(),
                    "最后一段 Level=100 会话首条".into(),
                ],
            )
            .emit()?;
        }
        if let Some((unix, app)) = out.unlock_first {
            run.record_values(
                "powerlog.overview",
                vec![
                    "首次解锁".into(),
                    format!(
                        "{}{}",
                        fmt_plus8(unix),
                        if app.is_empty() {
                            String::new()
                        } else {
                            format!(" → {app}")
                        }
                    )
                    .into(),
                    "SBLock 1→0 首次 + ScreenState".into(),
                ],
            )
            .emit()?;
        }
        if out.apps > 0 {
            run.record_values(
                "powerlog.overview",
                vec![
                    "应用".into(),
                    format!("AppVersions 去重 {} 个", out.apps).into(),
                    f.name().to_string().into(),
                ],
            )
            .emit()?;
        }
    }
    run.set_summary(format!("{n} 个 Powerlog 数据库完成归纳"));
    Ok(())
}

pub fn plugin() -> Plugin {
    let mut p = Plugin::new("org.lovelycarve.iosbackup");
    for s in specs::schemas() {
        p = p.schema(s);
    }
    p.analyzer("ios_backup", ios_backup)
        .analyzer("ios_comm", ios_comm)
        .analyzer("ios_whatsapp", ios_whatsapp)
        .analyzer("ios_icloud", ios_icloud)
        .analyzer("ios_airdrop", ios_airdrop)
        .analyzer("ios_coreduet", ios_coreduet)
        .analyzer("ios_photos", ios_photos)
        .analyzer("ios_apps", ios_apps)
        .analyzer("ios_connect", ios_connect)
        .analyzer("ios_power", ios_power)
        .analyzer("ios_media", ios_media)
        .analyzer("log_archive", log_archive)
        .analyzer("powerlog_files", powerlog_files)
}

export_plugin!(plugin);

#[cfg(test)]
mod wa_photos_tests;
