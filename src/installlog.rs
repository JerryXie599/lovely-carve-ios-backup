//! mobile_installation.log 解析：安装请求（Placeholder/Customer）、安装成功、
//! 卸载请求、容器销毁。行首时间为本地墙钟（时区未知 → local 时间戳，不进时间线）。
//! 行格式：`Wed Aug 12 10:07:21 2026 [323] <notice> ...`

use crate::util::ts_local;
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct InstallEvent {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub action: &'static str,
    pub bundle: String,
    pub app_name: String,
    pub version: String,
    pub detail: String,
    pub requested_by: String,
}

/// bundle → 最早“安装请求”事件（任意类型），供应用清单与 finding 使用。
pub type FirstInstall = HashMap<String, InstallEvent>;

fn parse_stamp(line: &str) -> Option<(i32, u32, u32, u32, u32, u32)> {
    // "Wed Aug 12 10:07:21 2026 [..."
    let mut it = line.split(' ');
    let _wd = it.next()?;
    let mon = match it.next()? {
        "Jan" => 1, "Feb" => 2, "Mar" => 3, "Apr" => 4, "May" => 5, "Jun" => 6,
        "Jul" => 7, "Aug" => 8, "Sep" => 9, "Oct" => 10, "Nov" => 11, "Dec" => 12,
        _ => return None,
    };
    let day: u32 = it.next()?.parse().ok()?;
    let hms = it.next()?;
    let hms: Vec<&str> = hms.split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let year: i32 = it.next()?.parse().ok()?;
    Some((year, mon, day, hms[0].parse().ok()?, hms[1].parse().ok()?, hms[2].parse().ok()?))
}

/// 从 `Installing <MIInstallableParallelPlaceholder ID=com.x; Version=(null), ShortVersion=3.8.0>` 提取。
fn extract_id_seg(line: &str) -> Option<(String, String)> {
    let start = line.find("ID=")?;
    let rest = &line[start + 3..];
    let end = rest.find(';')?;
    let bundle = rest[..end].to_string();
    let ver = rest[end..]
        .find("ShortVersion=")
        .map(|i| {
            let v = &rest[end + i + 13..];
            let v = v.split([',', '>']).next().unwrap_or(v);
            v.trim_end_matches(')').to_string()
        })
        .unwrap_or_default();
    Some((bundle, ver))
}

fn after_quote_path(line: &str) -> Option<String> {
    let start = line.find("Install of \"")? + 12;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn app_display_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).trim_end_matches(".app").to_string()
}

