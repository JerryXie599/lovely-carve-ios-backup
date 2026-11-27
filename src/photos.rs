//! Photos.sqlite 完整解析（分类 `fphotos`，独立于媒体信息栏）。
//!
//! 覆盖 Photos 库的全部分析面：
//! - `ios.photos_asset`：资产明细（拍摄/导入时间、尺寸、UTI、方向、位置、
//!   拍摄设备、云端状态、收藏/隐藏/删除标记、播放/分享/浏览计数、描述、UUID）。
//! - `ios.photos_album`：相册（用户相册 / 智能分类 / 媒体类型，含资产数与
//!   云端所有者）。
//! - `ios.photos_album_asset`：相册成员关系（相册 → 资产）。
//! - `ios.photos_moment`：回忆时刻（标题、地点名、起止时间、资产数）。
//! - `ios.photos_person`：人物聚类（显示名、脸数、是否确认）。
//! - `ios.photos_scene`：场景识别标签与命中资产。
//! - `ios.photos_cloud`：iCloud 云端资产（云 GUID、删除状态、本地状态）。
//! - `ios.photos_analysis`：资产分析状态（场景数、人脸数、分析时间戳）。
//!
//! 兼容 iOS 17 起的布局差异：相册表在 iOS 17 为 `ZGENERICALBUM`（更早为
//! `ZALBUM`），资产相册成员关系走 `Z_28ASSETS` 中间表。

use std::collections::{BTreeMap, HashMap, HashSet};

use lovelycarve_plugin::{FileRef, Run, Severity, SourceRef, Value};
use rusqlite::Connection;

use crate::backupdb::{
    apple, apple_precise, cols, find_sidecar, open_db, pick, read_file_bytes, table_exists,
    ts_or_nil,
};
use crate::photos_enums::{self, scene_name, unsigned_scene_id};
use crate::sqlite_wal::{self, SqlValue, WalFrameStatus};

/// 场景识别表（iOS 17+ 资产级分类）。
const SCENE_TABLE: &str = "ZSCENECLASSIFICATION";

/// 根据 ZASSET.ZDIRECTORY 与 ZFILENAME 在检材视图中定位真实媒体文件。
/// 返回 (检材内相对路径字符串, Option<FileRef>)。
fn resolve_media_path(run: &Run, directory: &str, filename: &str) -> (String, Option<FileRef>) {
    let dir = directory.trim_matches('/');
    let name = filename.trim_matches('/');
    if name.is_empty() {
        return (String::new(), None);
    }
    let rel = if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    };
    let candidates = [
        rel.clone(),
        format!("Media/{rel}"),
        format!("HomeDomain/Media/{rel}"),
        format!("CameraRollDomain/Media/{rel}"),
        format!("private/var/mobile/Media/{rel}"),
        format!("var/mobile/Media/{rel}"),
    ];
    if let Some(view) = run.view() {
        for cand in &candidates {
            if let Ok(Some(fref)) = view.open(cand) {
                let p = fref.view_path().unwrap_or(cand).to_string();
                return (p, Some(fref));
            }
        }
    }
    (rel, None)
}

/// 相册成员中间表（资产 → 相册）。
const ALBUM_ASSETS_TABLE: &str = "Z_28ASSETS";

/// 相册 → 资产 中间表名（不同 iOS 版本命名不同）。
const ALBUM_TABLE_CANDIDATES: &[&str] = &["ZGENERICALBUM", "ZALBUM"];

/// 反向地理编码（NSKeyedArchiver 归档的 CLPlacemark）里常见的地点键。
const PLACE_KEYS: &[&str] = &[
    "kCLPlacemarkCountryName",
    "kCLPlacemarkState",
    "kCLPlacemarkCity",
    "kCLPlacemarkSubLocality",
    "kCLPlacemarkName",
];

/// 解析入口：Photos.sqlite → 各类记录。
pub fn scan(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = match open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(f, "DB_OPEN", format!("照片库：Photos.sqlite 打开失败：{e}"))?;
            return Ok(0);
        }
    };
    if !table_exists(&conn, "ZASSET") {
        run.warn_file(f, "NO_TABLE", "Photos.sqlite 缺少 ZASSET 表")?;
        return Ok(0);
    }

    let mut n = 0usize;
    let tomb_by_pk = collect_tombstone_briefs(&conn);
    let (wal_n, recovery) = wal_analysis(run, f, sides, &conn, &tomb_by_pk)?;
    n += wal_n;
    n += assets(run, f, &conn)?;
    let (album_table, album_col) = album_layout(&conn);
    n += albums(run, f, &conn, album_table, album_col)?;
    n += album_assets(run, f, &conn, album_table, album_col)?;
    n += moments(run, f, &conn)?;
    n += persons(run, f, &conn)?;
    n += scenes(run, f, &conn)?;
    n += cloud(run, f, &conn)?;
    n += analysis(run, f, &conn)?;
    n += tombstones(run, f, &conn, &recovery)?;
    n += qrcodes(run, f, &conn)?;
    n += share_fields(run, f, sides)?;
    Ok(n)
}

/// 相册表名与「相册→资产」列名（iOS 17 用 ZGENERICALBUM + Z_28ASSETS）。
fn album_layout(conn: &Connection) -> (&'static str, &'static str) {
    for table in ALBUM_TABLE_CANDIDATES {
        if table_exists(conn, table) {
            let c = cols(conn, table);
            // 成员关系列：ZGENERICALBUM 用 ZASSETS / ZALBUM 用 ZASSET。
            let col = if c.iter().any(|x| x == "ZASSETS") {
                "ZASSETS"
            } else if c.iter().any(|x| x == "ZASSET") {
                "ZASSET"
            } else {
                "ZASSETS"
            };
            return (table, col);
        }
    }
    (ALBUM_TABLE_CANDIDATES[0], "ZASSETS")
}

fn text(row: &rusqlite::Row<'_>, idx: usize) -> Option<String> {
    row.get::<_, Option<String>>(idx).ok().flatten()
}

fn int(row: &rusqlite::Row<'_>, idx: usize) -> Option<i64> {
    row.get::<_, Option<i64>>(idx).ok().flatten()
}

fn real(row: &rusqlite::Row<'_>, idx: usize) -> Option<f64> {
    row.get::<_, Option<f64>>(idx).ok().flatten()
}

