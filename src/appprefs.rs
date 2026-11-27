//! 应用容器偏好设置中的通用元数据：版本号、安装时间。
//!
//! 许多 App 在 `Library/Preferences/<bundle>.plist` 写入 `ds-app-version`、
//! `ds-app-install-date`、`mc_freshinstall_time` 等通用键；本模块只提取这些
//! 跨应用通用的键，输出 `ios.app` 记录补充 Manifest 清单。

use crate::appmap;
use crate::util::ts_plus8;
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use plist::Value as PValue;

/// plist::Date（UTC 绝对时间）→ UTC+8 Timestamp。
fn plist_date(d: plist::Date) -> Option<Timestamp> {
    let unix = std::time::SystemTime::from(d)
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    ts_plus8(unix)
}

/// 字典按键名取值（先精确、后忽略大小写）。
fn get_key<'a>(d: &'a plist::Dictionary, key: &str) -> Option<&'a PValue> {
    d.get(key).or_else(|| {
        d.iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    })
}

/// 字符串/数值统一转文本，空串视为缺失。
fn value_text(v: Option<&PValue>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_string() {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(i) = v.as_signed_integer() {
        return Some(i.to_string());
    }
    v.as_real().filter(|x| x.is_finite()).map(|x| x.to_string())
}

/// version_history：字符串数组取最后一项；字典按最大 build 取值（非数字键忽略）。
fn history_version(v: &PValue) -> Option<String> {
    if let Some(arr) = v.as_array() {
        return arr.iter().rev().find_map(|x| value_text(Some(x)));
    }
    if let Some(d) = v.as_dictionary() {
        let mut best: Option<(u64, String)> = None;
        for (k, val) in d {
            let Ok(build) = k.trim().parse::<u64>() else {
                continue;
            };
            let Some(text) = value_text(Some(val)) else {
                continue;
            };
            if best.as_ref().map(|(b, _)| build > *b).unwrap_or(true) {
                best = Some((build, text));
            }
        }
        return best.map(|(_, text)| text);
    }
    value_text(Some(v))
}

/// 解析单个应用 Preferences plist；`containers` 为 bundle → 检材内数据目录。
pub fn scan(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(32 << 20) {
        Ok(b) => b,
        Err(e) => {
            run.warn_file(f, "READ", format!("偏好设置读取失败：{e}"))?;
            return Ok(0);
        }
    };
    // bundle = 文件名去 .plist；只处理含通用键的 plist。
    let name = f.name();
    let lower = name.to_lowercase();
    let plower_path = f.view_path().unwrap_or(f.name()).to_lowercase();
    let bundle = if lower.ends_with(".plist") {
        &name[..name.len() - ".plist".len()]
    } else {
        name
    };
    if bundle.is_empty() {
        return Ok(0);
    }
    let Some(v) = crate::util::read_plist(&bytes) else {
        // 个别偏好 plist 含越界日期导致解析失败：退化为原始字符串提取，
        // 至少保留账号/JID/邮箱等通用标识。
        run.warn_file(f, "PLIST_PARSE", "应用偏好设置解析失败，改为原始字符串提取")?;
        return raw_identifiers(run, f, bundle, &bytes);
    };
    let Some(d) = v.as_dictionary() else {
        return Ok(0);
    };

    // 「照片」「文件」/ iCloud Drive 容器：导出全部顶层标量配置键（做题指路与状态核验）。
    let cfg_app = if bundle == "com.apple.mobileslideshow" {
        Some("照片 (com.apple.mobileslideshow)")
    } else if lower.contains("documentsapp") {
        Some("文件 (com.apple.DocumentsApp)")
    } else if plower_path.contains("/clouddocs/") {
        Some("iCloud Drive (CloudDocs)")
    } else {
        None
    };
    if let Some(cfg_app) = cfg_app {
        return dump_config(run, f, cfg_app, d);
    }

    // 版本号/安装时间改由 lib.rs 预扫描（pref_meta）汇总进应用清单，
    // 这里不再单独发 ios.app（避免同一应用两行版本号口径不一致）。
    Ok(0)
}

/// Preferences 文件名 → 归一化 bundle：去掉 group. 前缀与 .shared 后缀，
/// 使组容器配置能对回应用主包名（group.net.whatsapp.WhatsApp.shared → net.whatsapp.WhatsApp）。
pub fn bundle_from_stem(stem: &str) -> String {
    let mut b = stem.trim().to_string();
    if let Some(rest) = b.strip_prefix("group.") {
        b = rest.to_string();
    }
    if let Some(rest) = b.strip_suffix(".shared") {
        b = rest.to_string();
    }
    b
}

/// 读取应用配置文件里的版本号与安装时间（跨应用通用键 + WhatsApp SPClientVersion）。
pub fn pref_meta(
    run: &Run,
    f: &FileRef,
) -> lovelycarve_plugin::Result<Option<(String, Option<Timestamp>)>> {
    let bytes = match f.read_all(32 << 20) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let Some(v) = crate::util::read_plist(&bytes) else {
        return Ok(None);
    };
    let Some(d) = v.as_dictionary() else {
        return Ok(None);
    };
    let version = get_key(d, "SPClientVersion")
        .and_then(|x| value_text(Some(x)))
        .or_else(|| get_key(d, "ds-app-version").and_then(|x| value_text(Some(x))))
        .or_else(|| get_key(d, "mc_app_version").and_then(|x| value_text(Some(x))))
        .or_else(|| get_key(d, "version_history").and_then(history_version));
    let install = ["ds-app-install-date", "mc_freshinstall_time"]
        .iter()
        .find_map(|k| get_key(d, k).and_then(|x| x.as_date()).and_then(plist_date));
    match version {
        Some(v) if !v.trim().is_empty() => Ok(Some((v, install))),
        _ => Ok(None),
    }
}

/// 解析失败时的兜底：从原始字节中提取 JID / 邮箱等账号标识。
fn raw_identifiers(
    run: &Run,
    f: &FileRef,
    bundle: &str,
    bytes: &[u8],
) -> lovelycarve_plugin::Result<usize> {
    let text = String::from_utf8_lossy(bytes);
    let mut seen = std::collections::HashSet::new();
    let mut n = 0usize;
    for token in text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '%' | '+' | '-'))
    }) {
        if token.len() < 6 || token.len() > 96 || !token.contains('@') {
            continue;
        }
        let (kind, ok) = if token.ends_with("@s.whatsapp.net")
            || token.ends_with("@g.us")
            || token.ends_with("@c.us")
            || token.ends_with("@lid")
        {
            ("即时通讯ID", true)
        } else {
            let mut parts = token.split('@');
            let local = parts.next().unwrap_or("");
            let domain = parts.next().unwrap_or("");
            (
                "邮箱",
                !local.is_empty() && domain.contains('.') && !domain.starts_with('.'),
            )
        };
        if !ok || !seen.insert(token.to_string()) {
            continue;
        }
        run.record_values(
            "ios.app_artifact",
            vec![
                appmap::name(bundle).unwrap_or(bundle).to_string().into(),
                kind.into(),
                token.to_string().into(),
                f.view_path().unwrap_or(f.name()).to_string().into(),
                "偏好设置解析失败，原始字符串提取".into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
        if n >= 50 {
            break;
        }
    }
    Ok(n)
}

/// 顶层标量键值导出（字符串/数值/布尔/日期），数组与字典折叠为摘要。
fn dump_config(
    run: &Run,
    f: &FileRef,
    app: &str,
    d: &plist::Dictionary,
) -> lovelycarve_plugin::Result<usize> {
    let mut n = 0usize;
    for (k, v) in d {
        if n >= 300 {
            break;
        }
        let text = match v {
            PValue::Boolean(b) => b.to_string(),
            PValue::Real(x) => x.to_string(),
            PValue::Integer(x) => x.to_string(),
            PValue::Date(dt) => std::time::SystemTime::from(*dt)
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|u| crate::util::ts_plus8(u.as_secs() as i64))
                .map(|t| t.as_str().to_string())
                .unwrap_or_default(),
            PValue::String(sv) => {
                let sv = sv.trim();
                if sv.is_empty() {
                    continue;
                }
                sv.chars().take(500).collect::<String>()
            }
            PValue::Array(a) => format!("[{} 项]", a.len()),
            PValue::Dictionary(dd) => format!("{{{} 键}}", dd.len()),
            _ => continue,
        };
        run.record_values(
            "ios.app_config",
            vec![app.into(), k.clone().into(), text.into(), f.name().into()],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 配置文件值里的账号标识（JID / 邮箱等）—— WhatsApp 自身 ID 等散落在
/// 组容器偏好里，通用提取供做题引用。
pub fn pref_identifiers(f: &FileRef) -> Vec<String> {
    let bytes = match f.read_all(32 << 20) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let Some(v) = crate::util::read_plist(&bytes) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    fn walk(
        v: &plist::Value,
        out: &mut Vec<String>,
        seen: &mut std::collections::HashSet<String>,
        depth: usize,
    ) {
        if out.len() >= 20 || depth > 12 {
            return;
        }
        match v {
            plist::Value::String(s) => {
                let s = s.trim();
                let ok = (s.ends_with("@s.whatsapp.net")
                    || s.ends_with("@g.us")
                    || s.ends_with("@c.us")
                    || s.ends_with("@lid")
                    || (s.contains('@') && {
                        let domain = s.rsplit('@').next().unwrap_or("");
                        domain.contains('.') && !domain.starts_with('.') && s.len() > 6
                    }))
                    && !s.contains(' ');
                if ok && seen.insert(s.to_string()) {
                    out.push(s.chars().take(200).collect());
                }
            }
            plist::Value::Array(a) => {
                for x in a {
                    walk(x, out, seen, depth + 1);
                }
            }
            plist::Value::Dictionary(d) => {
                for x in d.values() {
                    walk(x, out, seen, depth + 1);
                }
            }
            _ => {}
        }
    }
    walk(&v, &mut found, &mut seen, 0);
    found
}
