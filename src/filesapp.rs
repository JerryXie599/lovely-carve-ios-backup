//! 「文件」App（com.apple.DocumentsApp）与 iCloud Drive 本地存储清单。
//!
//! 覆盖三类数据源：文件 App 的 File Provider 本地存储、DocumentsApp 容器、
//! CloudDocs（iCloud Drive）会话目录。输出 `ios.app_media`（名称/路径/大小），
//! 与媒体元数据（media_exif）互补，构成"文件 App 里有什么"的完整视图。

use lovelycarve_plugin::{FileRef, Run, Value};

/// 路径是否属于「文件」App / iCloud Drive 的存储范围。
pub fn matches(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("/file provider storage/")
        || p.contains("group.com.apple.fileprovider.localstorage")
        || p.contains("com.apple.documentsapp")
        || p.contains("/clouddocs/")
}

/// 单文件清单记录。
pub fn scan(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    run.check_cancelled()?;
    let path = f.view_path().unwrap_or(f.name());
    let app = if path.to_lowercase().contains("/clouddocs/") {
        "iCloud Drive (CloudDocs)"
    } else {
        "文件 (com.apple.DocumentsApp)"
    };
    // 修改时间：经 view.stat 取节点 mtime（UTC RFC3339 → UTC+8），缺失留空。
    let time = run
        .require_view()
        .ok()
        .and_then(|view| view.stat(path).ok().flatten())
        .and_then(|node| node.timestamps.modified)
        .and_then(|m| chrono::DateTime::parse_from_rfc3339(&m).ok())
        .map(|dt| {
            (dt + chrono::Duration::hours(8))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .and_then(|t| lovelycarve_plugin::Timestamp::parse(&t))
        .map(|t| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil);
    run.record_values(
        "ios.app_media",
        vec![
            app.into(),
            f.name().into(),
            path.into(),
            Value::from(f.size()),
            time,
        ],
    )
    .source(crate::util::node_source(f))
    .emit()?;
    Ok(1)
}
