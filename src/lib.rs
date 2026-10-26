//! LovelyCarve iOS 备份与 Sysdiagnose 取证插件。
//!
//! 两个分析器：
//! - `ios_backup`（evidence）：Manifest/Status/Info + KeyBag + sysdiagnose（蓝牙/电池/
//!   安装日志/系统版本/预检体积）+ powerlog（充满/解锁/时长/每日），并汇总 ios.finding。
//! - `powerlog_files`（files）：选中 .PLSQL 的深度归纳（复用同一 powerlog 引擎）。

pub mod appmap;
pub mod backup;
pub mod apps;
pub mod apps_route;
pub mod apps_route_glued;
pub mod backupdb;
pub mod pushkey;
pub mod unifiedlog;
pub mod diag;
pub mod installlog;
pub mod powerlog;
pub mod specs;
pub mod util;

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

fn finding(run: &Run, item: &str, conclusion: &str, meaning: &str) -> lovelycarve_plugin::Result<()> {
    run.record_values(
        "ios.finding",
        vec![item.clone().into(), conclusion.clone().into(), meaning.clone().into()],
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
    #[derive(Default)]
    struct BackupCounts {
        sms: usize, contact: usize, call: usize, wifi: usize, bt: usize,
        account: usize, photo: usize, note: usize, app_msgs: usize,
    }
    let mut counts = BackupCounts::default();
    let all_files: Vec<FileRef> = ctx.files.iter().map(|(_, f)| f.clone()).collect();
    for (p, f) in &ctx.files {
        run.check_cancelled()?;
        let name = f.name();
        let lower = name.to_lowercase();
        if lower == "bluetooth_status.txt" {
            diag::bluetooth(run, f, &mut d)?;
        } else if lower.starts_with("bdc_once") && lower.ends_with(".csv") {
            diag::bdc_once(run, f, &mut d)?;
        } else if lower == "batteryhealth.log" {
            diag::battery_health(run, f, &mut d)?;
        } else if lower == "com.apple.powerd.bdc.plist" {
            diag::bdc_plist(run, f, &mut d)?;
        } else if lower == "systemversion.plist" {
            diag::system_version(run, f)?;
        } else if lower == "com.apple.mobilebackup.plist" {
            diag::mobile_backup_sizing(run, f, &mut d)?;
        } else if lower.starts_with("mobile_installation.log") {
            let first = installlog::parse(run, f)?;
            for (b, e) in first {
                let prev = install_first.get(&b);
                let take = match prev {
                    Some(old) => (e.year, e.month, e.day, e.hour, e.minute) < (old.year, old.month, old.day, old.hour, old.minute),
                    None => true,
                };
                if take {
                    install_first.insert(b, e);
                }
            }
        } else if lower == "sms.db" {
            counts.sms += backupdb::sms(run, f, &all_files)?;
        } else if lower == "addressbook.sqlitedb" || lower == "addressbook.sqlitedb.db" {
            counts.contact += backupdb::contact(run, f, &all_files)?;
        } else if lower == "callhistory.storedata" || lower == "callhistory.storedata.db" || lower == "call_history.db" {
            counts.call += backupdb::calllog(run, f, &all_files)?;
        } else if lower == "com.apple.wifi.plist" || lower == "com.apple.wifi-private-mac-networks.plist" {
            counts.wifi += backupdb::wifi(run, f)?;
        } else if lower == "com.apple.mobilebluetooth.ledevices.paired.db" {
            counts.bt += backupdb::bluetooth(run, f, &all_files, true)?;
        } else if lower == "com.apple.mobilebluetooth.ledevices.other.db" {
            counts.bt += backupdb::bluetooth(run, f, &all_files, false)?;
        } else if lower == "accounts3.sqlite" {
            counts.account += backupdb::account(run, f, &all_files)?;
        } else if lower == "photos.sqlite" {
            counts.photo += backupdb::photo(run, f, &all_files)?;
        } else if lower == "notestore.sqlite" {
            counts.note += backupdb::note(run, f, &all_files)?;
        } else if lower == "deliverednotifications.plist" {
            pushkey::notifications(run, f)?;
        } else if lower == "comparablekeychainbackup.plist" {
            pushkey::keychain_items(run, f)?;
        } else if lower == "history.db" {
            backupdb::web_history(run, f, &all_files)?;
        } else if apps::is_app_db(&lower) {
            let m = apps::route(run, f, &all_files)?;
            counts.app_msgs += m;
        } else if lower.ends_with(".plsql") {
            plsqls.push((p.clone(), f.clone()));
        }
    }

    // 3. Manifest Applications → ios.app（与 AppVersions/AppRunTime 合并由各来源独立出记录）
    for (bundle, name, _v, placeholder) in &apps_manifest {
        if bundle.starts_with("com.apple.") && name.is_empty() && appmap::name(bundle).is_none() {
            continue;
        }
        let display_name = if name.is_empty() { appmap::name(bundle).unwrap_or("").to_string() } else { name.clone() };
        let size = d
            .domain_sizes
            .iter()
            .find(|(dom, _)| dom.strip_prefix("AppDomain-") == Some(bundle.as_str()))
            .map(|(_, n)| *n);
        run.record_values(
            "ios.app",
            vec![
                bundle.clone().into(),
                display_name.into(),
                appmap::name(bundle).unwrap_or("").into(),
                (if bundle.starts_with("com.apple.") { "系统应用" } else { "第三方应用" }).into(),
                Value::Boolean(*placeholder),
                size.unwrap_or(0).into(),
                format!("AppDomain-{bundle}").into(),
                install_first
                    .get(bundle)
                    .map(|e| Value::from(format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", e.year, e.month, e.day, e.hour, e.minute, e.second)))
                    .unwrap_or(Value::Nil),
                "Manifest.plist Applications + PreflightSizing + mobile_installation.log".into(),
            ],
        )
        .emit()?;
    }

    run.progress(4, Some(8), Some("解析 powerlog"))?;
    // 4. powerlog
    let mut plout: Option<powerlog::PowerlogOut> = None;
    for (p, f) in &plsqls {
        run.check_cancelled()?;
        match powerlog::analyze(run, f, &all_files, ctx.anchor) {
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
        finding(
            run,
            "备份密码（口令）",
            if b.encrypt { "备份已加密（IsEncrypted=true），口令需持有人提供" } else { "备份未加密" },
            &format!(
                "Manifest.plist；密钥包参数见 ios.keybag（{}）",
                b.keybag
                    .as_ref()
                    .map(|k| format!("类密钥 {} 个", k.class_count))
                    .unwrap_or_else(|| "未见 BackupKeyBag".into())
            ),
        )?;
        finding(
            run,
            "备份时间（UTC+8）",
            &format!("{}（UTC+8；UTC 原值 {}）", b.backup_unix.map(fmt_plus8).unwrap_or_default(), b.backup_raw),
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
                if b.passcode_set { "设有锁屏密码" } else { "无锁屏密码" }
            ),
            "Manifest.plist Lockdown + ProductType 型号映射",
        )?;
        if !b.udid.is_empty() {
            finding(run, "UDID", &b.udid, "Lockdown UniqueDeviceID")?;
        }
    }
    if let Some(mac) = &d.bt_mac {
        finding(run, "蓝牙 MAC 地址", mac, "sysdiagnose logs/Bluetooth/CoreCapture/bluetooth_status.txt")?;
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
        finding(run, "电池序列号", ser, "com.apple.powerd.bdc.plist / BatterySerialNumber")?;
    }
    if let (Some(h), Some(cyc)) = (d.battery_health, plout.as_ref().and_then(|o| o.cycle_count)) {
        finding(run, "电池健康度", &format!("{h}%（循环 {cyc:.0} 次）"), "BatteryHealth.log + powerlog CycleCount")?;
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
                    if app.is_empty() { String::new() } else { format!("，其后首个前台应用 {app}") }
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
        let disp = appmap::name(bundle).map(|n| format!("{n}（{bundle}）")).unwrap_or_else(|| bundle.clone());
        finding(
            run,
            &format!("首次安装请求：{disp}"),
            &format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}（UTC+8，{}）",
                e.year, e.month, e.day, e.hour, e.minute, e.second, e.action
            ),
            &format!("mobile_installation.log：Install of …/{}.app（{}）；日志行首为设备本地时间=UTC+8", e.app_name, e.detail),
        )?;
    }
    if counts.app_msgs > 0 {
        finding(run, "第三方应用数据", &format!("{} 条消息记录", counts.app_msgs), "逐 App 解析（小红书/知乎/B站/通用 IM 等）")?;
    }
    let bd = [
        ("短信", counts.sms), ("通话记录", counts.call), ("通讯录", counts.contact),
        ("WiFi网络", counts.wifi), ("蓝牙设备", counts.bt), ("账户", counts.account),
        ("相册资产", counts.photo), ("笔记", counts.note),
    ];
    for (what, n) in bd {
        if n > 0 {
            finding(run, &format!("备份数据库：{what}"), &format!("{n} 条"), "iOS 备份业务数据库解析（Apple epoch 时间统一转 UTC+8）")?;
        }
    }
    if let Some(total) = d.domain_sizes.iter().find(|(k, _)| k == "_TotalSize").map(|(_, n)| *n) {
        finding(run, "备份预检总体积", &fmt_bytes(total), "MobileBackup.plist PreflightSizing/_TotalSize")?;
    }

    // 6. overview（给默认摘要视图）
    let rows = ctx.findings.len();
    let _ = rows;
    for (item, conclusion, meaning) in [
        (
            "输入文件",
            &format!("{} 个（Manifest {} / 安装日志 / sysdiagnose / powerlog {}）",
                ctx.files.len(),
                if binfo.is_some() { "有" } else { "无" },
                plsqls.len()),
            "按 plugin.toml inputs 模式自动收集",
        ),
        (
            "应用清单",
            &format!("Manifest Applications {} 个；安装事件日志含首装请求 {} 个应用", apps_manifest.len(), install_first.len()),
            "Manifest.plist Applications 与 mobile_installation.log 分别统计",
        ),
    ] {
        run.record_values(
            "ios.overview",
            vec![item.clone().into(), conclusion.clone().into(), meaning.clone().into()],
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
        if let Some(u) = b.backup_unix {
            run.provide("backup_time", fmt_plus8(u))?;
        }
        run.provide("device_product", b.product_type.clone())?;
    }
    run.set_summary(format!(
        "iOS备份取证：{} 项发现；设备 {} 序列号 {}；powerlog {}",
        10 + install_first.len(),
        binfo.as_ref().map(|b| b.product_type.clone()).unwrap_or_default(),
        binfo.as_ref().map(|b| b.serial.clone()).unwrap_or_default(),
        if plout.is_some() { "已解析" } else { "未发现" },
    ));
    Ok(())
}

/// files 作用域：选中的 PLSQL 深度归纳。
/// 分组分析器共用的输入路由。
fn route_group(run: &Run, group: &str) -> lovelycarve_plugin::Result<usize> {
    let files: Vec<(String, FileRef)> = run
        .inputs()
        .iter()
        .map(|f| (f.view_path().unwrap_or(f.name()).to_string(), f.clone()))
        .collect();
    let all: Vec<FileRef> = files.iter().map(|(_, f)| f.clone()).collect();
    let anchor = anchor_from_inputs(&files);
    let mut n = 0usize;
    for (_p, f) in &files {
        run.check_cancelled()?;
        let lower = f.name().to_lowercase();
        match group {
            "comm" => {
                if lower == "sms.db" {
                    n += backupdb::sms(run, f, &all)?;
                } else if lower == "addressbook.sqlitedb" || lower == "addressbook.sqlitedb.db" {
                    n += backupdb::contact(run, f, &all)?;
                } else if lower == "callhistory.storedata" || lower == "callhistory.storedata.db" || lower == "call_history.db" {
                    n += backupdb::calllog(run, f, &all)?;
                } else if lower == "deliverednotifications.plist" {
                    n += pushkey::notifications(run, f)?;
                }
            }
            "appinfo" => {
                if lower.starts_with("mobile_installation.log") {
                    installlog::parse(run, f)?;
                    n += 1;
                } else if lower == "history.db" {
                    n += backupdb::web_history(run, f, &all)?;
                } else if lower == "manifest.plist" {
                    let apps = backup::apps_from_manifest(run, &files)?;
                    for (bundle, name, _v, placeholder) in &apps {
                        if bundle.starts_with("com.apple.") && name.is_empty() && appmap::name(bundle).is_none() {
                            continue;
                        }
                        let display = if name.is_empty() { appmap::name(bundle).unwrap_or("").to_string() } else { name.clone() };
                        run.record_values(
                            "ios.app",
                            vec![
                                bundle.clone().into(),
                                display.into(),
                                appmap::name(bundle).unwrap_or("").into(),
                                (if bundle.starts_with("com.apple.") { "系统应用" } else { "第三方应用" }).into(),
                                Value::Boolean(*placeholder),
                                Value::Nil,
                                format!("AppDomain-{bundle}").into(),
                                Value::Nil,
                                "Manifest.plist Applications".into(),
                            ],
                        )
                        .from(f)
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
                    let mut d = diag::DiagResult { bt_mac: None, battery_activation: None, battery_serial: None, battery_health: None, domain_sizes: Vec::new(), anchor_unix: anchor };
                    diag::bluetooth(run, f, &mut d)?;
                    n += 1;
                } else if lower == "com.apple.mobilebluetooth.ledevices.paired.db" {
                    n += backupdb::bluetooth(run, f, &all, true)?;
                } else if lower == "com.apple.mobilebluetooth.ledevices.other.db" {
                    n += backupdb::bluetooth(run, f, &all, false)?;
                } else if lower == "com.apple.wifi.plist" || lower == "com.apple.wifi-private-mac-networks.plist" {
                    n += backupdb::wifi(run, f)?;
                } else if lower == "accounts3.sqlite" {
                    n += backupdb::account(run, f, &all)?;
                } else if lower == "comparablekeychainbackup.plist" {
                    n += pushkey::keychain_items(run, f)?;
                }
            }
            "power" => {
                if lower.starts_with("bdc_once") && lower.ends_with(".csv") {
                    let mut d = diag::DiagResult { bt_mac: None, battery_activation: None, battery_serial: None, battery_health: None, domain_sizes: Vec::new(), anchor_unix: anchor };
                    diag::bdc_once(run, f, &mut d)?;
                } else if lower == "batteryhealth.log" {
                    let mut d = diag::DiagResult { bt_mac: None, battery_activation: None, battery_serial: None, battery_health: None, domain_sizes: Vec::new(), anchor_unix: anchor };
                    diag::battery_health(run, f, &mut d)?;
                } else if lower == "com.apple.powerd.bdc.plist" {
                    let mut d = diag::DiagResult { bt_mac: None, battery_activation: None, battery_serial: None, battery_health: None, domain_sizes: Vec::new(), anchor_unix: anchor };
                    diag::bdc_plist(run, f, &mut d)?;
                } else if lower.ends_with(".plsql") {
                    let _ = powerlog::analyze_opts(run, f, &all, anchor, false)?;
                    n += 1;
                }
            }
            "media" => {
                if lower == "photos.sqlite" {
                    n += backupdb::photo(run, f, &all)?;
                } else if lower == "notestore.sqlite" {
                    n += backupdb::note(run, f, &all)?;
                }
            }
            _ => {}
        }
    }
    Ok(n)
}

pub fn ios_comm(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "comm")?;
    run.set_summary(format!("通讯信息 {n} 项相关记录"));
    Ok(())
}
pub fn ios_apps(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "appinfo")?;
    run.set_summary(format!("应用信息 {n} 项相关记录"));
    Ok(())
}
pub fn ios_connect(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "connect")?;
    run.set_summary(format!("设备连接 {n} 项相关记录"));
    Ok(())
}
pub fn ios_power(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "power")?;
    run.set_summary(format!("电池与屏幕 {n} 项相关记录"));
    Ok(())
}
pub fn ios_media(run: &Run) -> lovelycarve_plugin::Result<()> {
    let n = route_group(run, "media")?;
    run.set_summary(format!("媒体 {n} 项相关记录"));
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
        return Err(lovelycarve_plugin::Error::invalid_params(
            "未找到 .tracev3 文件（需要 sysdiagnose 的 system_logs.logarchive）",
        ));
    }
    let map = unifiedlog::load_timesync(&timesync_files);
    let kw = run.options().text("keyword").map(|s| s.to_string());
    let cap: usize = run
        .options()
        .text("limit")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(20000);
    let mut stats = unifiedlog::LogStats { total: 0, emitted: 0 };
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
        stats.total, stats.emitted, kw.unwrap_or_default(), cap
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
                    format!("{}{}", fmt_plus8(unix), if app.is_empty() { String::new() } else { format!(" → {app}") }).into(),
                    "SBLock 1→0 首次 + ScreenState".into(),
                ],
            )
            .emit()?;
        }
        if out.apps > 0 {
            run.record_values(
                "powerlog.overview",
                vec!["应用".into(), format!("AppVersions 去重 {} 个", out.apps).into(), f.name().to_string().into()],
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
        .analyzer("ios_apps", ios_apps)
        .analyzer("ios_connect", ios_connect)
        .analyzer("ios_power", ios_power)
        .analyzer("ios_media", ios_media)
        .analyzer("log_archive", log_archive)
        .analyzer("powerlog_files", powerlog_files)
}

export_plugin!(plugin);