/// 资产明细（`ios.photos_asset`）。
fn assets(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let zc = cols(conn, "ZASSET");
    let z = |want: &[&str]| -> String {
        match pick(&zc, want) {
            Some(c) => format!("z.[{c}]"),
            None => "NULL".into(),
        }
    };
    let has_attr = table_exists(conn, "ZADDITIONALASSETATTRIBUTES")
        && cols(conn, "ZADDITIONALASSETATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ac = if has_attr {
        cols(conn, "ZADDITIONALASSETATTRIBUTES")
    } else {
        Vec::new()
    };
    let a = |want: &[&str]| -> String {
        if !has_attr {
            return "NULL".into();
        }
        match pick(&ac, want) {
            Some(c) => format!("a.[{c}]"),
            None => "NULL".into(),
        }
    };
    let has_ext = table_exists(conn, "ZEXTENDEDATTRIBUTES")
        && cols(conn, "ZEXTENDEDATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ec = if has_ext {
        cols(conn, "ZEXTENDEDATTRIBUTES")
    } else {
        Vec::new()
    };
    let e = |want: &[&str]| -> String {
        if !has_ext {
            return "NULL".into();
        }
        match pick(&ec, want) {
            Some(c) => format!("e.[{c}]"),
            None => "NULL".into(),
        }
    };
    let has_cm = table_exists(conn, "ZCLOUDMASTER") && zc.iter().any(|x| x == "ZMASTER");
    let cmc = if has_cm {
        cols(conn, "ZCLOUDMASTER")
    } else {
        Vec::new()
    };
    let cm = |want: &[&str]| -> String {
        if !has_cm {
            return "NULL".into();
        }
        match pick(&cmc, want) {
            Some(c) => format!("cm.[{c}]"),
            None => "NULL".into(),
        }
    };
    let joins = format!(
        "{}{}{}",
        if has_attr {
            " LEFT JOIN [ZADDITIONALASSETATTRIBUTES] a ON a.[ZASSET] = z.[Z_PK]"
        } else {
            ""
        },
        if has_ext {
            " LEFT JOIN [ZEXTENDEDATTRIBUTES] e ON e.[ZASSET] = z.[Z_PK]"
        } else {
            ""
        },
        if has_cm {
            " LEFT JOIN [ZCLOUDMASTER] cm ON cm.[Z_PK] = z.[ZMASTER]"
        } else {
            ""
        }
    );
    let columns: Vec<String> = vec![
        z(&["Z_PK"]),
        z(&["ZFILENAME"]),
        z(&["ZDIRECTORY"]),
        z(&["ZUNIFORMTYPEIDENTIFIER"]),
        z(&["ZWIDTH"]),
        z(&["ZHEIGHT"]),
        z(&["ZDURATION"]),
        z(&["ZDATECREATED"]),
        z(&["ZADDEDDATE"]),
        z(&["ZMODIFICATIONDATE"]),
        z(&["ZFAVORITE"]),
        z(&["ZHIDDEN"]),
        z(&["ZTRASHEDSTATE"]),
        z(&["ZTRASHEDDATE"]),
        z(&["ZDELETEREASON"]),
        z(&["ZCLOUDLOCALSTATE"]),
        z(&["ZORIENTATION"]),
        z(&["ZLATITUDE"]),
        z(&["ZLONGITUDE"]),
        z(&["ZUUID"]),
        z(&["ZTRASHEDREASON"]),
        // 附加属性
        a(&["ZORIGINALFILENAME"]),
        a(&["ZIMPORTEDBYBUNDLEIDENTIFIER"]),
        a(&["ZIMPORTEDBYDISPLAYNAME"]),
        a(&["ZSHARECOUNT"]),
        a(&["ZPLAYCOUNT"]),
        a(&["ZVIEWCOUNT"]),
        a(&["ZACCESSIBILITYDESCRIPTION"]),
        a(&["ZTITLE"]),
        a(&["ZTIMEZONENAME"]),
        a(&["ZGPSHORIZONTALACCURACY"]),
        // 扩展属性（EXIF）
        e(&["ZCAMERAMAKE"]),
        e(&["ZCAMERAMODEL"]),
        e(&["ZLENSMODEL"]),
        e(&["ZFOCALLENGTH"]),
        e(&["ZAPERTURE"]),
        e(&["ZSHUTTERSPEED"]),
        e(&["ZISO"]),
        e(&["ZFLASHFIRED"]),
        e(&["ZHDRTYPE"]),
        // 状态与来源枚举字段
        z(&["ZKIND"]),
        z(&["ZKINDSUBTYPE"]),
        z(&["ZSAVEDASSETTYPE"]),
        z(&["ZVISIBILITYSTATE"]),
        a(&["ZIMPORTEDBY"]),
        // 定位八元组（64 字节，含篡改鉴定）
        z(&["ZLOCATIONDATA"]),
        // 原始 EXIF 坐标（与 ZLOCATIONDATA 比对可判断手动改位置）
        e(&["ZLATITUDE"]),
        e(&["ZLONGITUDE"]),
        // 反向地理编码（街道 / 城市 / 完整地址）
        a(&["ZREVERSELOCATIONDATA"]),
        // 原始文件字节大小、云端主唯一标识符与 EXIF 时间字符串
        a(&["ZORIGINALFILESIZE"]),
        a(&["ZORIGINALSTABLEHASH"]),
        z(&["ZCLOUDASSETGUID"]),
        cm(&["ZCLOUDMASTERGUID"]),
        a(&["ZEXIFTIMESTAMPSTRING"]),
        a(&["ZTIMEZONEOFFSET"]),
    ];
    let sql = format!("SELECT {} FROM [ZASSET] z{joins}", columns.join(", "));
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let pk = int(&r, 0).unwrap_or(0);
        let Some(filename) = text(&r, 1).filter(|s| !s.is_empty()) else {
            continue;
        };
        let directory = text(&r, 2).unwrap_or_default();
        let uti = text(&r, 3).unwrap_or_default();
        let width = int(&r, 4).unwrap_or(0);
        let height = int(&r, 5).unwrap_or(0);
        let duration = real(&r, 6).unwrap_or(0.0);
        let created = real(&r, 7);
        let added = real(&r, 8);
        let modified = real(&r, 9);
        let favorite = int(&r, 10).unwrap_or(0);
        let hidden = int(&r, 11).unwrap_or(0);
        let trashed = int(&r, 12).unwrap_or(0);
        let trashed_date = real(&r, 13);
        let delete_reason = text(&r, 14).unwrap_or_default();
        let cloud_state = int(&r, 15).unwrap_or(-1);
        let orientation = int(&r, 16).unwrap_or(0);
        let lat = real(&r, 17).unwrap_or(f64::NAN);
        let lng = real(&r, 18).unwrap_or(f64::NAN);
        let uuid = text(&r, 19).unwrap_or_default();
        let trashed_reason_code =
            int(&r, 20).or_else(|| text(&r, 20).and_then(|s| s.parse::<i64>().ok()));
        let orig_name = text(&r, 21).unwrap_or_default();
        let src_bundle = text(&r, 22).unwrap_or_default();
        let src_name = text(&r, 23).unwrap_or_default();
        let share_count = int(&r, 24).unwrap_or(0);
        let play_count = int(&r, 25).unwrap_or(0);
        let view_count = int(&r, 26).unwrap_or(0);
        let description = text(&r, 27).unwrap_or_default();
        let title = text(&r, 28).unwrap_or_default();
        let tz_name = text(&r, 29).unwrap_or_default();
        let gps_accuracy = real(&r, 30);
        let cam_make = text(&r, 31).unwrap_or_default();
        let cam_model = text(&r, 32).unwrap_or_default();
        let lens = text(&r, 33).unwrap_or_default();
        let focal = text(&r, 34).unwrap_or_default();
        let aperture = text(&r, 35).unwrap_or_default();
        let shutter = text(&r, 36).unwrap_or_default();
        let iso = text(&r, 37).unwrap_or_default();
        let flash = int(&r, 38).unwrap_or(0);
        let hdr = int(&r, 39).unwrap_or(0);
        let kind_code = int(&r, 40).unwrap_or(-1);
        let subtype_code = int(&r, 41).unwrap_or(-1);
        let saved_type = int(&r, 42).unwrap_or(-1);
        let visibility = int(&r, 43).unwrap_or(-1);
        let imported_by = int(&r, 44).unwrap_or(-1);
        let location_blob = r.get::<_, Option<Vec<u8>>>(45).ok().flatten();
        let exif_lat = real(&r, 46);
        let exif_lng = real(&r, 47);
        let revgeo_blob = r.get::<_, Option<Vec<u8>>>(48).ok().flatten();
        let orig_filesize = int(&r, 49).filter(|&v| v > 0);
        let orig_stable_hash = text(&r, 50).unwrap_or_default();
        let cloud_asset_guid = text(&r, 51).unwrap_or_default();
        let cloud_master_guid_col = text(&r, 52).unwrap_or_default();
        let exif_time_str = text(&r, 53).unwrap_or_default();
        let tz_offset_str = int(&r, 54)
            .map(|s| {
                let sign = if s >= 0 { '+' } else { '-' };
                let abs = s.abs();
                format!("UTC{sign}{:02}:{:02} ({s}s)", abs / 3600, (abs % 3600) / 60)
            })
            .unwrap_or_default();
        let cloud_master_guid = if !cloud_master_guid_col.is_empty() {
            cloud_master_guid_col
        } else {
            orig_stable_hash
        };
        let _ = (description, title, gps_accuracy);

        // 媒体类型：以 ZKIND/ZKINDSUBTYPE 为权威（UTI 仅作补充）。
        let kind_label = photos_enums::enum_label(photos_enums::ASSET_KIND, kind_code);
        let subtype_label =
            photos_enums::enum_label(photos_enums::ASSET_KIND_SUBTYPE, subtype_code);
        let media_type = match (kind_label.as_str(), subtype_label.as_str()) {
            ("视频", _) => "视频".to_string(),
            (_, "实况照片") => "实况照片".to_string(),
            (_, "系统截屏") => "系统截屏".to_string(),
            (_, "屏幕录制") => "屏幕录制".to_string(),
            (_, "全景照片") => "全景照片".to_string(),
            (_, "慢动作") => "慢动作".to_string(),
            (_, "延时摄影") => "延时摄影".to_string(),
            (_, "电影效果") => "电影效果".to_string(),
            (_, "空间视频") => "空间视频".to_string(),
            ("照片", _) => "照片".to_string(),
            ("音频", _) => "音频".to_string(),
            _ => {
                if duration > 0.5 {
                    "视频".to_string()
                } else if uti.contains("heic") {
                    "HEIC 照片".to_string()
                } else if uti.is_empty() {
                    "未知".to_string()
                } else {
                    "其他".to_string()
                }
            }
        };
        // 子类型单独列（截屏/实况/录屏是取证关键形态）。
        let subtype = if subtype_label.is_empty() {
            String::new()
        } else {
            subtype_label
        };
        // 来源：ZIMPORTEDBY 枚举（区分本机拍摄 / 第三方 App 保存 / AirDrop 接收）。
        let imported_label = photos_enums::enum_label(photos_enums::IMPORTED_BY, imported_by);
        let source_label = if !imported_label.is_empty() {
            imported_label
        } else if !src_name.is_empty() {
            src_name.clone()
        } else {
            src_bundle.clone()
        };
        let saved_label = photos_enums::enum_label(photos_enums::ASSET_SAVED_TYPE, saved_type);
        let visibility_label = photos_enums::enum_label(photos_enums::VISIBILITY_STATE, visibility);
        // 拍摄设备：EXIF 型号优先，其次扩展属性里的原始设备描述。
        let camera = if !cam_model.is_empty() {
            if cam_make.is_empty() {
                cam_model.clone()
            } else {
                format!("{} {}", cam_make.trim(), cam_model.trim())
            }
        } else {
            String::new()
        };
        let source_app = if !src_name.is_empty() {
            src_name
        } else if !src_bundle.is_empty() {
            src_bundle.clone()
        } else {
            String::new()
        };
        // 云端状态（0=本地、1=已上传、2=仅云端等，取值按 iCloud 语义归纳）。
        let cloud = match cloud_state {
            0 => "仅本地",
            1 => "已上传 iCloud",
            2 => "已在 iCloud（未下载）",
            3 => "上传中",
            _ => "",
        };
        let location = if lat.is_finite() && lat.abs() <= 90.0 && lng.abs() <= 180.0 {
            format!("{lat:.6}, {lng:.6}")
        } else {
            String::new()
        };
        // ZLOCATIONDATA：64 字节 = 8 个 f64（纬度/经度/海拔/水平精度/垂直精度/
        // 航向/速度/采样时刻）。手动改位置的签名：海拔=0.0 且水平精度=1.0 且
        // 垂直精度=-1.0（航向/速度同时为 -1.0）。
        let loc = location_blob.as_deref().and_then(decode_location_data);
        let tampered = loc.map(|l| l.is_tampered()).unwrap_or(false);
        let loc_detail = match &loc {
            Some(l) => {
                let mut parts = vec![format!("海拔 {:.1}m", l.altitude)];
                if l.horiz_acc >= 0.0 {
                    parts.push(format!("水平精度 {:.1}m", l.horiz_acc));
                }
                if l.vert_acc != -1.0 {
                    parts.push(format!("垂直精度 {:.1}m", l.vert_acc));
                }
                if l.speed > 0.0 {
                    parts.push(format!("速度 {:.2}m/s", l.speed));
                }
                parts.join("；")
            }
            None => String::new(),
        };
        // EXIF 原坐标与系统坐标的差（>0.0001° ≈ 11m 说明位置被改过）。
        let exif_diff = if let (Some(l), el, eg) = (&loc, exif_lat, exif_lng) {
            if el.is_some_and(|v| v.is_finite())
                && eg.is_some_and(|v| v.is_finite())
                && el.unwrap_or(0.0).abs() <= 90.0
                && eg.unwrap_or(0.0).abs() <= 180.0
            {
                let d = ((el.unwrap_or(0.0) - l.latitude).powi(2)
                    + (eg.unwrap_or(0.0) - l.longitude).powi(2))
                .sqrt();
                if d > 0.0001 {
                    Some(d)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        // 反向地理编码：Apple GeoServices 的 PLRevGeoLocationInfo 归档。
        let revgeo = revgeo_blob
            .as_deref()
            .and_then(reverse_geo_address)
            .unwrap_or_default();
        let _ = (saved_label, visibility_label, loc_detail);
        let location_flag = if tampered {
            "疑似手动修改位置".to_string()
        } else if let Some(d) = exif_diff {
            format!("坐标与 EXIF 原始值相差 {d:.6}°")
        } else {
            String::new()
        };
        // 删除状态（ZTRASHEDSTATE：0 正常、1 已移入最近删除、2 已永久移除）。
        let trashed_reason_text = format_trashed_reason(trashed_reason_code, trashed);
        let deleted = if trashed != 0 {
            match (trashed, trashed_reason_code) {
                (1, Some(1)) => "在「最近删除」回收站（用户主动删除）".to_owned(),
                (1, Some(2)) => "在「最近删除」回收站（iCloud 同步移除）".to_owned(),
                (1, _) => "在「最近删除」回收站".to_owned(),
                (2, _) => "已从回收站永久移除".to_owned(),
                _ => photos_enums::enum_label(photos_enums::TRASHED_STATE, trashed),
            }
        } else {
            delete_reason_zh(&delete_reason)
        };
        let trash_delay = compute_trash_delay(created, trashed_date);

        let (asset_path_str, media_fref) = resolve_media_path(run, &directory, &filename);
        let severity = if !location_flag.is_empty() {
            Some(Severity::Critical)
        } else if hidden == 1 || !deleted.is_empty() {
            Some(Severity::Medium)
        } else if source_label.contains("AirDrop")
            || source_label.contains("保存")
            || media_type.contains("截屏")
        {
            Some(Severity::Info)
        } else {
            None
        };

        let values = vec![
            // 14 个核心列
            filename.clone().into(),
            orig_name.clone().into(),
            media_type.into(),
            subtype.into(),
            ts_or_nil(created.and_then(apple_precise)),
            ts_or_nil(added.and_then(apple_precise)),
            source_label.into(),
            source_app.into(),
            location_flag.into(),
            revgeo.into(),
            location.into(),
            (if hidden == 1 { "是" } else { "" }).into(),
            deleted.into(),
            asset_path_str.into(),
            // 38 个详情列
            directory.into(),
            ts_or_nil(modified.and_then(apple_precise)),
            (if exif_time_str.is_empty() {
                Value::Nil
            } else {
                Value::from(exif_time_str)
            }),
            tz_name.into(),
            (if tz_offset_str.is_empty() {
                Value::Nil
            } else {
                Value::from(tz_offset_str)
            }),
            (if exif_lat.is_some_and(|v| v.is_finite()) {
                Value::from(exif_lat.unwrap_or(0.0))
            } else {
                Value::Nil
            }),
            (if exif_lng.is_some_and(|v| v.is_finite()) {
                Value::from(exif_lng.unwrap_or(0.0))
            } else {
                Value::Nil
            }),
            cam_make.into(),
            cam_model.into(),
            lens.into(),
            (if focal.is_empty() {
                Value::Nil
            } else {
                Value::from(focal.parse::<f64>().unwrap_or(0.0))
            }),
            (if aperture.is_empty() {
                Value::Nil
            } else {
                Value::from(aperture.parse::<f64>().unwrap_or(0.0))
            }),
            shutter.into(),
            (if iso.is_empty() {
                Value::Nil
            } else {
                Value::from(iso.parse::<i64>().unwrap_or(0))
            }),
            (if flash == 1 { "是" } else { "" }).into(),
            (if hdr > 0 { "是" } else { "" }).into(),
            (if width > 0 {
                Value::from(width)
            } else {
                Value::Nil
            }),
            (if height > 0 {
                Value::from(height)
            } else {
                Value::Nil
            }),
            (if orientation > 0 {
                Value::from(orientation)
            } else {
                Value::Nil
            }),
            Value::from(if duration.is_finite() { duration } else { 0.0 }),
            orig_filesize.map_or(Value::Nil, Value::from), // filesize (Size)
            orig_filesize.map_or(Value::Nil, Value::from), // original_filesize (Int bytes)
            uti.into(),
            (if favorite == 1 { "是" } else { "" }).into(),
            ts_or_nil(trashed_date.and_then(apple_precise)),
            trashed_reason_text.into(),
            trash_delay.map_or(Value::Nil, Value::from),
            Value::Nil, // adjusted
            Value::Nil, // burst_uuid
            Value::Nil, // live_photo_uuid
            cloud.into(),
            (if cloud_asset_guid.is_empty() {
                Value::Nil
            } else {
                Value::from(cloud_asset_guid)
            }),
            (if cloud_master_guid.is_empty() {
                Value::Nil
            } else {
                Value::from(cloud_master_guid)
            }),
            Value::Nil, // shared_by
            Value::from(view_count),
            Value::from(play_count),
            Value::from(share_count),
            uuid.into(),
        ];

        let mut emit = run.record_values("ios.photos_asset", values);
        if let Some(sev) = severity {
            emit = emit.severity(sev);
        }
        let db_source = crate::util::node_source(f).note(format!("ZASSET Z_PK={pk}"));
        if let Some(mf) = media_fref {
            emit = emit
                .source(mf.source().note("原始媒体文件"))
                .source(db_source);
        } else {
            emit = emit.source(db_source);
        }
        emit.emit()?;
        n += 1;
    }
    Ok(n)
}

/// `ZASSET.ZLOCATIONDATA`：64 字节 = 8 个小端 f64。
#[derive(Debug, Clone, Copy)]
pub struct LocationData {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: f64,
    pub horiz_acc: f64,
    pub vert_acc: f64,
    pub course: f64,
    pub speed: f64,
    pub timestamp: f64,
}

impl LocationData {
    /// 手动修改位置的签名：海拔 0.0 + 水平精度 1.0 + 垂直精度 -1.0。
    pub fn is_tampered(&self) -> bool {
        self.altitude == 0.0 && self.horiz_acc == 1.0 && self.vert_acc == -1.0
    }
}

/// 解码 ZLOCATIONDATA（长度不符返回 None）。
pub fn decode_location_data(blob: &[u8]) -> Option<LocationData> {
    if blob.len() != 64 {
        return None;
    }
    let mut d = [0f64; 8];
    for (i, slot) in d.iter_mut().enumerate() {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&blob[i * 8..i * 8 + 8]);
        *slot = f64::from_le_bytes(buf);
    }
    Some(LocationData {
        latitude: d[0],
        longitude: d[1],
        altitude: d[2],
        horiz_acc: d[3],
        vert_acc: d[4],
        course: d[5],
        speed: d[6],
        timestamp: d[7],
    })
}

/// 相册（`ios.photos_album`）。
fn albums(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
    table: &str,
    _member_col: &str,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, table) {
        return Ok(0);
    }
    let c = cols(conn, table);
    let q = |want: &[&str]| -> String {
        match pick(&c, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM [{table}]",
        q(&["ZTITLE"]),
        q(&["ZKIND"]),
        q(&["ZCACHEDCOUNT"]),
        q(&["ZCACHEDPHOTOSCOUNT"]),
        q(&["ZCACHEDVIDEOSCOUNT"]),
        q(&["ZCREATIONDATE"]),
        q(&["ZTRASHEDSTATE"]),
        q(&["ZCLOUDOWNERFULLNAME"]),
        q(&["ZIMPORTEDBYBUNDLEIDENTIFIER"]),
        q(&["ZUUID"]),
        q(&["ZISOWNED"]),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let title = text(&r, 0).unwrap_or_default();
        let kind = int(&r, 1).unwrap_or(-1);
        let count = int(&r, 2).unwrap_or(0);
        let photos = int(&r, 3).unwrap_or(0);
        let videos = int(&r, 4).unwrap_or(0);
        let created = real(&r, 5);
        let trashed = int(&r, 6).unwrap_or(0);
        let owner = text(&r, 7).unwrap_or_default();
        let imported_by = text(&r, 8).unwrap_or_default();
        let uuid = text(&r, 9).unwrap_or_default();
        let _owned = int(&r, 10).unwrap_or(0);
        if title.is_empty() && count == 0 {
            continue;
        }
        let album_kind = crate::photos_enums::album_kind_label(kind);
        run.record_values(
            "ios.photos_album",
            vec![
                title.clone().into(),
                album_kind.into(),
                Value::from(count),
                Value::from(photos),
                Value::from(videos),
                ts_or_nil(created.and_then(apple)),
                (if trashed == 1 { "已删除" } else { "" }).into(),
                (if owner.is_empty() {
                    Value::Nil
                } else {
                    Value::from(owner)
                }),
                (if imported_by.is_empty() {
                    Value::Nil
                } else {
                    Value::from(imported_by)
                }),
                (if uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(uuid)
                }),
                Value::from(kind),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 相册成员关系（`ios.photos_album_asset`）：相册 → 资产。
fn album_assets(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
    album_table: &str,
    member_col: &str,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, ALBUM_ASSETS_TABLE) || !table_exists(conn, album_table) {
        return Ok(0);
    }
    let ac = cols(conn, ALBUM_ASSETS_TABLE);
    // iOS 17：Z_28ASSETS(Z_28ALBUMS → 相册, Z_3ASSETS → 资产)；旧版命名不同。
    let album_fk = match pick(&ac, &["Z_28ALBUMS", "Z_2ALBUMS", "Z_1ALBUMS", "ZALBUM"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    let asset_fk = match pick(&ac, &["Z_3ASSETS", "Z_28ASSETS", "Z_1ASSETS", "ZASSET"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    let album_cols = cols(conn, album_table);
    let title_col = match pick(&album_cols, &["ZTITLE"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    // 资产 id → 文件名
    let mut names: HashMap<i64, String> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT [Z_PK], [ZFILENAME] FROM [ZASSET]") {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(name))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    names.insert(pk, name);
                }
            }
        }
    }
    // 相册 id → 标题
    let mut titles: HashMap<i64, String> = HashMap::new();
    let sql = format!("SELECT [Z_PK], [{title_col}] FROM [{album_table}]");
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(title))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    titles.insert(pk, title);
                }
            }
        }
    }
    let sql = format!("SELECT [{album_fk}], [{asset_fk}] FROM [{ALBUM_ASSETS_TABLE}]");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let album_id = int(&r, 0);
        let asset_id = int(&r, 1);
        let (Some(album_id), Some(asset_id)) = (album_id, asset_id) else {
            continue;
        };
        let Some(title) = titles.get(&album_id) else {
            continue;
        };
        let Some(filename) = names.get(&asset_id) else {
            continue;
        };
        run.record_values(
            "ios.photos_album_asset",
            vec![title.clone().into(), filename.clone().into()],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    let _ = member_col;
    Ok(n)
}

/// 回忆时刻（`ios.photos_moment`）。
fn moments(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZMOMENT") {
        return Ok(0);
    }
    let c = cols(conn, "ZMOMENT");
    let q = |want: &[&str]| -> String {
        match pick(&c, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {} FROM [ZMOMENT]",
        q(&["ZTITLE"]),
        q(&["ZSUBTITLE"]),
        q(&["ZSTARTDATE"]),
        q(&["ZENDDATE"]),
        q(&["ZCACHEDCOUNT"]),
        q(&["ZCACHEDPHOTOSCOUNT"]),
        q(&["ZLOCALIZEDLOCATIONNAMES"]),
        q(&["ZTRASHEDSTATE"]),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let title = text(&r, 0).unwrap_or_default();
        let subtitle = text(&r, 1).unwrap_or_default();
        let start = real(&r, 2);
        let end = real(&r, 3);
        let count = int(&r, 4).unwrap_or(0);
        let photos = int(&r, 5).unwrap_or(0);
        let loc_blob = r.get::<_, Option<Vec<u8>>>(6).ok().flatten();
        let trashed = int(&r, 7).unwrap_or(0);
        if title.is_empty() && count == 0 {
            continue;
        }
        // 地点名：NSKeyedArchiver 归档的字符串数组。
        let places = loc_blob
            .as_deref()
            .map(extract_place_names)
            .unwrap_or_default()
            .into_iter()
            // NSKeyedArchiver 的类名/占位符不是地点
            .filter(|s| !s.starts_with('$') && !s.starts_with("NS") && s != "null" && s.len() >= 2)
            .collect::<Vec<_>>();
        run.record_values(
            "ios.photos_moment",
            vec![
                title.into(),
                (if subtitle.is_empty() {
                    Value::Nil
                } else {
                    Value::from(subtitle)
                }),
                ts_or_nil(start.and_then(apple)),
                ts_or_nil(end.and_then(apple)),
                Value::from(count),
                Value::from(photos),
                (if places.is_empty() {
                    Value::Nil
                } else {
                    Value::from(places.join("、"))
                }),
                (if trashed == 1 { "已删除" } else { "" }).into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// NSKeyedArchiver 归档（bplist）里的地点名：递归找 NSString / NSArray。
fn extract_place_names(bytes: &[u8]) -> Vec<String> {
    let Some(value) = crate::util::read_plist(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_strings(&value, &mut out, 0);
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    out
}

fn collect_strings(value: &plist::Value, out: &mut Vec<String>, depth: usize) {
    if depth > 12 {
        return;
    }
    match value {
        plist::Value::String(text) => {
            // NSKeyedArchiver 的字符串在 NS.string 键下；普通字符串也一并收。
            let t = text.trim();
            if !t.is_empty() && t.chars().count() <= 120 {
                out.push(t.to_string());
            }
        }
        plist::Value::Array(items) => {
            for item in items {
                collect_strings(item, out, depth + 1);
            }
        }
        plist::Value::Dictionary(dict) => {
            for (key, item) in dict.iter() {
                // NSKeyedArchiver 的 $objects 数组与 $top 是结构，跳过键名本身。
                if key == "$archiver" || key == "$version" {
                    continue;
                }
                collect_strings(item, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// 人物聚类（`ios.photos_person`）。
fn persons(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZPERSON") {
        return Ok(0);
    }
    let pc = cols(conn, "ZPERSON");
    let q = |want: &[&str]| -> String {
        match pick(&pc, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {} FROM [ZPERSON]",
        q(&["ZDISPLAYNAME"]),
        q(&["ZFULLNAME"]),
        q(&["ZCONTACTMATCHINGDICTIONARY"]),
        q(&["ZPERSONUUID"]),
        q(&["ZFACECOUNT"]),
        q(&["ZVERIFIEDTYPE"]),
        q(&["ZDETECTIONTYPE"]),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    // 人脸数与确认状态从 ZDETECTEDFACE 汇总。
    let mut face_counts: HashMap<i64, i64> = HashMap::new();
    if table_exists(conn, "ZDETECTEDFACE") {
        let fc = cols(conn, "ZDETECTEDFACE");
        let person_fk = match pick(&fc, &["ZPERSON"]) {
            Some(x) => x.to_string(),
            None => String::new(),
        };
        if !person_fk.is_empty() {
            let sql = format!(
                "SELECT [{person_fk}], COUNT(*) FROM [ZDETECTEDFACE] GROUP BY [{person_fk}]"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(pk)), Ok(Some(cnt))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                        {
                            face_counts.insert(pk, cnt);
                        }
                    }
                }
            }
        }
    }
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let display = text(&r, 0).unwrap_or_default();
        let full = text(&r, 1).unwrap_or_default();
        let dict_blob = r.get::<_, Option<Vec<u8>>>(2).ok().flatten();
        let uuid = text(&r, 3).unwrap_or_default();
        let face_count = int(&r, 4).unwrap_or(0);
        let verified = int(&r, 5).unwrap_or(0);
        let detection = int(&r, 6).unwrap_or(0);
        // 显示名优先：ZDISPLAYNAME → ZFULLNAME → 联系名片字典（NSKeyedArchiver）。
        let mut name = display.clone();
        if name.is_empty() {
            name = full.clone();
        }
        if name.is_empty() {
            if let Some(bytes) = dict_blob.as_deref() {
                let mut strings = Vec::new();
                collect_strings(
                    &crate::util::read_plist(bytes).unwrap_or(plist::Value::Boolean(false)),
                    &mut strings,
                    0,
                );
                strings.retain(|s| {
                    !s.is_empty() && s.len() <= 80 && !s.starts_with('$') && !s.contains(".com.")
                });
                if let Some(first) = strings.first() {
                    name = first.clone();
                }
            }
        }
        let total_faces = if face_count > 0 { face_count } else { 0 };
        if name.is_empty() && uuid.is_empty() {
            continue;
        }
        let d = crate::photos_enums::detection_type_label(detection);
        let v = crate::photos_enums::verified_type_label(verified);
        let person_type = format!("{d} · {v}");
        run.record_values(
            "ios.photos_person",
            vec![
                (if name.is_empty() {
                    format!("人物 {}", &uuid[..uuid.len().min(8)])
                } else {
                    name
                })
                .into(),
                Value::from(total_faces),
                person_type.into(),
                (if uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(uuid)
                }),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 场景识别（`ios.photos_scene`）：场景标签 → 命中资产。
///
/// 关键结构（取证手册卷二 §4.1/§4.2）：`ZSCENECLASSIFICATION` 的外键列
/// `ZASSETATTRIBUTES`（静态通道）与 `ZASSETATTRIBUTESFORTEMPORALSCENECLASSIFICATIONS`
/// （视频/实况时序通道）**都指向 `ZADDITIONALASSETATTRIBUTES.Z_PK`**，再由
/// `ZADDITIONALASSETATTRIBUTES.ZASSET` 指向资产——直连资产会错。
///
/// `ZCLASSIFICATIONTYPE` 为分类通道（iOS 17 起才有该列，iOS 16 全部为静态）：
/// 0=静态视觉场景、1=局部显著目标、2=视频时序视觉、3=音频（SoundAnalysis，
/// 实况照片/视频里录到的人声与环境音）、4~7=扩展分类器。
fn scenes(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, SCENE_TABLE) {
        return Ok(0);
    }
    let c = cols(conn, SCENE_TABLE);
    let q = |want: &[&str]| -> String {
        match pick(&c, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    // 资产侧的 FK 列：静态通道 / 时序通道（iOS 17 才有后者）。
    let static_fk = match pick(&c, &["ZASSETATTRIBUTES"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    let temporal_fk =
        pick(&c, &["ZASSETATTRIBUTESFORTEMPORALSCENECLASSIFICATIONS"]).map(|x| x.to_string());
    let has_channel = c.iter().any(|x| x == "ZCLASSIFICATIONTYPE");
    let has_time = c.iter().any(|x| x == "ZSTARTTIME");
    let has_bbox = c.iter().any(|x| x == "ZPACKEDBOUNDINGBOXRECT");

    // 资产属性 id → 资产 id（经 ZADDITIONALASSETATTRIBUTES.ZASSET）。
    let mut asset_of: HashMap<i64, i64> = HashMap::new();
    let mut asset_info: HashMap<i64, (String, String)> = HashMap::new(); // (filename, directory)
    if let Ok(mut stmt) = conn.prepare("SELECT [Z_PK], [ZASSET] FROM [ZADDITIONALASSETATTRIBUTES]")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(asset))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                {
                    asset_of.insert(pk, asset);
                }
            }
        }
    }
    let zc = cols(conn, "ZASSET");
    let dir_col = pick(&zc, &["ZDIRECTORY"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let fn_col = pick(&zc, &["ZFILENAME"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let sql = format!("SELECT [Z_PK], {fn_col}, {dir_col} FROM [ZASSET]");
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(name))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    let dir = r
                        .get::<_, Option<String>>(2)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    asset_info.insert(pk, (name, dir));
                }
            }
        }
    }

    let columns = vec![
        q(&["ZSCENEIDENTIFIER"]),
        q(&["ZCONFIDENCE"]),
        format!("[{static_fk}]"),
        if has_channel {
            q(&["ZCLASSIFICATIONTYPE"])
        } else {
            "0".into()
        },
        if has_time {
            q(&["ZSTARTTIME"])
        } else {
            "NULL".into()
        },
        if has_time {
            q(&["ZDURATION"])
        } else {
            "NULL".into()
        },
        if has_bbox {
            q(&["ZPACKEDBOUNDINGBOXRECT"])
        } else {
            "NULL".into()
        },
        match &temporal_fk {
            Some(col) => format!("[{col}]"),
            None => "NULL".into(),
        },
    ];
    let sql = format!("SELECT {} FROM [{SCENE_TABLE}]", columns.join(", "));
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };

    // 静态场景 → 资产列表（同一场景 ID + 通道聚合）。
    struct SceneHit {
        scene_id: i64,
        channel: i64,
        assets: Vec<String>,
        first_asset: Option<(String, String)>,
        confidence: f64,
        bbox: Option<(f64, f64, f64, f64)>,
    }
    let mut by_scene: HashMap<(i64, i64), SceneHit> = HashMap::new();
    let mut n = 0usize;

    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let scene_id = int(&r, 0).unwrap_or(-1);
        let confidence = real(&r, 1).unwrap_or(0.0);
        let attr_static = int(&r, 2);
        let channel = int(&r, 3).unwrap_or(0);
        let start_sec = real(&r, 4);
        let duration = real(&r, 5);
        let bbox_raw = int(&r, 6).unwrap_or(0);
        let attr_temporal = int(&r, 7);
        // 两个外键列任一命中即算（静态 / 时序通道）。
        let attr = match (attr_static, attr_temporal) {
            (Some(pk), _) => pk,
            (None, Some(pk)) => pk,
            _ => continue,
        };
        let Some(asset_pk) = asset_of.get(&attr).copied() else {
            continue;
        };
        let Some((filename, directory)) = asset_info.get(&asset_pk).cloned() else {
            continue;
        };

        // 时序/语音分类（channel == 2 || channel == 3）：单条命中直接发射
        if channel == 2 || channel == 3 {
            let (media_path, maybe_fref) = resolve_media_path(run, &directory, &filename);
            let name = scene_name(scene_id).unwrap_or("");
            let scene_display = if name.is_empty() {
                format!("场景识别 ({scene_id})")
            } else {
                name.to_string()
            };
            let channel_label = match channel {
                2 => "视频时序视觉",
                3 => "实况/视频语音检测",
                _ => "时序通道",
            };
            let bbox_text = if bbox_raw != 0 {
                unpack_bbox(bbox_raw)
                    .map(|(x, y, w, h)| format!("x={x:.3},y={y:.3},w={w:.3},h={h:.3}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let mut rec = run
                .record_values(
                    "ios.photos_scene",
                    vec![
                        scene_display.into(),
                        channel_label.into(),
                        Value::from(1i64),
                        Value::from(confidence),
                        filename.clone().into(),
                        start_sec.map(Value::from).unwrap_or(Value::Nil),
                        duration.map(Value::from).unwrap_or(Value::Nil),
                        (if media_path.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(media_path)
                        }),
                        Value::from(scene_id),
                        Value::from(channel),
                        (if bbox_text.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(bbox_text)
                        }),
                    ],
                )
                .source(crate::util::node_source(f).note("音视频时序分类"));
            if channel == 3 && (scene_id == 6 || scene_id == 2 || scene_id == 1) {
                rec = rec.severity(Severity::High);
            }
            if let Some(mf) = maybe_fref {
                rec = rec.source(mf.source().note("关联音视频媒体文件"));
            }
            rec.emit()?;
            n += 1;
            continue;
        }

        // 静态分类聚合
        let entry = by_scene
            .entry((scene_id, channel))
            .or_insert_with(|| SceneHit {
                scene_id,
                channel,
                assets: Vec::new(),
                first_asset: Some((filename.clone(), directory.clone())),
                confidence: 0.0,
                bbox: None,
            });
        if entry.assets.len() < 50 && !entry.assets.contains(&filename) {
            entry.assets.push(filename);
        }
        if confidence > entry.confidence {
            entry.confidence = confidence;
        }
        if bbox_raw != 0 && entry.bbox.is_none() {
            entry.bbox = unpack_bbox(bbox_raw);
        }
    }

    let mut hits: Vec<SceneHit> = by_scene.into_values().collect();
    hits.sort_by(|a, b| a.scene_id.cmp(&b.scene_id).then(a.channel.cmp(&b.channel)));
    for hit in hits {
        let name = scene_name(hit.scene_id).unwrap_or("");
        let scene_display = if name.is_empty() {
            format!("场景识别 ({})", hit.scene_id)
        } else {
            name.to_string()
        };
        let channel_label = match hit.channel {
            0 => "静态视觉场景",
            1 => "局部显著目标",
            4..=7 => "扩展分类器",
            _ => "其他通道",
        };
        let bbox_text = hit
            .bbox
            .map(|(x, y, w, h)| format!("x={x:.3},y={y:.3},w={w:.3},h={h:.3}"))
            .unwrap_or_default();
        let (media_path, maybe_fref) = if let Some((fn_name, dir)) = &hit.first_asset {
            resolve_media_path(run, dir, fn_name)
        } else {
            (String::new(), None)
        };
        let mut rec = run
            .record_values(
                "ios.photos_scene",
                vec![
                    scene_display.into(),
                    channel_label.into(),
                    Value::from(hit.assets.len() as i64),
                    Value::from(hit.confidence),
                    hit.assets.join("、").into(),
                    Value::Nil,
                    Value::Nil,
                    (if media_path.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(media_path)
                    }),
                    Value::from(hit.scene_id),
                    Value::from(hit.channel),
                    (if bbox_text.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(bbox_text)
                    }),
                ],
            )
            .source(crate::util::node_source(f).note("静态图场景分类"));
        if matches!(hit.scene_id, 214 | 881 | 1053 | 1500..=1503) {
            rec = rec.severity(Severity::Info);
        }
        if let Some(mf) = maybe_fref {
            rec = rec.source(mf.source().note("代表性照片文件"));
        }
        rec.emit()?;
        n += 1;
    }
    Ok(n)
}

/// `ZPACKEDBOUNDINGBOXRECT`：4 个 uint16 打包进 u64，归一化到 [0,1]（Vision 坐标
/// 原点在左下角）。返回 (x, y, w, h)。
fn unpack_bbox(packed: i64) -> Option<(f64, f64, f64, f64)> {
    if packed == 0 {
        return None;
    }
    let u = (packed as u64) & 0xFFFF_FFFF_FFFF_FFFF;
    let x = (u & 0xFFFF) as f64 / 65535.0;
    let y = ((u >> 16) & 0xFFFF) as f64 / 65535.0;
    let w = ((u >> 32) & 0xFFFF) as f64 / 65535.0;
    let h = ((u >> 48) & 0xFFFF) as f64 / 65535.0;
    Some((x, y, w, h))
}

/// 二维码 / 条码盲解（`ios.photos_qr`）。
///
/// 取证手册卷二 §2.4：`ZCHARACTERRECOGNITIONATTRIBUTES.ZMACHINEREADABLECODEDATA`
/// 存的是 `VNBarcodeObservation` 的 NSKeyedArchiver，其中
/// `CIQRCodeDescriptor.errorCorrectedPayload` 是**经 Reed-Solomon 纠错后的
/// 原始 ISO/IEC 18004 位流**——即使原图已删，也能还原出链接内容。
/// 实测：FUNG 的 `Z_PK=37` → `https://example.com/qr?s=2`。
fn qrcodes(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    const TABLE: &str = "ZCHARACTERRECOGNITIONATTRIBUTES";
    if !table_exists(conn, TABLE) {
        return Ok(0);
    }
    let tc = cols(conn, TABLE);
    let blob_col = match pick(&tc, &["ZMACHINEREADABLECODEDATA"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    // 资产属性 → 资产 → 文件名。
    let mut attr_to_asset: HashMap<i64, i64> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT [Z_PK], [ZASSET] FROM [ZADDITIONALASSETATTRIBUTES]")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(asset))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                {
                    attr_to_asset.insert(pk, asset);
                }
            }
        }
    }
    struct AssetQrInfo {
        filename: String,
        directory: String,
        created: Option<f64>,
        uuid: String,
        lat: Option<f64>,
        lon: Option<f64>,
    }
    let mut asset_map: HashMap<i64, AssetQrInfo> = HashMap::new();
    let zc = cols(conn, "ZASSET");
    let fn_col = pick(&zc, &["ZFILENAME"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let dir_col = pick(&zc, &["ZDIRECTORY"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let date_col = pick(&zc, &["ZDATECREATED"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let uuid_col = pick(&zc, &["ZUUID"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let lat_col = pick(&zc, &["ZLATITUDE"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let lon_col = pick(&zc, &["ZLONGITUDE"])
        .map(|x| format!("[{x}]"))
        .unwrap_or_else(|| "NULL".into());
    let sql = format!("SELECT [Z_PK], {fn_col}, {dir_col}, {date_col}, {uuid_col}, {lat_col}, {lon_col} FROM [ZASSET]");
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(name))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    let dir = r
                        .get::<_, Option<String>>(2)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let created = r.get::<_, Option<f64>>(3).ok().flatten();
                    let uuid = r
                        .get::<_, Option<String>>(4)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let lat = r.get::<_, Option<f64>>(5).ok().flatten();
                    let lon = r.get::<_, Option<f64>>(6).ok().flatten();
                    asset_map.insert(
                        pk,
                        AssetQrInfo {
                            filename: name,
                            directory: dir,
                            created,
                            uuid,
                            lat,
                            lon,
                        },
                    );
                }
            }
        }
    }
    // 资产 → OCR 属性（ZMEDIAANALYSISASSETATTRIBUTES）→ 资产属性 → 资产。
    let mut media_to_asset: HashMap<i64, i64> = HashMap::new();
    if table_exists(conn, "ZMEDIAANALYSISASSETATTRIBUTES") {
        let mc = cols(conn, "ZMEDIAANALYSISASSETATTRIBUTES");
        if let Some(col) = pick(&mc, &["ZASSET"]) {
            let sql = format!("SELECT [Z_PK], [{col}] FROM [ZMEDIAANALYSISASSETATTRIBUTES]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(pk)), Ok(Some(asset))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                        {
                            media_to_asset.insert(pk, asset);
                        }
                    }
                }
            }
        }
    }
    let sql = format!("SELECT [Z_PK], [{blob_col}] FROM [{TABLE}]");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let pk = int(&r, 0).unwrap_or(-1);
        let blob = r.get::<_, Option<Vec<u8>>>(1).ok().flatten();
        let Some(blob) = blob else { continue };
        let Some(payloads) = decode_qr_payloads(&blob) else {
            continue;
        };
        if payloads.is_empty() {
            continue;
        }
        // 该 OCR 记录 → 资产属性 → 资产 → 文件名与定位。
        let media_attr = pk;
        let asset = media_to_asset
            .get(&media_attr)
            .copied()
            .or_else(|| attr_to_asset.get(&media_attr).copied());
        let info = asset.and_then(|a| asset_map.get(&a));
        let filename = info.map(|i| i.filename.clone()).unwrap_or_default();
        let directory = info.map(|i| i.directory.as_str()).unwrap_or("");
        let (media_path, maybe_fref) = resolve_media_path(run, directory, &filename);
        let loc = match info.and_then(|i| i.lat.zip(i.lon)) {
            Some((lat, lon)) if lat.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 => {
                format!("{lat:.6}, {lon:.6}")
            }
            _ => String::new(),
        };
        let created_ts = ts_or_nil(info.and_then(|i| i.created).and_then(apple));
        let uuid_val = info
            .map(|i| {
                if i.uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(i.uuid.clone())
                }
            })
            .unwrap_or(Value::Nil);

        for payload in payloads {
            let mut rec = run
                .record_values(
                    "ios.photos_qr",
                    vec![
                        filename.clone().into(),
                        "QR Code (位流盲解)".into(),
                        Value::from(payload.clone()),
                        Value::from(1.0f64),
                        created_ts.clone(),
                        Value::Nil,
                        (if media_path.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(media_path.clone())
                        }),
                        (if directory.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(directory)
                        }),
                        (if loc.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(loc.clone())
                        }),
                        uuid_val.clone(),
                        Value::from(pk),
                    ],
                )
                .source(crate::util::node_source(f).note(format!(
                    "ZCHARACTERRECOGNITIONATTRIBUTES row #{pk}（Vision 二维码位流解包）"
                )));
            if !payload.is_empty() {
                rec = rec.severity(Severity::High);
            }
            if let Some(ref mf) = maybe_fref {
                rec = rec.source(mf.source().note("包含二维码的原图"));
            }
            rec.emit()?;
            n += 1;
        }
    }
    Ok(n)
}

/// 从 `ZMACHINEREADABLECODEDATA` 的 NSKeyedArchiver 还原条码明文。
///
/// 走 `CIQRCodeDescriptor.errorCorrectedPayload` → `NSMutableData.NS.data`，
/// 按 ISO/IEC 18004 位流解析（Byte 模式；字符计数字段宽度按 symbolVersion）。
fn decode_qr_payloads(blob: &[u8]) -> Option<Vec<String>> {
    let value = crate::util::read_plist(blob)?;
    let objects = value.as_dictionary()?.get("$objects")?.as_array()?;
    let mut out = Vec::new();
    for obj in objects {
        let Some(dict) = obj.as_dictionary() else {
            continue;
        };
        let Some(payload_ref) = dict.get("errorCorrectedPayload") else {
            continue;
        };
        // UID → $objects 下标
        let data = match payload_ref {
            plist::Value::Uid(uid) => {
                let idx = uid.get() as usize;
                match objects.get(idx) {
                    Some(plist::Value::Dictionary(inner)) => inner.get("NS.data"),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(plist::Value::Data(bytes)) = data else {
            continue;
        };
        let version = dict
            .get("symbolVersion")
            .and_then(plist::Value::as_signed_integer)
            .unwrap_or(0);
        if let Some(text) = decode_qr_bitstream(bytes, version) {
            out.push(text);
        }
    }
    Some(out)
}

/// ISO/IEC 18004 位流解码（仅 Byte 模式 + 终止符，足够还原链接）。
fn decode_qr_bitstream(data: &[u8], version: i64) -> Option<String> {
    let mut bits: Vec<u8> = Vec::with_capacity(data.len() * 8);
    for byte in data {
        for shift in (0..8).rev() {
            bits.push((byte >> shift) & 1);
        }
    }
    let mut idx = 0usize;
    let mut out: Vec<u8> = Vec::new();
    while idx + 4 <= bits.len() {
        let mode = bits[idx..idx + 4].iter().fold(0u8, |acc, b| (acc << 1) | b);
        idx += 4;
        match mode {
            0 => break,
            4 => {
                // Byte 模式：版本 1-9 用 8 位计数，10-40 用 16 位。
                let count_bits = if version > 0 && version < 10 { 8 } else { 16 };
                if idx + count_bits > bits.len() {
                    break;
                }
                let count = bits[idx..idx + count_bits]
                    .iter()
                    .fold(0usize, |acc, b| (acc << 1) | *b as usize);
                idx += count_bits;
                for _ in 0..count {
                    if idx + 8 > bits.len() {
                        return String::from_utf8(out).ok();
                    }
                    let byte = bits[idx..idx + 8].iter().fold(0u8, |acc, b| (acc << 1) | b);
                    idx += 8;
                    out.push(byte);
                }
            }
            _ => break,
        }
    }
    if out.is_empty() {
        None
    } else {
        String::from_utf8(out).ok()
    }
}

/// 反向地理编码（`ZREVERSELOCATIONDATA`）→ 街道 / 城市 / 完整地址。
///
/// 取证手册卷二 §2.2：这是 Apple GeoServices 的 `PLRevGeoLocationInfo`
/// NSKeyedArchiver，`addressString` 字段直接给出「街道, 地区, 国家」全地址。
fn reverse_geo_address(blob: &[u8]) -> Option<String> {
    let value = crate::util::read_plist(blob)?;
    let objects = value.as_dictionary()?.get("$objects")?.as_array()?;
    let root = objects.first()?.as_dictionary()?;
    // 顶层 addressString（CNPostalAddress._formattedAddress 为可选备份）。
    if let Some(plist::Value::String(text)) = root.get("addressString") {
        if !text.trim().is_empty() {
            return Some(text.clone());
        }
    }
    // 退而求其次：CNPostalAddress 的 _formattedAddress / _street + _city。
    let mut street = String::new();
    let mut city = String::new();
    for obj in objects {
        let Some(dict) = obj.as_dictionary() else {
            continue;
        };
        if let Some(plist::Value::String(s)) = dict.get("_formattedAddress") {
            if !s.trim().is_empty() {
                return Some(s.replace('\n', ", "));
            }
        }
        if let Some(plist::Value::String(s)) = dict.get("_street") {
            street = s.clone();
        }
        if let Some(plist::Value::String(s)) = dict.get("_city") {
            city = s.clone();
        }
    }
    if !street.is_empty() || !city.is_empty() {
        return Some(
            vec![street, city]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    None
}

/// 照片分享字段（`ios.photos_share`）——**渠道未知**，不能算作 AirDrop。
///
/// 取证口径（用户标准）：`ZLASTSHAREDDATE` / `ZSHARECOUNT` 只说明"被分享过"，
/// 分享渠道系统未记录（可能是 AirDrop，也可能是即时通讯/邮件/短信），因此单列到
/// 独立记录类型，channel 标注「渠道未记录」，不进 AirDrop 传输表。
///
/// 反之，AirDrop 确证来自两处：`sharingd` 导入（接收侧）与 CoreDuet
/// interactionC.db 的 `ZTARGETBUNDLEID=com.apple.UIKit.activity.AirDrop`（发起侧）。
pub fn share_fields(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(
                f,
                "DB_OPEN",
                format!("照片分享字段：Photos.sqlite 打开失败：{e}"),
            )?;
            return Ok(0);
        }
    };
    if !crate::backupdb::table_exists(&conn, "ZASSET") {
        return Ok(0);
    }
    let zc = crate::backupdb::cols(&conn, "ZASSET");
    let z = |want: &[&str]| -> String {
        match crate::backupdb::pick(&zc, want) {
            Some(x) => format!("z.[{x}]"),
            None => "NULL".into(),
        }
    };
    let has_attr = crate::backupdb::table_exists(&conn, "ZADDITIONALASSETATTRIBUTES")
        && crate::backupdb::cols(&conn, "ZADDITIONALASSETATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ac = if has_attr {
        crate::backupdb::cols(&conn, "ZADDITIONALASSETATTRIBUTES")
    } else {
        Vec::new()
    };
    let a = |want: &[&str]| -> String {
        if !has_attr {
            return "NULL".into();
        }
        match crate::backupdb::pick(&ac, want) {
            Some(x) => format!("a.[{x}]"),
            None => "NULL".into(),
        }
    };
    let has_ext = crate::backupdb::table_exists(&conn, "ZEXTENDEDATTRIBUTES")
        && crate::backupdb::cols(&conn, "ZEXTENDEDATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ec = if has_ext {
        crate::backupdb::cols(&conn, "ZEXTENDEDATTRIBUTES")
    } else {
        Vec::new()
    };
    let e = |want: &[&str]| -> String {
        if !has_ext {
            return "NULL".into();
        }
        match crate::backupdb::pick(&ec, want) {
            Some(x) => format!("e.[{x}]"),
            None => "NULL".into(),
        }
    };
    let joins = format!(
        "{}{}",
        if has_attr {
            " LEFT JOIN [ZADDITIONALASSETATTRIBUTES] a ON a.[ZASSET] = z.[Z_PK]"
        } else {
            ""
        },
        if has_ext {
            " LEFT JOIN [ZEXTENDEDATTRIBUTES] e ON e.[ZASSET] = z.[Z_PK]"
        } else {
            ""
        }
    );
    let columns = vec![
        z(&["ZFILENAME"]),
        z(&["ZLASTSHAREDDATE"]),
        z(&["ZADDEDDATE"]),
        z(&["ZDATECREATED"]),
        a(&["ZSHARECOUNT"]),
        a(&["ZIMPORTEDBYBUNDLEIDENTIFIER"]),
        a(&["ZORIGINALFILENAME"]),
        a(&["ZIMPORTEDBYDISPLAYNAME"]),
        e(&["ZCAMERAMAKE"]),
        e(&["ZCAMERAMODEL"]),
        e(&["ZTIMEZONENAME"]),
    ];
    let sql = format!(
        "SELECT {} FROM [ZASSET] z{joins} WHERE {} IS NOT NULL OR {} IS NOT NULL OR {} LIKE '%sharingd%' LIMIT 2000",
        columns.join(", "),
        z(&["ZLASTSHAREDDATE"]),
        z(&["ZLASTSHAREDDATE"]),
        a(&["ZIMPORTEDBYBUNDLEIDENTIFIER"]),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let filename = r.get::<_, Option<String>>(0).ok().flatten();
        let Some(filename) = filename else { continue };
        let shared = r.get::<_, Option<f64>>(1).ok().flatten();
        let added = r.get::<_, Option<f64>>(2).ok().flatten();
        let created = r.get::<_, Option<f64>>(3).ok().flatten();
        let share_count = r.get::<_, Option<i64>>(4).ok().flatten().unwrap_or(0);
        let imported = r
            .get::<_, Option<String>>(5)
            .ok()
            .flatten()
            .unwrap_or_default();
        let orig_name = r
            .get::<_, Option<String>>(6)
            .ok()
            .flatten()
            .unwrap_or_default();
        let src_name = r
            .get::<_, Option<String>>(7)
            .ok()
            .flatten()
            .unwrap_or_default();
        let cam_make = r
            .get::<_, Option<String>>(8)
            .ok()
            .flatten()
            .unwrap_or_default();
        let cam_model = r
            .get::<_, Option<String>>(9)
            .ok()
            .flatten()
            .unwrap_or_default();
        let tz = r
            .get::<_, Option<String>>(10)
            .ok()
            .flatten()
            .unwrap_or_default();
        let is_recv = imported.to_lowercase().contains("sharingd");
        if !is_recv && shared.is_none() && share_count == 0 {
            continue;
        }
        let device = if !cam_make.trim().is_empty() || !cam_model.trim().is_empty() {
            format!("{} {}", cam_make.trim(), cam_model.trim())
                .trim()
                .to_string()
        } else {
            String::new()
        };
        // 接收侧（sharingd 导入）= AirDrop 确证；其余渠道未知。
        let (direction, time, peer, channel, detail) = if is_recv {
            let mut d = "由 sharingd 导入（AirDrop 接收确证）；传输模式=AWDL 点对点直连（设备间直传，无云端中转）".to_string();
            if !orig_name.trim().is_empty() {
                d += &format!("；对方原文件名 {orig_name}");
            }
            if !tz.trim().is_empty() {
                d += &format!("；拍摄时区 {tz}");
            }
            (
                "接收（AirDrop 确证）",
                added.or(created).unwrap_or_default(),
                if device.is_empty() {
                    String::new()
                } else {
                    device
                },
                "AirDrop",
                d,
            )
        } else {
            (
                "分享（渠道未知）",
                shared.unwrap_or_default(),
                String::new(),
                "渠道未记录",
                format!(
                    "照片分享字段记录（ZLASTSHAREDDATE），分享 {share_count} 次；分享渠道未被系统记录——不能据此认定 AirDrop；本机拍摄设备 {}",
                    if device.is_empty() { "<无记录>" } else { &device }
                ),
            )
        };
        run.record_values(
            "ios.photos_share",
            vec![
                direction.into(),
                filename.into(),
                crate::backupdb::ts_or_nil(crate::backupdb::apple(time)),
                (if peer.is_empty() {
                    Value::Nil
                } else {
                    Value::from(peer)
                }),
                channel.into(),
                detail.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 硬删除墓碑（`ios.photos_tombstone`）。
///
/// 取证手册卷二 §1.4：CoreData 持久化历史表 `ACHANGE` 记录**已从「最近删除」
/// 彻底清空**的资产——`ZCHANGETYPE=2`（Delete）。`ZTOMBSTONE0..5` 按属性名
/// 字母序保留删除前的值（Asset 实体：cloudAssetGUID / cloudDeleteState /
/// deleteReason / trashedDate / trashedReason / uuid）。
///
/// 配合 `Z_PRIMARYKEY.Z_MAX` 与实际行数之差，可量化"被删了多少"。
struct TombstoneBrief {
    tx_time: Option<f64>,
    delete_reason: String,
    trashed_date: Option<f64>,
}

#[derive(Clone, Debug, Default)]
struct WalAssetSummary {
    filename: String,
    original_filename: String,
    original_filesize: Option<i64>,
    cloud_master_guid: String,
    cloud_asset_guid: String,
    imported_by: String,
    created: Option<f64>,
    trashed_date: Option<f64>,
    uuid: String,
}

#[derive(Clone, Debug, Default)]
struct WalAddAttrSummary {
    original_filename: String,
    original_filesize: Option<i64>,
    original_stable_hash: String,
    imported_by: String,
}

#[derive(Clone, Debug, Default)]
struct WalCloudMasterSummary {
    cloud_master_guid: String,
    original_filename: String,
    imported_by: String,
}

fn format_imported_by_summary(code: Option<i64>, display_name: &str, bundle_id: &str) -> String {
    let dn = display_name.trim();
    let bi = bundle_id.trim();
    let enum_lbl = code
        .map(|c| photos_enums::enum_label(photos_enums::IMPORTED_BY, c))
        .unwrap_or_default();
    match (!dn.is_empty(), !bi.is_empty(), !enum_lbl.is_empty()) {
        (true, true, _) if !dn.eq_ignore_ascii_case(bi) => format!("{dn} ({bi})"),
        (true, _, _) => dn.to_owned(),
        (false, true, true) => format!("{enum_lbl} ({bi})"),
        (false, true, false) => bi.to_owned(),
        (false, false, true) => enum_lbl,
        _ => String::new(),
    }
}

#[derive(Default)]
struct WalRecoveryMap {
    by_pk: HashMap<i64, WalAssetSummary>,
    by_uuid: HashMap<String, WalAssetSummary>,
}

fn format_trashed_reason(code: Option<i64>, trashed_state: i64) -> String {
    let Some(c) = code else {
        return String::new();
    };
    if trashed_state == 0 && c == 0 {
        return String::new();
    }
    let lbl = photos_enums::enum_label(photos_enums::TRASHED_REASON, c);
    if !lbl.is_empty() {
        format!("{lbl} ({c})")
    } else if c == 0 {
        "未记录原因码 (0)".to_owned()
    } else {
        format!("原因码 {c}")
    }
}

fn delete_reason_zh(reason: &str) -> String {
    let r = reason.trim();
    if r.is_empty() {
        return String::new();
    }
    if r.eq_ignore_ascii_case("Deleting expired asset from trash") {
        return "从「最近删除」过期/永久清除 (Deleting expired asset from trash)".to_owned();
    }
    r.to_owned()
}

fn compute_trash_delay(created: Option<f64>, trashed: Option<f64>) -> Option<f64> {
    let (Some(c), Some(t)) = (created, trashed) else {
        return None;
    };
    if !c.is_finite() || !t.is_finite() || c <= 0.0 || t <= 0.0 {
        return None;
    }
    let to_sec = |v: f64| if v.abs() > 1e14 { v / 1e9 } else { v };
    let dt = ((to_sec(t) - to_sec(c)) * 1000.0).round() / 1000.0;
    dt.is_finite().then_some(dt)
}

fn sql_str(cols: &HashMap<String, SqlValue>, key: &str) -> String {
    cols.get(key)
        .and_then(SqlValue::as_str)
        .unwrap_or("")
        .to_owned()
}

fn sql_i64(cols: &HashMap<String, SqlValue>, key: &str) -> Option<i64> {
    cols.get(key).and_then(SqlValue::as_i64)
}

fn sql_f64(cols: &HashMap<String, SqlValue>, key: &str) -> Option<f64> {
    cols.get(key).and_then(SqlValue::as_f64)
}

fn ms_key(v: Option<f64>) -> Option<i64> {
    let raw = v.filter(|x| x.is_finite() && *x > 0.0)?;
    let sec = if raw.abs() > 1e14 { raw / 1e9 } else { raw };
    Some((sec * 1000.0).round() as i64)
}

fn collect_tombstone_briefs(conn: &Connection) -> HashMap<i64, TombstoneBrief> {
    let mut map = HashMap::new();
    if !table_exists(conn, "ACHANGE") {
        return map;
    }
    let cc = cols(conn, "ACHANGE");
    let c = |want: &[&str]| -> String {
        match pick(&cc, want) {
            Some(x) => format!("ch.[{x}]"),
            None => "NULL".into(),
        }
    };
    let mut entity_asset = Vec::new();
    if table_exists(conn, "Z_PRIMARYKEY") {
        if let Ok(mut st) =
            conn.prepare("SELECT [Z_ENT] FROM [Z_PRIMARYKEY] WHERE [Z_NAME] = 'Asset'")
        {
            if let Ok(mut rows) = st.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let Ok(Some(ent)) = r.get::<_, Option<i64>>(0) {
                        entity_asset.push(ent);
                    }
                }
            }
        }
    }
    let has_tx = table_exists(conn, "ATRANSACTION");
    let tx_ts_col = if has_tx {
        let tc = cols(conn, "ATRANSACTION");
        pick(&tc, &["ZTIMESTAMP"])
            .map(|x| format!("tx.[{x}]"))
            .unwrap_or_else(|| "NULL".into())
    } else {
        "NULL".into()
    };
    let join = if has_tx {
        " LEFT JOIN [ATRANSACTION] tx ON tx.[Z_PK] = ch.[ZTRANSACTIONID]"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {tx_ts_col} FROM [ACHANGE] ch{join} WHERE {} = 2",
        c(&["ZENTITY"]),
        c(&["ZENTITYPK"]),
        c(&["ZTOMBSTONE2"]),
        c(&["ZTOMBSTONE3"]),
        c(&["ZCHANGETYPE"]),
    );
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let Some(ent) = int(&r, 0) else { continue };
                if !entity_asset.is_empty() && !entity_asset.contains(&ent) {
                    continue;
                }
                let Some(pk) = int(&r, 1) else { continue };
                let delete_reason = text(&r, 2).unwrap_or_default();
                let trashed_date = real(&r, 3);
                let tx_time = real(&r, 4);
                map.insert(
                    pk,
                    TombstoneBrief {
                        tx_time,
                        delete_reason,
                        trashed_date,
                    },
                );
            }
        }
    }
    map
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct AssetSnapSig {
    rowid: i64,
    filename: String,
    original_filename: String,
    directory: String,
    uuid: String,
    trashed_state: i64,
    trashed_ms: Option<i64>,
    trashed_reason: Option<i64>,
    created_ms: Option<i64>,
    added_ms: Option<i64>,
    hidden: i64,
    favorite: i64,
}

/// 结合 `Photos.sqlite` + `-wal` + `-shm` 解析所有历史页帧、未提交事务、旧 salt 残留与 B-Tree 空闲块，
/// 产出 `ios.photos_wal_meta` 与 `ios.photos_wal_history` 并返回可回填 Tombstone 的资产摘要。
fn wal_analysis(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
    conn: &Connection,
    tomb_by_pk: &HashMap<i64, TombstoneBrief>,
) -> lovelycarve_plugin::Result<(usize, WalRecoveryMap)> {
    let mut recovery = WalRecoveryMap::default();
    let db_bytes = read_file_bytes(f);
    let wal_file = find_sidecar(f, sides, "-wal");
    let shm_file = find_sidecar(f, sides, "-shm");
    let wal_bytes = wal_file.map(read_file_bytes);
    let shm_bytes = shm_file.map(read_file_bytes);

    let Some(inspection) =
        sqlite_wal::inspect_wal_shm(&db_bytes, wal_bytes.as_deref(), shm_bytes.as_deref())
    else {
        return Ok((0, recovery));
    };

    // Active Asset PKs + active snapshot signatures (from SQLite post-WAL view)
    let mut active_sigs: HashMap<i64, AssetSnapSig> = HashMap::new();
    let zc = cols(conn, "ZASSET");
    let has_attr = table_exists(conn, "ZADDITIONALASSETATTRIBUTES")
        && cols(conn, "ZADDITIONALASSETATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET");
    let ac = if has_attr {
        cols(conn, "ZADDITIONALASSETATTRIBUTES")
    } else {
        Vec::new()
    };
    let zcol = |w: &[&str]| {
        pick(&zc, w)
            .map(|x| format!("z.[{x}]"))
            .unwrap_or_else(|| "NULL".into())
    };
    let acol = |w: &[&str]| {
        if has_attr {
            pick(&ac, w)
                .map(|x| format!("a.[{x}]"))
                .unwrap_or_else(|| "NULL".into())
        } else {
            "NULL".into()
        }
    };
    let join_attr = if has_attr {
        " LEFT JOIN [ZADDITIONALASSETATTRIBUTES] a ON a.[ZASSET] = z.[Z_PK]"
    } else {
        ""
    };
    let active_sql = format!(
        "SELECT z.[Z_PK], {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM [ZASSET] z{join_attr}",
        zcol(&["ZFILENAME"]),
        acol(&["ZORIGINALFILENAME"]),
        zcol(&["ZDIRECTORY"]),
        zcol(&["ZUUID"]),
        zcol(&["ZTRASHEDSTATE"]),
        zcol(&["ZTRASHEDDATE"]),
        zcol(&["ZTRASHEDREASON"]),
        zcol(&["ZDATECREATED"]),
        zcol(&["ZADDEDDATE"]),
        zcol(&["ZHIDDEN"]),
        zcol(&["ZFAVORITE"]),
    );
    if let Ok(mut stmt) = conn.prepare(&active_sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let Some(pk) = int(&r, 0) else { continue };
                let orig = text(&r, 2).unwrap_or_default();
                active_sigs.insert(
                    pk,
                    AssetSnapSig {
                        rowid: pk,
                        filename: text(&r, 1).unwrap_or_default(),
                        original_filename: orig,
                        directory: text(&r, 3).unwrap_or_default(),
                        uuid: text(&r, 4).unwrap_or_default(),
                        trashed_state: int(&r, 5).unwrap_or(0),
                        trashed_ms: ms_key(real(&r, 6)),
                        trashed_reason: int(&r, 7),
                        created_ms: ms_key(real(&r, 8)),
                        added_ms: ms_key(real(&r, 9)),
                        hidden: int(&r, 10).unwrap_or(0),
                        favorite: int(&r, 11).unwrap_or(0),
                    },
                );
            }
        }
    }

    let z_max: Option<i64> = if table_exists(conn, "Z_PRIMARYKEY") {
        conn.query_row(
            "SELECT [Z_MAX] FROM [Z_PRIMARYKEY] WHERE [Z_NAME] = 'Asset' LIMIT 1",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    } else {
        None
    };

    let mut missing_pks = Vec::new();
    if let Some(max_pk) = z_max {
        if max_pk > 0 && max_pk <= 50_000 {
            for pk in 1..=max_pk {
                if !active_sigs.contains_key(&pk) {
                    missing_pks.push(pk);
                }
            }
        }
    }

    let mut n = 0usize;
    let db_src = crate::util::node_source(f).note("Photos.sqlite 主库与侧车日志");

    // Emit WAL/SHM & PK hole summary (`ios.photos_wal_meta`)
    let wal_summary = match &inspection.wal {
        Some(w) => format!(
            "共 {} 帧（已提交 {} / 未提交 {} / 旧Salt残留 {}），{} 次提交，ckpt_seq={}，salt={:08x}-{:08x}",
            inspection.frames.len(),
            inspection.committed_frames,
            inspection.uncommitted_frames,
            inspection.prior_salt_frames,
            inspection.commit_count,
            w.ckpt_seq,
            w.salt1,
            w.salt2,
        ),
        None => "无独立 -wal 日志文件或未包含有效 WAL 帧".to_owned(),
    };
    let shm_summary = match &inspection.shm {
        Some(s) => format!(
            "-shm 已初始化={}，pageSize={}，mxFrame={}，nPage={}，nBackfill={}，salt={:08x}-{:08x}",
            s.is_init, s.page_size, s.mx_frame, s.n_page, s.n_backfill, s.salt1, s.salt2
        ),
        None => "无 -shm 索引文件".to_owned(),
    };
    let wal_sev = if inspection.uncommitted_frames > 0
        || inspection.prior_salt_frames > 0
        || !missing_pks.is_empty()
    {
        Some(Severity::Medium)
    } else {
        None
    };
    let mut meta_rec1 = run.record_values(
        "ios.photos_wal_meta",
        vec![
            "WAL / SHM 事务状态".into(),
            Value::from(format!(
                "已提交 {} 帧 / 未提交 {} 帧 / 旧Salt {} 帧",
                inspection.committed_frames,
                inspection.uncommitted_frames,
                inspection.prior_salt_frames
            )),
            Value::from(
                if inspection.uncommitted_frames > 0 || inspection.prior_salt_frames > 0 {
                    "发现未提交或残留历史帧"
                } else if inspection.wal.is_some() {
                    "已关联 WAL 历史追踪"
                } else {
                    "仅主库页面"
                },
            ),
            Value::from(format!("{wal_summary}；{shm_summary}")),
        ],
    );
    if let Some(s) = wal_sev {
        meta_rec1 = meta_rec1.severity(s);
    }
    meta_rec1.source(db_src.clone()).emit()?;
    n += 1;

    let pk_gap_text = if missing_pks.is_empty() {
        "无缺失主键".to_owned()
    } else {
        let preview = missing_pks
            .iter()
            .take(15)
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        if missing_pks.len() > 15 {
            format!("缺失 {} 个主键：[{preview}, …]", missing_pks.len())
        } else {
            format!("缺失 {} 个主键：[{preview}]", missing_pks.len())
        }
    };
    let mut pk_rec = run.record_values(
        "ios.photos_wal_meta",
        vec![
            "ZASSET 主键空洞 (Z_PRIMARYKEY.Z_MAX)".into(),
            Value::from(format!(
                "Z_MAX={} / 现存 {} 行 / 缺失 {} 个",
                z_max.map_or_else(|| "?".into(), |m| m.to_string()),
                active_sigs.len(),
                missing_pks.len()
            )),
            Value::from(if missing_pks.is_empty() {
                "主键连续"
            } else {
                "存在已彻底硬删除的记录空洞"
            }),
            Value::from(pk_gap_text),
        ],
    );
    if !missing_pks.is_empty() {
        pk_rec = pk_rec.severity(Severity::High);
    }
    pk_rec.source(db_src.clone()).emit()?;
    n += 1;

    // Collect ZADDITIONALASSETATTRIBUTES, ZCLOUDMASTER, and ZINTERNALRESOURCE history across BaseDb + WAL + Freeblocks
    let mut orig_by_pk_seq: BTreeMap<(i64, u32), String> = BTreeMap::new();
    let mut best_attr_by_asset_pk: HashMap<i64, WalAddAttrSummary> = HashMap::new();
    let mut best_attr_by_row_pk: HashMap<i64, WalAddAttrSummary> = HashMap::new();
    if let Some(attr_meta) =
        sqlite_wal::load_table_meta(&db_bytes, &inspection, "ZADDITIONALASSETATTRIBUTES")
    {
        for row in sqlite_wal::scan_table_history(&db_bytes, &inspection, &attr_meta) {
            let asset_fk = sql_i64(&row.cols, "ZASSET");
            let orig = sql_str(&row.cols, "ZORIGINALFILENAME");
            let fsize = sql_i64(&row.cols, "ZORIGINALFILESIZE").filter(|&v| v > 0);
            let stable_hash = sql_str(&row.cols, "ZORIGINALSTABLEHASH");
            let imp_code = sql_i64(&row.cols, "ZIMPORTEDBY");
            let imp_bundle = sql_str(&row.cols, "ZIMPORTEDBYBUNDLEIDENTIFIER");
            let imp_name = sql_str(&row.cols, "ZIMPORTEDBYDISPLAYNAME");
            let imp_str = format_imported_by_summary(imp_code, &imp_name, &imp_bundle);

            let merge_attr = |dst: &mut WalAddAttrSummary| {
                if !orig.is_empty() {
                    dst.original_filename = orig.clone();
                }
                if fsize.is_some() {
                    dst.original_filesize = fsize;
                }
                if !stable_hash.is_empty() {
                    dst.original_stable_hash = stable_hash.clone();
                }
                if !imp_str.is_empty() {
                    dst.imported_by = imp_str.clone();
                }
            };
            if row.rowid > 0 {
                merge_attr(best_attr_by_row_pk.entry(row.rowid).or_default());
            }
            if let Some(apk) = asset_fk.filter(|&v| v > 0) {
                if !orig.is_empty() {
                    orig_by_pk_seq.insert((apk, row.frame_no), orig.clone());
                }
                merge_attr(best_attr_by_asset_pk.entry(apk).or_default());
            }
        }
    }

    let mut best_cm_by_pk: HashMap<i64, WalCloudMasterSummary> = HashMap::new();
    let mut best_cm_by_filename: HashMap<String, WalCloudMasterSummary> = HashMap::new();
    if let Some(cm_meta) = sqlite_wal::load_table_meta(&db_bytes, &inspection, "ZCLOUDMASTER") {
        for row in sqlite_wal::scan_table_history(&db_bytes, &inspection, &cm_meta) {
            let guid = sql_str(&row.cols, "ZCLOUDMASTERGUID");
            let orig = sql_str(&row.cols, "ZORIGINALFILENAME");
            let imp_code = sql_i64(&row.cols, "ZIMPORTEDBY");
            let imp_bundle = sql_str(&row.cols, "ZIMPORTEDBYBUNDLEIDENTIFIER");
            let imp_name = sql_str(&row.cols, "ZIMPORTEDBYDISPLAYNAME");
            let imp_str = format_imported_by_summary(imp_code, &imp_name, &imp_bundle);
            if guid.is_empty() && orig.is_empty() && imp_str.is_empty() {
                continue;
            }
            let merge_cm = |dst: &mut WalCloudMasterSummary| {
                if !guid.is_empty() {
                    dst.cloud_master_guid = guid.clone();
                }
                if !orig.is_empty() {
                    dst.original_filename = orig.clone();
                }
                if !imp_str.is_empty() {
                    dst.imported_by = imp_str.clone();
                }
            };
            if row.rowid > 0 {
                merge_cm(best_cm_by_pk.entry(row.rowid).or_default());
            }
            if !orig.is_empty() {
                merge_cm(best_cm_by_filename.entry(orig.clone()).or_default());
            }
        }
    }

    let mut ir_size_by_asset: HashMap<i64, i64> = HashMap::new();
    let mut ir_fp_by_asset: HashMap<i64, String> = HashMap::new();
    if let Some(ir_meta) = sqlite_wal::load_table_meta(&db_bytes, &inspection, "ZINTERNALRESOURCE")
    {
        for row in sqlite_wal::scan_table_history(&db_bytes, &inspection, &ir_meta) {
            let Some(apk) = sql_i64(&row.cols, "ZASSET").filter(|&v| v > 0) else {
                continue;
            };
            let res_type = sql_i64(&row.cols, "ZRESOURCETYPE").unwrap_or(-1);
            if let Some(dlen) = sql_i64(&row.cols, "ZDATALENGTH").filter(|&v| v > 0) {
                let entry = ir_size_by_asset.entry(apk).or_insert(0);
                if res_type == 0 || *entry == 0 {
                    *entry = dlen;
                }
            }
            let fp = sql_str(&row.cols, "ZFINGERPRINT");
            if !fp.is_empty() && (res_type == 0 || !ir_fp_by_asset.contains_key(&apk)) {
                ir_fp_by_asset.insert(apk, fp);
            }
        }
    }

    // Seed recovery.by_pk for any deleted PK present in ZADDITIONALASSETATTRIBUTES or ZCLOUDMASTER
    for (&apk, attr) in &best_attr_by_asset_pk {
        if !active_sigs.contains_key(&apk) {
            let entry = recovery.by_pk.entry(apk).or_default();
            if entry.original_filename.is_empty() && !attr.original_filename.is_empty() {
                entry.original_filename = attr.original_filename.clone();
            }
            if entry.original_filesize.is_none() {
                entry.original_filesize = attr
                    .original_filesize
                    .or_else(|| ir_size_by_asset.get(&apk).copied());
            }
            if entry.cloud_master_guid.is_empty() && !attr.original_stable_hash.is_empty() {
                entry.cloud_master_guid = attr.original_stable_hash.clone();
            }
            if entry.imported_by.is_empty() && !attr.imported_by.is_empty() {
                entry.imported_by = attr.imported_by.clone();
            }
        }
    }
    for (&cm_pk, cm_rec) in &best_cm_by_pk {
        if !active_sigs.contains_key(&cm_pk) {
            let entry = recovery.by_pk.entry(cm_pk).or_default();
            if entry.cloud_master_guid.is_empty() && !cm_rec.cloud_master_guid.is_empty() {
                entry.cloud_master_guid = cm_rec.cloud_master_guid.clone();
            }
            if entry.original_filename.is_empty() && !cm_rec.original_filename.is_empty() {
                entry.original_filename = cm_rec.original_filename.clone();
            }
            if entry.imported_by.is_empty() && !cm_rec.imported_by.is_empty() {
                entry.imported_by = cm_rec.imported_by.clone();
            }
        }
    }

    let Some(asset_meta) = sqlite_wal::load_table_meta(&db_bytes, &inspection, "ZASSET") else {
        return Ok((n, recovery));
    };

    let mut raw_rows = sqlite_wal::scan_table_history(&db_bytes, &inspection, &asset_meta);
    raw_rows.sort_by_key(|r| (r.rowid, r.commit_no, r.frame_no, r.is_freeblock));

    let mut seen_sigs: HashSet<(AssetSnapSig, &'static str)> = HashSet::new();

    for row in raw_rows {
        run.check_cancelled()?;
        let pk = row.rowid;
        if pk <= 0 {
            continue;
        }
        let filename = sql_str(&row.cols, "ZFILENAME");
        let directory = sql_str(&row.cols, "ZDIRECTORY");
        let uuid = sql_str(&row.cols, "ZUUID");
        let created = sql_f64(&row.cols, "ZDATECREATED");
        let added = sql_f64(&row.cols, "ZADDEDDATE");
        let trashed_state = sql_i64(&row.cols, "ZTRASHEDSTATE").unwrap_or(0);
        let trashed_date = sql_f64(&row.cols, "ZTRASHEDDATE");
        let trashed_reason_code = sql_i64(&row.cols, "ZTRASHEDREASON");
        let hidden = sql_i64(&row.cols, "ZHIDDEN").unwrap_or(0);
        let favorite = sql_i64(&row.cols, "ZFAVORITE").unwrap_or(0);
        let width = sql_i64(&row.cols, "ZWIDTH").unwrap_or(0);
        let height = sql_i64(&row.cols, "ZHEIGHT").unwrap_or(0);
        let duration = sql_f64(&row.cols, "ZDURATION");
        let add_attr_fk = sql_i64(&row.cols, "ZADDITIONALATTRIBUTES").filter(|&v| v > 0);
        let master_fk = sql_i64(&row.cols, "ZMASTER").filter(|&v| v > 0);
        let cloud_asset_guid = sql_str(&row.cols, "ZCLOUDASSETGUID");

        // Skip empty / un-populated freeblock fragments lacking basic Asset identity
        if filename.is_empty() && uuid.is_empty() && created.is_none() {
            continue;
        }

        let attr_rec = best_attr_by_asset_pk
            .get(&pk)
            .or_else(|| add_attr_fk.and_then(|fk| best_attr_by_row_pk.get(&fk)));
        let cm_rec = master_fk
            .and_then(|fk| best_cm_by_pk.get(&fk))
            .or_else(|| best_cm_by_pk.get(&pk))
            .or_else(|| {
                (!filename.is_empty())
                    .then(|| best_cm_by_filename.get(&filename))
                    .flatten()
            });

        let orig_name = orig_by_pk_seq
            .range((pk, 0)..=(pk, row.frame_no))
            .next_back()
            .map(|(_, v)| v.clone())
            .or_else(|| {
                attr_rec
                    .map(|a| a.original_filename.clone())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                cm_rec
                    .map(|c| c.original_filename.clone())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| active_sigs.get(&pk).map(|a| a.original_filename.clone()))
            .unwrap_or_default();

        let orig_filesize = attr_rec
            .and_then(|a| a.original_filesize)
            .or_else(|| ir_size_by_asset.get(&pk).copied());

        let cloud_master_guid = cm_rec
            .map(|c| c.cloud_master_guid.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                attr_rec
                    .map(|a| a.original_stable_hash.as_str())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                ir_fp_by_asset
                    .get(&pk)
                    .map(String::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("")
            .to_owned();

        let imported_by = attr_rec
            .map(|a| a.imported_by.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                cm_rec
                    .map(|c| c.imported_by.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("")
            .to_owned();

        let is_active = active_sigs.contains_key(&pk);
        if !is_active {
            let entry = recovery.by_pk.entry(pk).or_default();
            if entry.filename.is_empty() && !filename.is_empty() {
                entry.filename = filename.clone();
            }
            if entry.original_filename.is_empty() && !orig_name.is_empty() {
                entry.original_filename = orig_name.clone();
            }
            if entry.original_filesize.is_none() && orig_filesize.is_some() {
                entry.original_filesize = orig_filesize;
            }
            if entry.cloud_master_guid.is_empty() && !cloud_master_guid.is_empty() {
                entry.cloud_master_guid = cloud_master_guid.clone();
            }
            if entry.cloud_asset_guid.is_empty() && !cloud_asset_guid.is_empty() {
                entry.cloud_asset_guid = cloud_asset_guid.clone();
            }
            if entry.imported_by.is_empty() && !imported_by.is_empty() {
                entry.imported_by = imported_by.clone();
            }
            if entry.created.is_none() {
                entry.created = created;
            }
            if trashed_date.is_some() {
                entry.trashed_date = trashed_date;
            }
            if entry.uuid.is_empty() && !uuid.is_empty() {
                entry.uuid = uuid.clone();
            }
            if !uuid.is_empty() {
                recovery.by_uuid.insert(uuid.clone(), entry.clone());
            }
        }

        let sig = AssetSnapSig {
            rowid: pk,
            filename: filename.clone(),
            original_filename: orig_name.clone(),
            directory: directory.clone(),
            uuid: uuid.clone(),
            trashed_state,
            trashed_ms: ms_key(trashed_date),
            trashed_reason: trashed_reason_code,
            created_ms: ms_key(created),
            added_ms: ms_key(added),
            hidden,
            favorite,
        };

        // Filter out redundant unchanged active snapshots from BaseDb/CommittedWal
        let is_unusual_frame = matches!(
            row.status,
            WalFrameStatus::UncommittedWal
                | WalFrameStatus::PriorSaltResidual
                | WalFrameStatus::BtreeFreeblock
        );
        if is_active && !is_unusual_frame {
            if let Some(cur) = active_sigs.get(&pk) {
                if cur == &sig {
                    continue;
                }
            }
        }
        if !seen_sigs.insert((sig, row.status.zh_label())) {
            continue;
        }

        let tomb = tomb_by_pk.get(&pk);
        let eff_trashed = if trashed_state != 0 {
            trashed_date.or_else(|| tomb.and_then(|tb| tb.trashed_date))
        } else {
            trashed_date
        };
        let trash_delay = compute_trash_delay(created, eff_trashed);

        let record_state = if !is_active {
            match row.status {
                WalFrameStatus::BtreeFreeblock => "已彻底删除（从 B-Tree 空闲块雕刻恢复）",
                WalFrameStatus::UncommittedWal => "已删除 / 未提交 WAL 事务中的状态",
                WalFrameStatus::PriorSaltResidual => "已删除（旧 Salt 残留帧恢复）",
                _ => "已彻底硬删除（从 WAL 历史帧恢复）",
            }
        } else {
            match row.status {
                WalFrameStatus::UncommittedWal => "现存资产 · 未提交 WAL 变更",
                WalFrameStatus::PriorSaltResidual => "现存资产 · 旧 Salt 残留历史",
                WalFrameStatus::BtreeFreeblock => "现存资产 · 空闲块历史版本",
                _ => "现存资产 · 历史已变更快照",
            }
        };

        let trashed_state_text = match trashed_state {
            0 => "正常 (0)".to_owned(),
            1 => "在「最近删除」回收站 (1)".to_owned(),
            2 => "已从回收站永久移除 (2)".to_owned(),
            x => format!("状态码 {x}"),
        };
        let trashed_reason_text = format_trashed_reason(trashed_reason_code, trashed_state);
        let delete_time_val = ts_or_nil(tomb.and_then(|t| t.tx_time).and_then(apple_precise));
        let delete_reason_text = tomb
            .map(|t| delete_reason_zh(&t.delete_reason))
            .unwrap_or_default();

        let sev = if !is_active {
            Some(Severity::High)
        } else if is_unusual_frame || trashed_state != 0 {
            Some(Severity::Medium)
        } else {
            Some(Severity::Info)
        };

        let src_note = format!(
            "{} · ZASSET Z_PK={pk} · 页 #{} · 帧 #{} (提交 #{})",
            row.status.zh_label(),
            row.pgno,
            row.frame_no,
            row.commit_no
        );
        let src_file = match row.status {
            WalFrameStatus::BaseDb | WalFrameStatus::BtreeFreeblock => f,
            WalFrameStatus::CommittedWal
            | WalFrameStatus::UncommittedWal
            | WalFrameStatus::PriorSaltResidual => wal_file.unwrap_or(f),
        };

        let mut emit = run.record_values(
            "ios.photos_wal_history",
            vec![
                Value::from(pk),
                (if filename.is_empty() {
                    Value::Nil
                } else {
                    Value::from(filename)
                }),
                (if orig_name.is_empty() {
                    Value::Nil
                } else {
                    Value::from(orig_name)
                }),
                orig_filesize.map_or(Value::Nil, Value::from),
                (if cloud_master_guid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(cloud_master_guid)
                }),
                record_state.into(),
                row.status.zh_label().into(),
                Value::from(row.frame_no as i64),
                Value::from(row.commit_no as i64),
                Value::from(row.pgno as i64),
                ts_or_nil(created.and_then(apple_precise)),
                ts_or_nil(eff_trashed.and_then(apple_precise)),
                trash_delay.map_or(Value::Nil, Value::from),
                trashed_state_text.into(),
                (if trashed_reason_text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(trashed_reason_text)
                }),
                delete_time_val,
                (if delete_reason_text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(delete_reason_text)
                }),
                (if imported_by.is_empty() {
                    Value::Nil
                } else {
                    Value::from(imported_by)
                }),
                ts_or_nil(added.and_then(apple_precise)),
                (if directory.is_empty() {
                    Value::Nil
                } else {
                    Value::from(directory)
                }),
                (if uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(uuid)
                }),
                (if width > 0 {
                    Value::from(width)
                } else {
                    Value::Nil
                }),
                (if height > 0 {
                    Value::from(height)
                } else {
                    Value::Nil
                }),
                duration
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .map_or(Value::Nil, Value::from),
                (if favorite == 1 { "是" } else { "" }).into(),
                (if hidden == 1 { "是" } else { "" }).into(),
            ],
        );
        if let Some(s) = sev {
            emit = emit.severity(s);
        }
        emit.source(crate::util::node_source(src_file).note(src_note))
            .emit()?;
        n += 1;
    }

    Ok((n, recovery))
}

/// 硬删除记录（`ios.photos_tombstone`）：已从「最近删除」彻底清空的持久化历史日志，
/// 结合 `ATRANSACTION` 硬删除时间和 WAL 历史还原的原文件名/拍摄时间/销毁间隔。
fn tombstones(
    run: &Run,
    f: &FileRef,
    conn: &Connection,
    recovery: &WalRecoveryMap,
) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ACHANGE") || !table_exists(conn, "ATRANSACTION") {
        return Ok(0);
    }
    let mut entity_name: HashMap<i64, String> = HashMap::new();
    if table_exists(conn, "Z_PRIMARYKEY") {
        if let Ok(mut stmt) = conn.prepare("SELECT [Z_ENT], [Z_NAME] FROM [Z_PRIMARYKEY]") {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let (Ok(Some(ent)), Ok(Some(name))) =
                        (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                    {
                        entity_name.insert(ent, name);
                    }
                }
            }
        }
    }
    let mut strings: HashMap<i64, String> = HashMap::new();
    if table_exists(conn, "ATRANSACTIONSTRING") {
        if let Ok(mut stmt) = conn.prepare("SELECT [Z_PK], [ZNAME] FROM [ATRANSACTIONSTRING]") {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let (Ok(Some(pk)), Ok(Some(name))) =
                        (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                    {
                        strings.insert(pk, name);
                    }
                }
            }
        }
    }
    let tc = cols(conn, "ATRANSACTION");
    let t = |want: &[&str]| -> String {
        match pick(&tc, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let cc = cols(conn, "ACHANGE");
    let c = |want: &[&str]| -> String {
        match pick(&cc, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let columns = vec![
        c(&["ZENTITY"]),
        c(&["ZENTITYPK"]),
        c(&["ZTRANSACTIONID"]),
        c(&["ZTOMBSTONE0"]),
        c(&["ZTOMBSTONE1"]),
        c(&["ZTOMBSTONE2"]),
        c(&["ZTOMBSTONE3"]),
        c(&["ZTOMBSTONE4"]),
        c(&["ZTOMBSTONE5"]),
    ];
    let sql = format!(
        "SELECT {} FROM [ACHANGE] WHERE {} = 2",
        columns.join(", "),
        c(&["ZCHANGETYPE"])
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let Some(entity) = int(&r, 0) else { continue };
        let entity_pk = int(&r, 1);
        let tx_id = int(&r, 2);
        let ts0 = text(&r, 3).unwrap_or_default();
        let ts1 = int(&r, 4);
        let ts2 = text(&r, 5).unwrap_or_default();
        let ts3 = real(&r, 6);
        let ts4 = int(&r, 7);
        let ts5 = text(&r, 8).unwrap_or_default();
        let ent_name = entity_name.get(&entity).cloned().unwrap_or_default();
        if ent_name != "Asset" && ent_name != "Person" && ent_name != "Album" {
            continue;
        }
        let ent_display = match ent_name.as_str() {
            "Asset" => "照片/视频资产 (Asset)",
            "Person" => "人物聚类 (Person)",
            "Album" => "相册 (Album)",
            other => other,
        };
        let (tx_time, process, context) = if let Some(tid) = tx_id.filter(|v| *v >= 0) {
            let sql = format!(
                "SELECT {}, {}, {} FROM [ATRANSACTION] WHERE [Z_PK] = ?1",
                t(&["ZTIMESTAMP"]),
                t(&["ZPROCESSIDTS"]),
                t(&["ZCONTEXTNAMETS"]),
            );
            let mut result = (None, String::new(), String::new());
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([tid]) {
                    if let Ok(Some(tr)) = rows.next() {
                        let ts = tr.get::<_, Option<f64>>(0).ok().flatten();
                        let proc_id = tr.get::<_, Option<i64>>(1).ok().flatten();
                        let ctx_id = tr.get::<_, Option<i64>>(2).ok().flatten();
                        result = (
                            ts,
                            proc_id
                                .and_then(|id| strings.get(&id).cloned())
                                .unwrap_or_default(),
                            ctx_id
                                .and_then(|id| strings.get(&id).cloned())
                                .unwrap_or_default(),
                        );
                    }
                }
            }
            result
        } else {
            (None, String::new(), String::new())
        };
        let uuid = if ent_name == "Asset" {
            ts5.clone()
        } else {
            String::new()
        };
        let wal_rec = if ent_name == "Asset" {
            entity_pk
                .and_then(|pk| recovery.by_pk.get(&pk))
                .or_else(|| {
                    (!uuid.is_empty())
                        .then(|| recovery.by_uuid.get(&uuid))
                        .flatten()
                })
        } else {
            None
        };
        let recovered_name = wal_rec
            .map(|w| {
                if !w.original_filename.is_empty() && w.original_filename != w.filename {
                    format!("{} (原名 {})", w.filename, w.original_filename)
                } else if !w.filename.is_empty() {
                    w.filename.clone()
                } else {
                    w.original_filename.clone()
                }
            })
            .unwrap_or_default();
        let created_raw = wal_rec.and_then(|w| w.created);
        let trashed_raw = ts3.or_else(|| wal_rec.and_then(|w| w.trashed_date));
        let trash_delay = compute_trash_delay(created_raw, trashed_raw);
        let trashed_reason_text = ts4
            .map(|v| format_trashed_reason(Some(v), 1))
            .unwrap_or_default();
        let cloud_delete_text = ts1
            .map(|v| {
                let lbl = photos_enums::enum_label(photos_enums::CLOUD_DELETE_STATE, v);
                if lbl.is_empty() {
                    format!("状态码 {v}")
                } else {
                    format!("{lbl} ({v})")
                }
            })
            .unwrap_or_default();
        let del_reason_text = delete_reason_zh(&ts2);
        let orig_filesize = wal_rec.and_then(|w| w.original_filesize);
        let cloud_master_guid = wal_rec.map(|w| w.cloud_master_guid.as_str()).unwrap_or("");
        let cloud_asset_guid = if !ts0.is_empty() {
            ts0.as_str()
        } else {
            wal_rec.map(|w| w.cloud_asset_guid.as_str()).unwrap_or("")
        };
        let imported_by = wal_rec.map(|w| w.imported_by.as_str()).unwrap_or("");

        run.record_values(
            "ios.photos_tombstone",
            vec![
                ent_display.into(),
                entity_pk.map_or(Value::Nil, Value::from),
                (if recovered_name.is_empty() {
                    Value::Nil
                } else {
                    Value::from(recovered_name)
                }),
                orig_filesize.map_or(Value::Nil, Value::from),
                (if cloud_master_guid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(cloud_master_guid)
                }),
                ts_or_nil(created_raw.and_then(apple_precise)),
                ts_or_nil(trashed_raw.and_then(apple_precise)),
                trash_delay.map_or(Value::Nil, Value::from),
                (if trashed_reason_text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(trashed_reason_text)
                }),
                ts_or_nil(tx_time.and_then(apple_precise)),
                (if del_reason_text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(del_reason_text)
                }),
                (if cloud_delete_text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(cloud_delete_text)
                }),
                (if uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(uuid)
                }),
                (if cloud_asset_guid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(cloud_asset_guid)
                }),
                (if imported_by.is_empty() {
                    Value::Nil
                } else {
                    Value::from(imported_by)
                }),
                (if process.is_empty() {
                    Value::Nil
                } else {
                    Value::from(process)
                }),
                (if context.is_empty() {
                    Value::Nil
                } else {
                    Value::from(context)
                }),
            ],
        )
        .severity(Severity::High)
        .source(crate::util::node_source(f).note(format!(
            "ACHANGE 永久删除日志 ZENTITYPK={}",
            entity_pk.map_or_else(|| "?".into(), |p| p.to_string())
        )))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// iCloud 云端资产（`ios.photos_cloud`）。
