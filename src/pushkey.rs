//! 通知中心与钥匙串条目（明细级，不解密）。

use crate::util::ts_plus8;
use lovelycarve_plugin::{FileRef, Run, Value};
use plist::Value as PValue;
use std::time::{SystemTime, UNIX_EPOCH};

fn plist_ts(d: &plist::Date) -> Value {
    SystemTime::from(*d)
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|x| ts_plus8(x.as_secs() as i64))
        .map(|t| Value::from(t.as_str().to_string()))
        .unwrap_or(Value::Nil)
}

/// 通知：DeliveredNotifications.plist（数组，元素含 alarm/bundleid/title/body）。
pub fn notifications(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = f.read_all(32 << 20)?;
    let Ok(v) = PValue::from_reader(std::io::Cursor::new(&bytes)) else {
        run.warn_file(f, "PLIST_PARSE", "通知 plist 解析失败")?;
        return Ok(0);
    };
    let Some(arr) = v.as_array() else {
        return Ok(0);
    };
    let mut n = 0usize;
    for item in arr {
        let d = item.as_dictionary();
        let get = |k: &str| {
            d.and_then(|x| x.get(k))
                .and_then(PValue::as_string)
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        // 时间在 alarm/date 字段（Date 类型）
        let time = d
            .and_then(|x| x.get("alarm").or_else(|| x.get("date")))
            .and_then(PValue::as_date)
            .map(|d| plist_ts(&d))
            .unwrap_or(Value::Nil);
        run.record_values(
            "ios.push",
            vec![
                time,
                get("app").into(),
                get("title").into(),
                get("subtitle").into(),
                get("body").into(),
                get("bundleid").into(),
            ],
        )
        .from(f)
        .emit()?;
        n += 1;
    }
    if n == 0 {
        run.warn_file(f, "PUSH_EMPTY", "通知 plist 无条目")?;
    }
    Ok(n)
}

/// 钥匙串条目清单：ComparableKeychainBackup.plist（备份导出结构，明细不做解密）。
pub fn keychain_items(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = f.read_all(64 << 20)?;
    let Ok(v) = PValue::from_reader(std::io::Cursor::new(&bytes)) else {
        run.warn_file(f, "PLIST_PARSE", "钥匙串 plist 解析失败")?;
        return Ok(0);
    };
    let mut n = 0usize;
    // 结构防御式遍历：寻找含 agrp/svce/acct 类键的字典数组
    fn walk(run: &Run, f: &FileRef, v: &PValue, n: &mut usize) -> lovelycarve_plugin::Result<()> {
        match v {
            PValue::Array(a) => {
                for x in a {
                    walk(run, f, x, n)?;
                }
            }
            PValue::Dictionary(d) => {
                let has = ["acct", "svce", "agrp", "Server", "Account"].iter().any(|k| d.contains_key(*k));
                if has {
                    let get = |ks: &[&str]| -> String {
                        for k in ks {
                            if let Some(s) = d.get(*k).and_then(PValue::as_string) {
                                return s.to_string();
                            }
                        }
                        String::new()
                    };
                    let data_len = d
                        .get("v_Data")
                        .and_then(PValue::as_data)
                        .map(|b| b.len())
                        .unwrap_or(0);
                    let created = d.get("crat").or_else(|| d.get("CreationDate")).and_then(PValue::as_date);
                    let modified = d.get("mdat").or_else(|| d.get("ModDate")).and_then(PValue::as_date);
                    run.record_values(
                        "ios.keychain_item",
                        vec![
                            get(&["acct", "Account"]).into(),
                            get(&["svce", "Service", "Server"]).into(),
                            get(&["agrp", "AccessGroup"]).into(),
                            created.map(|d| plist_ts(&d)).unwrap_or(Value::Nil),
                            modified.map(|d| plist_ts(&d)).unwrap_or(Value::Nil),
                            (data_len as i64).into(),
                            "明细来自备份导出；数据未解密".into(),
                        ],
                    )
                    .from(f)
                    .emit()?;
                    *n += 1;
                } else {
                    for x in d.values() {
                        walk(run, f, x, n)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(run, f, &v, &mut n)?;
    if n == 0 {
        run.warn_file(f, "KEYCHAIN_EMPTY", "钥匙串 plist 未识别出条目")?;
    }
    Ok(n)
}
