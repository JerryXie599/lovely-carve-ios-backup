//! 时间与格式化工具。iOS 取证里三种时间基准并存：
//! - Manifest.plist 的 plist Date（Apple epoch 2001-01-01 UTC，plist crate 换算）
//! - powerlog 的 `timestamp` 列：基准不确定（本检材实测为 Unix 秒，历史库为 Apple epoch），
//!   由 [`Epoch::detect`] 自动判别
//! - 日志行首 / CSV 里的本地墙钟时间：时区未知，输出为 local（不进时间线）

use lovelycarve_plugin::Timestamp;

pub const APPLE_EPOCH: i64 = 978_307_200; // 2001-01-01T00:00:00Z
pub const BEIJING: i32 = 8 * 60;

/// powerlog `timestamp` 列的换算基准。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Epoch {
    Unix,
    Apple,
}

impl Epoch {
    pub fn to_unix(self, raw: f64) -> i64 {
        match self {
            Epoch::Unix => raw as i64,
            Epoch::Apple => APPLE_EPOCH + raw as i64,
        }
    }

    /// 用日志库自身的取值范围判别基准：`anchor_unix` 是检材可信时间
    /// （sysdiagnose 目录名等）。中值换算落在 [2007-01-01, anchor+2年] 视为合理。
    pub fn detect(samples: &[f64], anchor_unix: i64) -> Option<Epoch> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = sorted[sorted.len() / 2];
        let lo = 1_169_094_400i64; // 2007-01-01
        let hi = anchor_unix + 2 * 365 * 86_400;
        for epoch in [Epoch::Unix, Epoch::Apple] {
            let u = epoch.to_unix(mid);
            if (lo..=hi).contains(&u) {
                return Some(epoch);
            }
        }
        None
    }
}

/// Unix 秒 → UTC+8 RFC3339（带偏移，进时间线）。
pub fn ts_plus8(unix: i64) -> Option<Timestamp> {
    civil(unix + 8 * 3600).map(|(y, mo, d, h, mi, s)| Timestamp::with_offset(y, mo, d, h, mi, s, BEIJING).unwrap())
}

/// Unix 秒 → UTC RFC3339。
pub fn ts_utc(unix: i64) -> Timestamp {
    Timestamp::from_unix(unix)
}

/// 本地墙钟时间（时区未知，不进时间线）。
pub fn ts_local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<Timestamp> {
    Timestamp::local(y, mo, d, h, mi, s)
}

pub fn civil(unix: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    chrono::DateTime::from_timestamp(unix, 0).map(|dt| {
        use chrono::{Datelike, Timelike};
        (dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second())
    })
}

/// Unix 秒 → "YYYY-MM-DD HH:MM:SS"（UTC+8 墙钟展示用）。
pub fn fmt_plus8(unix: i64) -> String {
    civil(unix + 8 * 3600)
        .map(|(y, mo, d, h, mi, s)| format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
        .unwrap_or_else(|| format!("<invalid {unix}>"))
}

pub fn day_plus8(unix: i64) -> String {
    civil(unix + 8 * 3600)
        .map(|(y, mo, d, _h, _mi, _s)| format!("{y:04}-{mo:02}-{d:02}"))
        .unwrap_or_default()
}

pub fn fmt_secs(sec: f64) -> String {
    if sec.is_finite() && sec >= 0.0 {
        format!("{sec:.0}")
    } else {
        String::new()
    }
}

pub fn fmt_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// 常见 ProductType → 营销型号（iPhone 主要型号）。
pub fn iphone_model(product_type: &str) -> Option<&'static str> {
    Some(match product_type {
        "iPhone1,1" => "iPhone",
        "iPhone1,2" => "iPhone 3G",
        "iPhone2,1" => "iPhone 3GS",
        "iPhone3,1" | "iPhone3,2" | "iPhone3,3" => "iPhone 4",
        "iPhone4,1" => "iPhone 4S",
        "iPhone5,1" | "iPhone5,2" => "iPhone 5",
        "iPhone5,3" | "iPhone5,4" => "iPhone 5c",
        "iPhone6,1" | "iPhone6,2" => "iPhone 5s",
        "iPhone7,1" => "iPhone 6 Plus",
        "iPhone7,2" => "iPhone 6",
        "iPhone8,1" => "iPhone 6s",
        "iPhone8,2" => "iPhone 6s Plus",
        "iPhone8,4" => "iPhone SE (1st gen)",
        "iPhone9,1" | "iPhone9,3" => "iPhone 7",
        "iPhone9,2" | "iPhone9,4" => "iPhone 7 Plus",
        "iPhone10,1" | "iPhone10,4" => "iPhone 8",
        "iPhone10,2" | "iPhone10,5" => "iPhone 8 Plus",
        "iPhone10,3" | "iPhone10,6" => "iPhone X",
        "iPhone11,2" => "iPhone XS",
        "iPhone11,4" | "iPhone11,6" => "iPhone XS Max",
        "iPhone11,8" => "iPhone XR",
        "iPhone12,1" => "iPhone 11",
        "iPhone12,3" => "iPhone 11 Pro",
        "iPhone12,5" => "iPhone 11 Pro Max",
        "iPhone12,8" => "iPhone SE (2nd gen)",
        "iPhone13,1" => "iPhone 12 mini",
        "iPhone13,2" => "iPhone 12",
        "iPhone13,3" => "iPhone 12 Pro",
        "iPhone13,4" => "iPhone 12 Pro Max",
        "iPhone14,2" => "iPhone 13 Pro",
        "iPhone14,3" => "iPhone 13 Pro Max",
        "iPhone14,4" => "iPhone 13 mini",
        "iPhone14,5" => "iPhone 13",
        "iPhone14,6" => "iPhone SE (3rd gen)",
        "iPhone14,7" => "iPhone 14",
        "iPhone14,8" => "iPhone 14 Plus",
        "iPhone15,2" => "iPhone 14 Pro",
        "iPhone15,3" => "iPhone 14 Pro Max",
        "iPhone15,4" => "iPhone 15",
        "iPhone15,5" => "iPhone 15 Plus",
        "iPhone16,1" => "iPhone 15 Pro",
        "iPhone16,2" => "iPhone 15 Pro Max",
        _ => return None,
    })
}
