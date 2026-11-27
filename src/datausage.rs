//! DataUsage.sqlite 流量使用与进程记录深度解析
//! 从 ZPROCESS 与 ZLIVEUSAGE 提取每个进程与 App 的网络通信流量，
//! 并提取进程名交叉验证补充应用真实名称（如 com.quora.app.Experts -> Poe）。

use crate::backupdb::{open_db, ts_or_nil};
use crate::util::{ts_plus8, APPLE_EPOCH};
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use std::collections::HashMap;

const SYSTEM_DAEMONS: &[&str] = &[
    "mdnsresponder",
    "storekitd",
    "promotedcontentd",
    "cloudd",
    "geod",
    "nsurlsessiond",
    "trustd",
    "com.apple.webkit.networking",
    "mediaserverd",
    "appstored",
    "identityservicesd",
    "akd",
    "apsd",
    "commcenter",
    "gamed",
    "bluetoothd",
    "fitcored",
    "watchlistd",
    "mobileactivationd",
    "familycircled",
    "ind",
    "itunesstored",
    "bookassetd",
    "remindd",
    "cloudphotod",
    "maild",
    "mapspushd",
    "coreidvd",
    "triald",
    "sharingd",
    "translationd",
    "seld",
    "mediaanalysisd",
    "devicecheckd",
    "storekituiservice",
    "appstorecomponentsd",
    "asd",
    "duetexpertd",
    "siri.embeddedspeech",
    "avconferenced",
    "clipserviced",
    "com.apple.safari.safebrowsing.se",
    "exchangesyncd",
    "_personalhotspot_",
    "passwordbreachd",
    "biomesyncd",
    "commcentermobilehelper",
    "healthd",
    "bookdatastored",
    "timed",
    "networkserviceproxy",
    "securityd",
    "searchd",
    "swcd",
    "parsec-fbf",
    "softwareupdateservicesd",
    "callservicesd",
    "tipsd",
    "fmfd",
    "dataaccessd",
    "searchpartyd",
    "ndoagent",
    "mediasetupd",
    "cloudpaird",
    "accountsd",
    "atc",
    "videosubscriptionsd",
    "protectedcloudstorage.protectedcloudkeysyncing",
    "backupd",
    "homed",
    "mobileassetd",
    "parsecd",
    "findmydeviced",
    "softwareupdated",
    "transparencyd",
    "locationd",
];

