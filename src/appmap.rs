//! iOS 应用名映射：内嵌的 bundle id → 应用中文名/分类码对照表（10 万余条）。
//! 首次查询时从内嵌 SQLite 载入内存，并支持运行时通过进程名交叉验证动态登记。

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

static MAP: OnceLock<HashMap<String, (String, String)>> = OnceLock::new();
static DYNAMIC: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn dynamic_map() -> &'static RwLock<HashMap<String, String>> {
    DYNAMIC.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 运行时动态登记（如从 DataUsage.sqlite 进程名解析出的 bundle -> app_name）
pub fn register_dynamic(bundle: &str, name: &str) {
    if bundle.is_empty() || name.is_empty() {
        return;
    }
    if let Ok(mut map) = dynamic_map().write() {
        map.insert(bundle.to_string(), name.to_string());
    }
}

pub fn dynamic_name(bundle: &str) -> Option<String> {
    dynamic_map()
        .read()
        .ok()
        .and_then(|map| map.get(bundle).cloned())
}

fn load() -> &'static HashMap<String, (String, String)> {
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        let Ok(tmp) = tempfile::tempdir() else {
            return map;
        };
        let path = tmp.path().join("appmap.sqlite");
        if std::fs::write(&path, include_bytes!("../assets/ios_app_map.sqlite")).is_err() {
            return map;
        }
        if let Ok(conn) =
            rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        {
            if let Ok(mut stmt) = conn.prepare("SELECT packageName, appName, appType FROM app_map")
            {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(row)) = rows.next() {
                        if let (Ok(Some(pkg)), Ok(name)) = (
                            row.get::<_, Option<String>>(0),
                            row.get::<_, Option<String>>(1),
                        ) {
                            if !pkg.is_empty() {
                                let t = row
                                    .get::<_, Option<String>>(2)
                                    .ok()
                                    .flatten()
                                    .unwrap_or_default();
                                map.insert(pkg, (name.unwrap_or_default(), t));
                            }
                        }
                    }
                }
            }
        }
        map
    })
}

/// bundle id → (应用名, 分类码)。分类码为数据源自带的应用类别编码，语义未全部公开，
/// 输出时保留原码（如 06=系统内置、07=办公协同、14=购物、99=通讯加密类）。
pub fn lookup(bundle: &str) -> Option<&'static (String, String)> {
    load().get(bundle)
}

pub fn name(bundle: &str) -> Option<&'static str> {
    lookup(bundle).map(|(n, _)| n.as_str())
}

/// 优先查询动态交叉验证的进程名，其次回退到内置静态字典。
pub fn resolve_name(bundle: &str) -> Option<String> {
    if let Some(dyn_n) = dynamic_name(bundle) {
        if !dyn_n.trim().is_empty() {
            return Some(dyn_n);
        }
    }
    name(bundle).map(|s| s.to_string())
}