/// 解析一个 mobile_installation.log 文件，输出 ios.install_event 记录。
/// 返回 bundle → 最早安装请求。
pub fn parse(run: &Run, file: &FileRef) -> lovelycarve_plugin::Result<FirstInstall> {
    let lines = file.read_lines(64 << 20)?;
    let mut events: Vec<InstallEvent> = Vec::new();
    // Install of 行暂存：秒级时间 → (路径, 类型, requested_by)，等待紧随的 Installing ID= 行补 bundle
    let mut pending: Option<(usize, String, &'static str, String)> = None;
    let mut first: FirstInstall = HashMap::new();

    for line in &lines {
        let Some((y, mo, d, h, mi, s)) = parse_stamp(line) else { continue };
        let idx = events.len();
        if let Some(rest_start) = line.find("]: ") {
            let msg = &line[rest_start + 3..];
            if msg.starts_with("Install of \"") {
                if let Some(path) = after_quote_path(msg) {
                    let typ = if msg.contains("type Placeholder") {
                        "安装请求（Placeholder 占位）"
                    } else {
                        "安装请求（Customer 正式）"
                    };
                    let by = msg
                        .find("requested by ")
                        .map(|i| msg[i + 13..].split(' ').next().unwrap_or("").to_string())
                        .unwrap_or_default();
                    pending = Some((idx, path.clone(), typ, by.clone()));
                    events.push(InstallEvent {
                        year: y, month: mo, day: d, hour: h, minute: mi, second: s,
                        action: typ,
                        bundle: String::new(),
                        app_name: app_display_name(&path.clone()),
                        version: String::new(),
                        detail: path,
                        requested_by: by,
                    });
                }
            } else if msg.starts_with("Installing <MIInstallable") {
                if let Some((bundle, ver)) = extract_id_seg(msg) {
                    // 补写 pending 的 Install of 事件
                    if let Some((pidx, _, _, _)) = &pending {
                        if *pidx + 4 >= idx {
                            events[*pidx].bundle = bundle.clone();
                            events[*pidx].version = ver.clone();
                        }
                    }
                    pending = None;
                    events.push(InstallEvent {
                        year: y, month: mo, day: d, hour: h, minute: mi, second: s,
                        action: "开始安装",
                        bundle,
                        app_name: String::new(),
                        version: ver,
                        detail: msg.rsplit("]: ").next().unwrap_or(msg).chars().take(160).collect(),
                        requested_by: String::new(),
                    });
                }
            } else if msg.starts_with("Install Successful for (") {
                let seg = &msg["Install Successful for (".len()..];
                let seg = seg.split(')').next().unwrap_or("");
                let (kind, bundle) = seg.split_once(':').unwrap_or(("", seg));
                events.push(InstallEvent {
                    year: y, month: mo, day: d, hour: h, minute: mi, second: s,
                    action: if kind.contains("Placeholder") { "安装成功（占位）" } else { "安装成功（正式）" },
                    bundle: bundle.to_string(),
                    app_name: String::new(),
                    version: String::new(),
                    detail: String::new(),
                    requested_by: String::new(),
                });
            } else if msg.starts_with("Uninstall requested by ") {
                if let Some(i) = msg.find("for identifier ") {
                    let bundle = msg[i + 15..].split(' ').next().unwrap_or("").to_string();
                    events.push(InstallEvent {
                        year: y, month: mo, day: d, hour: h, minute: mi, second: s,
                        action: "卸载请求",
                        bundle,
                        app_name: String::new(),
                        version: String::new(),
                        detail: String::new(),
                        requested_by: msg["Uninstall requested by ".len()..]
                            .split(' ')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            } else if msg.starts_with("Destroying container with identifier ") {
                let bundle = msg["Destroying container with identifier ".len()..]
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string();
                events.push(InstallEvent {
                    year: y, month: mo, day: d, hour: h, minute: mi, second: s,
                    action: "销毁容器",
                    bundle,
                    app_name: String::new(),
                    version: String::new(),
                    detail: String::new(),
                    requested_by: String::new(),
                });
            }
        }
    }

    for e in &events {
        run.check_cancelled()?;
        let ts: Option<Timestamp> = ts_local(e.year, e.month, e.day, e.hour, e.minute, e.second);
        if e.action.contains("安装请求") && !e.bundle.is_empty() {
            let key = format!("{:04}{:02}{:02}{:02}{:02}{:02}", e.year, e.month, e.day, e.hour, e.minute, e.second);
            let better = first
                .get(&e.bundle)
                .map(|old| {
                    let oldkey = format!(
                        "{:04}{:02}{:02}{:02}{:02}{:02}",
                        old.year, old.month, old.day, old.hour, old.minute, old.second
                    );
                    key < oldkey
                })
                .unwrap_or(true);
            if better {
                first.insert(e.bundle.clone(), e.clone());
            }
        }
        run.record_values(
            "ios.install_event",
            vec![
                (ts.as_ref()).map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil),
                e.action.into(),
                if e.bundle.is_empty() { e.app_name.clone().into() } else { e.bundle.clone().into() },
                e.app_name.clone().into(),
                e.version.clone().into(),
                e.detail.clone().into(),
                e.requested_by.clone().into(),
            ],
        )
        .from(file)
        .note(format!("{} {:02}:{:02}:{:02}", e.action, e.hour, e.minute, e.second))
        .emit()?;
    }
    if events.is_empty() {
        run.warn_file(file, "INSTALL_LOG_EMPTY", "未识别到任何安装/卸载事件")?;
    }
    Ok(first)
}