fn is_uuid_string(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

fn is_extension_or_widget(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("extension")
        || lower.contains("widget")
        || lower.contains("notification")
        || lower.contains("service")
        || lower.contains("intent")
        || lower.contains("shareextension")
}

fn score_candidate(candidate: &str) -> i32 {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || is_uuid_string(trimmed) {
        return 0;
    }
    let lower = trimmed.to_ascii_lowercase();
    if SYSTEM_DAEMONS.contains(&lower.as_str()) {
        return 0;
    }
    if lower.ends_with('d') && SYSTEM_DAEMONS.iter().any(|d| d.ends_with(&lower)) {
        return 0;
    }
    if is_extension_or_widget(trimmed) {
        return 1;
    }
    if matches!(lower.as_str(), "runner" | "app" | "application")
        || lower.ends_with(" runner")
        || lower.ends_with(" app")
        || lower.ends_with(" application")
    {
        return 2;
    }
    3
}

fn apple_time(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 {
        (v / 1e9) as i64
    } else {
        v as i64
    };
    ts_plus8(APPLE_EPOCH + secs)
}

/// 从 DataUsage.sqlite 的 ZPROCESS 表中反查出每个 bundle id 的真实进程应用名。
pub fn extract_process_app_map(f: &FileRef, sidecars: &[FileRef]) -> HashMap<String, String> {
    let mut out: HashMap<String, (i32, String)> = HashMap::new();
    let Ok((_tmp, conn)) = open_db(f, sidecars) else {
        return HashMap::new();
    };

    let sql = "SELECT ZBUNDLENAME, ZPROCNAME FROM ZPROCESS WHERE ZBUNDLENAME IS NOT NULL AND ZPROCNAME IS NOT NULL";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return HashMap::new();
    };

    let Ok(mut rows) = stmt.query([]) else {
        return HashMap::new();
    };

    while let Ok(Some(row)) = rows.next() {
        let Ok(bundle) = row.get::<_, String>(0) else {
            continue;
        };
        let Ok(proc_name) = row.get::<_, String>(1) else {
            continue;
        };

        let candidate = proc_name.split('/').next().unwrap_or("").trim();
        let score = score_candidate(candidate);
        if score > 0 {
            let current = out.get(&bundle);
            if current.is_none() || score > current.unwrap().0 {
                out.insert(bundle, (score, candidate.to_string()));
            }
        }
    }

    out.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

/// 解析 DataUsage.sqlite 流量使用与进程运行记录，输出 ios.data_usage 记录。
pub fn parse_data_usage(
    run: &Run,
    f: &FileRef,
    sidecars: &[FileRef],
    app_map: &HashMap<String, String>,
) -> lovelycarve_plugin::Result<usize> {
    let Ok((_tmp, conn)) = open_db(f, sidecars) else {
        return Ok(0);
    };

    let sql = r#"
        SELECT 
            p.Z_PK,
            p.ZBUNDLENAME,
            p.ZPROCNAME,
            p.ZFIRSTTIMESTAMP,
            COALESCE(u.ZTIMESTAMP, p.ZTIMESTAMP) AS rec_time,
            COALESCE(u.ZWIFIIN, 0.0),
            COALESCE(u.ZWIFIOUT, 0.0),
            COALESCE(u.ZWWANIN, 0.0),
            COALESCE(u.ZWWANOUT, 0.0),
            COALESCE(u.ZKIND, 0)
        FROM ZPROCESS p
        LEFT JOIN ZLIVEUSAGE u ON p.Z_PK = u.ZHASPROCESS
        ORDER BY rec_time DESC
    "#;

    let Ok(mut stmt) = conn.prepare(sql) else {
        return Ok(0);
    };

    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };

    let mut n = 0usize;
    while let Ok(Some(row)) = rows.next() {
        run.check_cancelled()?;

        let bundle = row
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        let proc_name = row
            .get::<_, Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_default();
        let first_ts = row.get::<_, Option<f64>>(3).ok().flatten();
        let rec_ts = row.get::<_, Option<f64>>(4).ok().flatten();
        let wifi_in = row.get::<_, Option<f64>>(5).ok().flatten().unwrap_or(0.0) as i64;
        let wifi_out = row.get::<_, Option<f64>>(6).ok().flatten().unwrap_or(0.0) as i64;
        let wwan_in = row.get::<_, Option<f64>>(7).ok().flatten().unwrap_or(0.0) as i64;
        let wwan_out = row.get::<_, Option<f64>>(8).ok().flatten().unwrap_or(0.0) as i64;
        let kind = row.get::<_, Option<i64>>(9).ok().flatten().unwrap_or(0);

        // 确定展示的应用名称：优先 app_map > 内置字典 > 进程名前缀 > bundle id
        let app_name = app_map
            .get(&bundle)
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| crate::appmap::resolve_name(&bundle))
            .or_else(|| {
                let candidate = proc_name.split('/').next().unwrap_or("").trim();
                if score_candidate(candidate) > 0 {
                    Some(candidate.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                if !bundle.is_empty() {
                    bundle.clone()
                } else {
                    proc_name.clone()
                }
            });

        let start_time = first_ts.and_then(apple_time);
        let time = rec_ts.and_then(apple_time);

        run.record_values(
            "ios.data_usage",
            vec![
                app_name.into(),
                bundle.into(),
                proc_name.into(),
                ts_or_nil(start_time),
                ts_or_nil(time),
                Value::from(wifi_in),
                Value::from(wifi_out),
                Value::from(wwan_in),
                Value::from(wwan_out),
                Value::from(kind),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;

        n += 1;
    }

    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_candidate() {
        assert_eq!(score_candidate("Poe"), 3);
        assert_eq!(score_candidate("Telegram"), 3);
        assert_eq!(score_candidate("WhatsApp"), 3);
        assert_eq!(score_candidate("mDNSResponder"), 0);
        assert_eq!(score_candidate("cloudd"), 0);
        assert_eq!(score_candidate("PoeNotificationService"), 1);
        assert_eq!(score_candidate("Flutter Runner"), 2);
    }
}
