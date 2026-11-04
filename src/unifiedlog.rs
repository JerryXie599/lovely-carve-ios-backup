//! macOS/iOS 统一日志（Unified Log / tracev3）解析，基于 macos-unifiedlogs（Apache-2.0）。
//! 输入：logarchive 的 *.tracev3 与 *.timesync；uuidtext/dsc 字符串表经 LovelyCarve 视图按需读取。
//! 由于日志量可达数十万条，输出默认仅保留 Error/Fault 级别；提供 keyword 选项过滤。

use lovelycarve_plugin::{FileRef, Run, Value};
use macos_unifiedlogs::{
    cache::MemoryStringCache,
    dsc::SharedCacheStrings,
    filesystem::LocalFile,
    timesync::TimesyncBoot,
    traits::{FileProvider, SourceFile},
    unified_log::LogData,
    uuidtext::UUIDText,
};
use std::collections::HashMap;

/// 以 LovelyCarve 视图为后端的字符串源。
struct ViewProvider<'a> {
    run: &'a Run,
    base: String, // logarchive 根（视图内路径，无尾斜杠）
}

impl<'a> ViewProvider<'a> {
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        let view = self.run.view()?;
        view.read(&format!("{}/{}", self.base, rel), 64 << 20).ok().flatten()
    }
}

fn normalize_uuid(uuid: &str) -> Option<String> {
    let u = if uuid.len() == 30 {
        format!("00{uuid}")
    } else if uuid.len() == 31 {
        format!("0{uuid}")
    } else if uuid.len() == 32 {
        uuid.to_string()
    } else {
        return None;
    };
    Some(u)
}

impl<'a> FileProvider for ViewProvider<'a> {
    fn tracev3_files(&self) -> impl Iterator<Item = impl SourceFile> {
        std::iter::empty::<LocalFile>()
    }
    fn uuidtext_files(&self) -> impl Iterator<Item = impl SourceFile> {
        std::iter::empty::<LocalFile>()
    }
    fn read_uuidtext(&self, uuid: &str) -> std::io::Result<UUIDText> {
        let Some(u) = normalize_uuid(uuid) else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "uuid len"));
        };
        // logarchive 内 uuidtext 目录平铺：<根>/<前2字符>/<后30字符>
        let rel = format!("{}/{}", &u[0..2], &u[2..]);
        match self.read(&rel) {
            Some(buf) => UUIDText::parse_uuidtext(&buf)
                .map(|(_, v)| v)
                .map_err(|e| std::io::Error::other(format!("uuidtext parse: {e:?}"))),
            None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, rel)),
        }
    }
    fn dsc_files(&self) -> impl Iterator<Item = impl SourceFile> {
        std::iter::empty::<LocalFile>()
    }
    fn read_dsc_uuid(&self, uuid: &str) -> std::io::Result<SharedCacheStrings> {
        let Some(u) = normalize_uuid(uuid) else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "uuid len"));
        };
        let rel = format!("dsc/{}", &u);
        match self.read(&rel) {
            Some(buf) => SharedCacheStrings::parse_dsc(&buf)
                .map(|(_, v)| v)
                .map_err(|e| std::io::Error::other(format!("dsc parse: {e:?}"))),
            None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, rel)),
        }
    }
    fn timesync_files(&self) -> impl Iterator<Item = impl SourceFile> {
        std::iter::empty::<LocalFile>()
    }
}

/// 从 tracev3 输入的视图路径推 logarchive 根（截掉 `Persist|Special|Signpost/xxx.tracev3`）。
fn archive_base(view_path: &str) -> Option<String> {
    let p = view_path.trim_end_matches('/');
    let idx = p.rfind(|c| c == '/')?;
    let (dir, file) = (&p[..idx], &p[idx + 1..]);
    if !file.ends_with(".tracev3") {
        return None;
    }
    if matches!(dir.rsplit('/').next().unwrap_or(""), "Persist" | "Special" | "Signpost" | "HighVolume" | "Trace") {
        let cut = dir.rfind('/')?;
        return Some(dir[..cut].to_string());
    }
    Some(dir.to_string())
}

pub struct LogStats {
    pub total: usize,
    pub emitted: usize,
}

/// 解析一个 tracev3 文件，产出 ios.log_event 记录。
/// keyword 为 None/空 → 仅输出 Error/Fault 级；非空 → 匹配 message/process/subsystem/category。
pub fn parse_one(
    run: &Run,
    f: &FileRef,
    timesync: &HashMap<String, macos_unifiedlogs::timesync::TimesyncBoot>,
    keyword: Option<&str>,
    cap: usize,
    stats: &mut LogStats,
) -> lovelycarve_plugin::Result<()> {
    let view_path = f.view_path().unwrap_or(f.name()).to_string();
    let Some(base) = archive_base(&view_path) else {
        run.warn_file(f, "LOG_PATH", "无法从路径推断 logarchive 根")?;
        return Ok(());
    };
    let provider = ViewProvider { run, base };
    let buf = f.read_all(256 << 20)?;
    let (_, unified) = LogData::parse_unified_log(&buf, &f.name()).map_err(|e| {
        lovelycarve_plugin::Error::new("TRACEV3", format!("{}: {e:?}", f.name()))
    })?;
    let cache = MemoryStringCache::default();
    let (logs, _missing) = LogData::build_log(&unified, &provider, &cache, timesync, true);
    for entry in logs {
        run.check_cancelled()?;
        stats.total += 1;
        let kw = keyword.unwrap_or("");
        let pass = if kw.is_empty() {
            matches!(entry.log_type, macos_unifiedlogs::unified_log::LogType::Error | macos_unifiedlogs::unified_log::LogType::Fault)
        } else {
            entry.message.contains(kw)
                || entry.process.contains(kw)
                || entry.subsystem.contains(kw)
                || entry.category.contains(kw)
        };
        if !pass {
            continue;
        }
        if stats.emitted >= cap {
            return Ok(());
        }
        let ts = entry
            .timestamp
            .split('.')
            .next()
            .map(|s| s.replace(' ', "T"))
            .unwrap_or_default();
        run.record_values(
            "ios.log_event",
            vec![
                Value::from(ts),
                entry.process.clone().into(),
                entry.pid.into(),
                entry.thread_id.into(),
                entry.subsystem.clone().into(),
                entry.category.clone().into(),
                format!("{:?}", entry.event_type).into(),
                format!("{:?}", entry.log_type).into(),
                entry.message.clone().into(),
            ],
        )
        .from(f)
        .emit()?;
        stats.emitted += 1;
    }
    Ok(())
}

/// 合并全部 .timesync 输入。
pub fn load_timesync(files: &[FileRef]) -> HashMap<String, macos_unifiedlogs::timesync::TimesyncBoot> {
    let mut map = HashMap::new();
    for f in files {
        if let Ok(buf) = f.read_all(16 << 20) {
            if let Ok((_, m)) = TimesyncBoot::parse_timesync_data(&buf) {
                map.extend(m);
            }
        }
    }
    map
}