fn cloud(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZCLOUDMASTER") {
        return Ok(0);
    }
    let c = cols(conn, "ZCLOUDMASTER");
    let q = |want: &[&str]| -> String {
        match pick(&c, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {} FROM [ZCLOUDMASTER]",
        q(&["ZCLOUDMASTERGUID"]),
        q(&["ZCLOUDLOCALSTATE"]),
        q(&["ZIMPORTEDBY"]),
        q(&["ZCREATIONDATE"]),
        q(&["ZIMPORTDATE"]),
        q(&["ZPLACEHOLDERSTATE"]),
        q(&["ZCODECNAME"]),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let guid = text(&r, 0).unwrap_or_default();
        if guid.is_empty() {
            continue;
        }
        let local_state = int(&r, 1).unwrap_or(-1);
        let imported_by = text(&r, 2).unwrap_or_default();
        let created = real(&r, 3);
        let import_date = real(&r, 4);
        let placeholder = int(&r, 5).unwrap_or(-1);
        let codec = text(&r, 6).unwrap_or_default();
        let state = match local_state {
            0 => "仅云端",
            1 => "已下载到本机",
            2 => "下载中",
            3 => "已从云端删除",
            _ => "",
        };
        run.record_values(
            "ios.photos_cloud",
            vec![
                guid.into(),
                state.into(),
                (if imported_by.is_empty() {
                    Value::Nil
                } else {
                    Value::from(imported_by)
                }),
                ts_or_nil(created.and_then(apple)),
                ts_or_nil(import_date.and_then(apple)),
                (if placeholder >= 0 {
                    Value::from(placeholder)
                } else {
                    Value::Nil
                }),
                (if codec.is_empty() {
                    Value::Nil
                } else {
                    Value::from(codec)
                }),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 资产分析状态（`ios.photos_analysis`）：场景数 / 人脸数 / 分析时间。
fn analysis(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZASSETANALYSISSTATE") {
        return Ok(0);
    }
    let c = cols(conn, "ZASSETANALYSISSTATE");
    let q = |want: &[&str]| -> String {
        match pick(&c, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let asset_fk = match pick(&c, &["ZASSET"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    let sql = format!(
        "SELECT {}, {}, {}, {} FROM [ZASSETANALYSISSTATE]",
        q(&["ZANALYSISSTATE"]),
        q(&["ZWORKERTYPE"]),
        q(&["ZWORKERFLAGS"]),
        format!("[{asset_fk}]"),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut names: HashMap<i64, String> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT [Z_PK], [ZFILENAME] FROM [ZASSET]") {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(pk)), Ok(Some(name))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    names.insert(pk, name);
                }
            }
        }
    }
    // 每资产的场景数与最大置信度。
    let mut scene_counts: HashMap<i64, i64> = HashMap::new();
    if table_exists(conn, SCENE_TABLE) {
        let sc = cols(conn, SCENE_TABLE);
        if let Some(fk) = pick(&sc, &["ZASSET", "ZASSETATTRIBUTES"]) {
            let sql = format!("SELECT [{fk}], COUNT(*) FROM [{SCENE_TABLE}] GROUP BY [{fk}]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(pk)), Ok(Some(cnt))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                        {
                            scene_counts.insert(pk, cnt);
                        }
                    }
                }
            }
        }
    }
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let analysis_state = int(&r, 0).unwrap_or(-1);
        let worker_type = int(&r, 1).unwrap_or(-1);
        let worker_flags = int(&r, 2).unwrap_or(-1);
        let asset = int(&r, 3);
        let Some(asset) = asset else { continue };
        let Some(filename) = names.get(&asset) else {
            continue;
        };
        let scenes = scene_counts.get(&asset).copied().unwrap_or(0);
        run.record_values(
            "ios.photos_analysis",
            vec![
                filename.clone().into(),
                Value::from(scenes),
                (if analysis_state >= 0 {
                    Value::from(analysis_state)
                } else {
                    Value::Nil
                }),
                (if worker_type >= 0 {
                    Value::from(worker_type)
                } else {
                    Value::Nil
                }),
                (if worker_flags >= 0 {
                    Value::from(worker_flags)
                } else {
                    Value::Nil
                }),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}
