//! AirDrop 取证：CoreDuet 交互记录（权威渠道证据）、Photos.sqlite 分享/
//! 导入标记与本机标识（sharingd）。
//!
//! 数据源与口径：
//! 1. `var/mobile/Library/CoreDuet/People/interactionC.db` 的 ZINTERACTIONS
//!    （ZTARGETBUNDLEID=com.apple.UIKit.activity.AirDrop）是 AirDrop 收发的
//!    权威记录：ZBUNDLEID 给出内容来源 App（mobileslideshow=照片图库、
//!    DocumentsApp=文件 App），ZDIRECTION=1 发送 / 0 接收；
//! 2. 交叉验证：照片行 ↔ Photos.sqlite ZLASTSHAREDDATE 同秒命中具体资产；
//!    文档行 ↔ 文件 App 本地存储（File Provider Storage）修改时间最近的候选；
//! 3. 接收侧（sharingd 导入 + ZEXTENDEDATTRIBUTES 发送方设备）与照片分享
//!    字段为佐证；被 CoreDuet 权威记录同秒覆盖的佐证行不重报（去重）；
//!    对端装置 ID 在文件级提取中不存在结构化记录。

use std::collections::HashMap;

use lovelycarve_plugin::{FileRef, Run, Severity, Value};
use plist::Value as PValue;

/// Photos.sqlite 中的 AirDrop 收发记录 → `ios.airdrop_transfer`。
///
/// 去重口径：与 CoreDuet 权威记录（interactionC.db）同秒的传输时刻不再
/// 重报照片分享字段行——权威行已确证渠道并带接收方，佐证行只留冗余。
/// CoreDuet 库缺席（老备份/检材不全）时保留全部照片行（原行为）。
pub fn transfers(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    // 已被 CoreDuet 权威记录覆盖的传输时刻。
    let mut duet_starts: Vec<f64> = Vec::new();
    if let Some(duet) = sides
        .iter()
        .find(|s| s.name().to_lowercase() == "interactionc.db")
    {
        if let Ok((_tmp, duet_conn)) = crate::backupdb::open_db(duet, sides) {
            if crate::backupdb::table_exists(&duet_conn, "ZINTERACTIONS") {
                if let Ok(mut stmt) = duet_conn.prepare(
                    "SELECT [ZSTARTDATE] FROM [ZINTERACTIONS] WHERE [ZTARGETBUNDLEID] = 'com.apple.UIKit.activity.AirDrop'",
                ) {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            if let Ok(Some(t)) = r.get::<_, Option<f64>>(0) {
                                duet_starts.push(t);
                            }
                        }
                    }
                }
            }
        }
    }
    let (_t, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(
                f,
                "DB_OPEN",
                format!("AirDrop：Photos.sqlite 打开失败：{e}"),
            )?;
            return Err(lovelycarve_plugin::Error::new("DB_OPEN", e));
        }
    };
    let zc = crate::backupdb::cols(&conn, "ZASSET");
    if !crate::backupdb::table_exists(&conn, "ZASSET") {
        return Ok(0);
    }
    let ac = if crate::backupdb::table_exists(&conn, "ZADDITIONALASSETATTRIBUTES") {
        crate::backupdb::cols(&conn, "ZADDITIONALASSETATTRIBUTES")
    } else {
        Vec::new()
    };
    let has_join = ac.iter().any(|x| x == "ZASSET");
    let join = if has_join {
        " LEFT JOIN [ZADDITIONALASSETATTRIBUTES] a ON a.[ZASSET] = z.[Z_PK]"
    } else {
        ""
    };
    // 扩展属性：接收的 AirDrop 资产保留发送方设备的拍摄型号与时区
    let ec = if crate::backupdb::table_exists(&conn, "ZEXTENDEDATTRIBUTES")
        && crate::backupdb::cols(&conn, "ZEXTENDEDATTRIBUTES")
            .iter()
            .any(|x| x == "ZASSET")
    {
        crate::backupdb::cols(&conn, "ZEXTENDEDATTRIBUTES")
    } else {
        Vec::new()
    };
    let has_ext = !ec.is_empty();
    let ext_join = if has_ext {
        " LEFT JOIN [ZEXTENDEDATTRIBUTES] e ON e.[ZASSET] = z.[Z_PK]"
    } else {
        ""
    };
    let col = |want: &[&str], tbl: &str| -> String {
        let c = match tbl {
            "z" => &zc,
            "a" => &ac,
            _ => &ec,
        };
        match crate::backupdb::pick(c, want) {
            Some(x) => format!("{tbl}.[{x}]"),
            None => "NULL".into(),
        }
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM [ZASSET] z{join}{ext_join} \
         WHERE {} IS NOT NULL OR {} IS NOT NULL OR {} LIKE '%sharingd%' LIMIT 2000",
        col(&["ZFILENAME"], "z"),
        col(&["ZLASTSHAREDDATE"], "z"),
        col(&["ZADDEDDATE"], "z"),
        col(&["ZDATECREATED"], "z"),
        col(&["ZSHARECOUNT"], "a"),
        col(&["ZIMPORTEDBYBUNDLEIDENTIFIER"], "a"),
        col(&["ZORIGINALFILENAME"], "a"),
        col(&["ZCAMERAMAKE"], "e"),
        col(&["ZCAMERAMODEL"], "e"),
        col(&["ZTIMEZONENAME"], "e"),
        col(&["ZLASTSHAREDDATE"], "z"),
        col(&["ZSHARECOUNT"], "a"),
        col(&["ZIMPORTEDBYBUNDLEIDENTIFIER"], "a"),
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
        let file: Option<String> = r.get(0).ok().flatten();
        let Some(file) = file else { continue };
        let shared: Option<f64> = r.get(1).ok().flatten();
        let added: Option<f64> = r.get(2).ok().flatten();
        let created: Option<f64> = r.get(3).ok().flatten();
        let share_count: Option<i64> = r.get(4).ok().flatten();
        let imported: Option<String> = r.get(5).ok().flatten();
        let orig_name: Option<String> = r.get(6).ok().flatten();
        let cam_make: Option<String> = r.get(7).ok().flatten();
        let cam_model: Option<String> = r.get(8).ok().flatten();
        let tz: Option<String> = r.get(9).ok().flatten();
        let is_recv = imported
            .as_deref()
            .map(|s| s.to_lowercase().contains("sharingd"))
            .unwrap_or(false);
        if !is_recv && shared.is_none() && share_count.unwrap_or(0) == 0 {
            continue;
        }
        // CoreDuet 权威行已覆盖该传输（同秒）：佐证行不重报。
        let representative = if is_recv { added.or(created) } else { shared };
        if representative.is_some_and(|t| duet_starts.iter().any(|s| (s - t).abs() < 2.0)) {
            continue;
        }
        // 对端装置：接收资产的拍摄设备即发送方手机（本机为接收方）
        let device = match (&cam_make, &cam_model) {
            (Some(mk), Some(md)) if !mk.trim().is_empty() || !md.trim().is_empty() => {
                format!("{} {}", mk.trim(), md.trim()).trim().to_string()
            }
            _ => String::new(),
        };
        let (direction, time, peer, mode, detail) = if is_recv {
            let mut d = format!(
                "由 sharingd 导入（AirDrop 接收确证）；AirDrop 传输模式=AWDL 点对点直连（设备间直传，无云端中转）"
            );
            if let Some(o) = &orig_name {
                if !o.trim().is_empty() {
                    d += &format!("；对方原文件名 {o}");
                }
            }
            if let Some(t) = &tz {
                if !t.trim().is_empty() {
                    d += &format!("；拍摄时区 {t}");
                }
            }
            (
                "接收",
                added.or(created).unwrap_or_default(),
                device,
                "AWDL 点对点直连",
                d,
            )
        } else {
            (
                "发送",
                shared.unwrap_or_default(),
                String::new(),
                "渠道未记录",
                format!(
                    "照片分享字段记录（ZLASTSHAREDDATE），分享 {} 次；分享渠道未被系统记录——不能据此认定 AirDrop；本机拍摄设备 {}",
                    share_count.unwrap_or(0),
                    if device.is_empty() { "<无记录>" } else { &device }
                ),
            )
        };
        let (media_path, maybe_fref) = {
            let mut resolved_path = String::new();
            let mut resolved_ref = None;
            if let Some(view) = run.view() {
                let candidates = [
                    file.clone(),
                    format!("var/mobile/Media/DCIM/100APPLE/{file}"),
                    format!("var/mobile/Media/DCIM/101APPLE/{file}"),
                    format!("var/mobile/Media/DCIM/102APPLE/{file}"),
                    format!("var/mobile/Media/{file}"),
                    format!("var/mobile/Media/Downloads/{file}"),
                    format!("Media/DCIM/100APPLE/{file}"),
                    format!("Media/DCIM/101APPLE/{file}"),
                    format!("Media/{file}"),
                    format!("HomeDomain/Media/{file}"),
                    format!("CameraRollDomain/Media/{file}"),
                ];
                for cand in &candidates {
                    if let Ok(Some(fref)) = view.open(cand) {
                        resolved_path = fref.view_path().unwrap_or(cand).to_string();
                        resolved_ref = Some(fref);
                        break;
                    }
                }
            }
            (resolved_path, resolved_ref)
        };
        // AirDrop 传输表只收 AirDrop 确证行（sharingd 导入 = 接收）。
        // 渠道未知的照片分享字段单列到 ios.photos_share，不能算作传输。
        if is_recv {
            let mut rec = run
                .record_values(
                    "ios.airdrop_transfer",
                    vec![
                        direction.into(),
                        file.into(),
                        crate::backupdb::ts_or_nil(crate::backupdb::apple(time)),
                        (if peer.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(peer)
                        }),
                        mode.into(),
                        detail.into(),
                        (if media_path.is_empty() {
                            Value::Nil
                        } else {
                            Value::from(media_path)
                        }),
                    ],
                )
                .severity(Severity::High)
                .source(crate::util::node_source(f).note("Photos.sqlite sharingd 接收确证"));
            if let Some(mf) = maybe_fref {
                rec = rec.source(mf.source().note("AirDrop 检材媒体文件"));
            }
            rec.emit()?;
        } else {
            run.record_values(
                "ios.photos_share",
                vec![
                    direction.into(),
                    file.into(),
                    crate::backupdb::ts_or_nil(crate::backupdb::apple(time)),
                    (if peer.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(peer)
                    }),
                    mode.into(),
                    detail.into(),
                ],
            )
            .source(crate::util::node_source(f).note("Photos.sqlite 照片分享字段"))
            .emit()?;
        }
        n += 1;
    }
    Ok(n)
}

/// 来源 App → （内容类型标签, file 字段缺省描述）。
fn source_app_kind(bundle: &str) -> (&'static str, &'static str) {
    match bundle {
        "com.apple.mobileslideshow" => ("照片图库（图片）", "图片（照片图库）"),
        "com.apple.DocumentsApp" => ("文件 App（文档）", "文档（文件 App）"),
        _ => ("", "未知内容"),
    }
}

/// interactionC.db（CoreDuet）ZINTERACTIONS 的 AirDrop 行 → `ios.airdrop_transfer`。
///
/// 这是 AirDrop 收发的权威记录；照片行用 Photos.sqlite 的 ZLASTSHAREDDATE
/// 同秒命中交叉验证到具体资产，文档行用文件 App 本地存储的修改时间报候选。
pub fn coreduet(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(
                f,
                "DB_OPEN",
                format!("AirDrop：interactionC.db 打开失败：{e}"),
            )?;
            return Ok(0);
        }
    };
    if !crate::backupdb::table_exists(&conn, "ZINTERACTIONS") {
        return Ok(0);
    }
    let sql = "SELECT [Z_PK], [ZBUNDLEID], [ZSTARTDATE], [ZDIRECTION], [ZMECHANISM], [ZRECIPIENTCOUNT] FROM [ZINTERACTIONS] \
               WHERE [ZTARGETBUNDLEID] = 'com.apple.UIKit.activity.AirDrop' ORDER BY [ZSTARTDATE]";
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };

    // 接收方（join Z_2INTERACTIONRECIPIENT → ZCONTACTS）：AirDrop 外发时分享
    // 面板扫描到附近设备即写入，只有设备显示名（ZDISPLAYNAME），通常无
    // Apple ID / 电话 / 序列号（ZIDENTIFIER 等为 NULL）。部分备份可能缺
    // 关联表，缺席时 peer 留空。
    let recipient_sql = if crate::backupdb::table_exists(&conn, "Z_2INTERACTIONRECIPIENT")
        && crate::backupdb::table_exists(&conn, "ZCONTACTS")
    {
        Some(
            "SELECT c.[ZDISPLAYNAME], c.[ZTYPE], c.[ZCREATIONDATE], c.[ZLASTOUTGOINGRECIPIENTDATE] \
             FROM [Z_2INTERACTIONRECIPIENT] j LEFT JOIN [ZCONTACTS] c ON c.[Z_PK] = j.[Z_2RECIPIENTS] \
             WHERE j.[Z_3INTERACTIONRECIPIENT] = ?1",
        )
    } else {
        None
    };

    // 附件工件（ZATTACHMENT + Z_1INTERACTIONS）：唯一记录「实际传输了什么
    // 文件」的表——UTI 直接给出文件类型（public.heic / com.adobe.pdf），
    // 比时间就近猜测更硬。
    let attachment_utis = attachment_utis_by_interaction(&conn);
    // 交叉验证数据源：Photos.sqlite（同批检材）。
    let photos_ref = sides
        .iter()
        .find(|s| s.name().to_lowercase() == "photos.sqlite");
    let photos_conn = photos_ref.and_then(|photos| {
        crate::backupdb::open_db(photos, sides)
            .ok()
            .map(|(_tmp, conn)| conn)
    });

    // View（文件 App 本地存储候选的修改时间需要 stat）。
    let view = run.require_view().ok();

    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let pk: Option<i64> = r.get(0).ok();
        let bundle: Option<String> = r.get(1).ok().flatten();
        let start: Option<f64> = r.get(2).ok().flatten();
        let direction: Option<i64> = r.get(3).ok().flatten();
        let mechanism: Option<i64> = r.get(4).ok().flatten();
        let recipient_count: Option<i64> = r.get(5).ok().flatten();
        let Some(bundle) = bundle else { continue };
        let (kind, default_file) = source_app_kind(&bundle);
        // ZDIRECTION：1=外发、0=接收。CoreDuet 只记录 AirDrop 分享动作的
        // 发起（分享面板扫描到附近设备并选定对端），不记录传输是否成功、
        // 是否被接收——故措辞用「发起分享」而非「发送完成」。
        let dir_label = match direction {
            Some(1) => "外发方向（发起分享）",
            Some(0) => "接收方向",
            _ => "方向未知",
        };
        let mut detail =
            format!("来源 App {bundle}（{kind}）：CoreDuet 交互记录=AirDrop 分享动作发起");
        if let Some(mechanism) = mechanism {
            detail += &format!("；ZMECHANISM={mechanism}");
        }
        if let Some(count) = recipient_count {
            detail += &format!("；接收方数={count}");
        }
        if let Some(pk) = pk {
            detail += &format!("；ZINTERACTIONS 行 Z_PK={pk}");
        }

        // 接收方设备名（外发才有意义）：同秒新建 = 分享面板扫描到附近设备即写。
        let mut peer = String::new();
        if direction == Some(1) {
            if let (Some(pk), Some(rec_sql)) = (pk, recipient_sql) {
                if let Ok(mut rec_stmt) = conn.prepare(rec_sql) {
                    if let Ok(mut rec_rows) = rec_stmt.query([pk]) {
                        while let Ok(Some(rec)) = rec_rows.next() {
                            let name: Option<String> = rec.get(0).ok().flatten();
                            let ctype: Option<i64> = rec.get(1).ok().flatten();
                            let created: Option<f64> = rec.get(2).ok().flatten();
                            let last_out: Option<f64> = rec.get(3).ok().flatten();
                            let Some(name) = name else { continue };
                            peer = name.clone();
                            let type_label = ctype
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "未知".into());
                            detail += &format!("；接收方设备名「{name}」（ZCONTACTS.ZTYPE={type_label} 设备/终端类；Apple ID/电话/序列号无记录）");
                            if let (Some(created), Some(start)) = (created, start) {
                                let delta = (created - start).abs();
                                if delta < 5.0 {
                                    detail += &format!(
                                        "；联系人记录与交互同秒新建（差 {delta:.3}s）＝分享面板扫描即写，非既有通讯录"
                                    );
                                }
                            }
                            if last_out.is_some() {
                                detail += "；ZLASTOUTGOINGRECIPIENTDATE 与交互开始一致";
                            }
                            break;
                        }
                    }
                }
            }
            if peer.is_empty() {
                detail += "；接收方：无 ZCONTACTS 记录（或库缺关联表）";
            } else {
                detail += "；注意：设备名不含对端 Apple ID/序列号/MAC，同名旁证（如同名 Wi-Fi 热点）仅为旁证";
            }
        }

        let mut file = default_file.to_string();
        let mut file_kind = String::new();
        if let Some(utis) = pk.and_then(|k| attachment_utis.get(&k)) {
            if !utis.is_empty() {
                file_kind = utis.join("、");
                // UTI 即文件类型（public.heic / com.adobe.pdf / public.jpeg…）。
                file = file_kind.clone();
            }
        }
        match bundle.as_str() {
            "com.apple.mobileslideshow" => {
                // 交叉验证：Photos.sqlite 里 ZLASTSHAREDDATE 与交互时刻同秒的资产。
                let mut hit = None;
                if let (Some(start), Some(conn)) = (start, photos_conn.as_ref()) {
                    let sql = "SELECT a.[ZORIGINALFILENAME], z.[ZFILENAME], a.[ZSHARECOUNT] \
                               FROM [ZASSET] z LEFT JOIN [ZADDITIONALASSETATTRIBUTES] a ON a.[ZASSET]=z.[Z_PK] \
                               WHERE ABS(z.[ZLASTSHAREDDATE] - ?1) < 2";
                    if let Ok(mut stmt) = conn.prepare(sql) {
                        if let Ok(mut rows) = stmt.query([start]) {
                            while let Ok(Some(r)) = rows.next() {
                                let orig: Option<String> = r.get(0).ok().flatten();
                                let zname: Option<String> = r.get(1).ok().flatten();
                                let count: Option<i64> = r.get(2).ok().flatten();
                                let name = orig.or(zname);
                                if let Some(name) = name {
                                    hit = Some((name, count.unwrap_or(0)));
                                    break;
                                }
                            }
                        }
                    }
                }
                match hit {
                    Some((name, count)) => {
                        file = name.clone();
                        detail += &format!(
                            "；交叉验证：Photos.sqlite 命中 {name}（ZLASTSHAREDDATE 与交互时刻同秒吻合，分享 {count} 次）"
                        );
                    }
                    None => {
                        detail += "；交叉验证：Photos.sqlite 无同秒资产命中";
                    }
                }
            }
            "com.apple.DocumentsApp" => {
                // 交叉验证：文件 App 本地存储（File Provider Storage）修改时间
                // 最接近交互时刻的候选（文档分享不落单独字段，按时间就近报候选）。
                let mut candidates: Vec<(String, i64, i64)> = Vec::new();
                if let (Some(start), Some(view)) = (start, view.as_ref()) {
                    let interact_epoch = 978307200i64 + start as i64;
                    if let Ok(paths) = view.find(&["**/File Provider Storage/**/*"]) {
                        for path in paths.iter().take(400) {
                            let Ok(Some(node)) = view.stat(path) else {
                                continue;
                            };
                            let Some(modified) = node.timestamps.modified else {
                                continue;
                            };
                            let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&modified) else {
                                continue;
                            };
                            let mtime = dt.timestamp();
                            let diff = interact_epoch - mtime;
                            // 发出的文件在交互前修改/落盘：接受 30 分钟内的提前量，
                            // 以及 ±10 分钟内的双向容差。
                            if (0..=1800).contains(&diff) || (-600..=0).contains(&diff) {
                                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                                candidates.push((name, mtime, diff));
                            }
                        }
                    }
                }
                candidates.sort_by_key(|(_, _, diff)| diff.abs());
                if candidates.is_empty() {
                    detail += "；交叉验证：文件 App 本地存储无时间窗内候选";
                } else {
                    let (name, _mtime, diff) = &candidates[0];
                    file = name.clone();
                    detail +=
                        &format!("；交叉验证：文件 App 本地存储最接近候选 {name}（早 {diff} 秒）");
                    if candidates.len() > 1 {
                        let others: Vec<&str> = candidates
                            .iter()
                            .skip(1)
                            .take(3)
                            .map(|(name, _, _)| name.as_str())
                            .collect();
                        detail += &format!("；其余候选（按接近度）：{}", others.join("、"));
                    }
                }
            }
            _ => {}
        }

        if !file_kind.is_empty() {
            detail += &format!("；传输工件（ZATTACHMENT）：{file_kind}");
        }
        detail += "；边界：该库仅记录分享动作发起与对端选择，不记录传输是否成功/被接收";
        let (media_path, maybe_fref) = {
            let mut resolved_path = String::new();
            let mut resolved_ref = None;
            if let Some(view) = run.view() {
                let candidates = [
                    file.clone(),
                    format!("var/mobile/Media/DCIM/100APPLE/{file}"),
                    format!("var/mobile/Media/DCIM/101APPLE/{file}"),
                    format!("var/mobile/Media/DCIM/102APPLE/{file}"),
                    format!("var/mobile/Media/{file}"),
                    format!("var/mobile/Media/Downloads/{file}"),
                    format!("Media/DCIM/100APPLE/{file}"),
                    format!("Media/DCIM/101APPLE/{file}"),
                    format!("Media/{file}"),
                    format!("HomeDomain/Media/{file}"),
                    format!("CameraRollDomain/Media/{file}"),
                ];
                for cand in &candidates {
                    if let Ok(Some(fref)) = view.open(cand) {
                        resolved_path = fref.view_path().unwrap_or(cand).to_string();
                        resolved_ref = Some(fref);
                        break;
                    }
                }
            }
            (resolved_path, resolved_ref)
        };
        let mut rec = run
            .record_values(
                "ios.airdrop_transfer",
                vec![
                    dir_label.into(),
                    file.into(),
                    crate::backupdb::ts_or_nil(start.and_then(crate::backupdb::apple)),
                    (if peer.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(peer)
                    }),
                    "AirDrop（CoreDuet 分享动作）".into(),
                    detail.into(),
                    (if media_path.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(media_path)
                    }),
                ],
            )
            .severity(Severity::High)
            .source(
                crate::util::node_source(f)
                    .note(format!("interactionC.db ZINTERACTIONS row #{:?}", pk)),
            );
        if let Some(mf) = maybe_fref {
            rec = rec.source(mf.source().note("AirDrop 关联媒体文件"));
        }
        rec.emit()?;
        n += 1;
    }
    // 全量交互残留记录（ios.coreduet_interaction）
    n += parse_all_interactions(run, f, &conn, &attachment_utis)?;
    Ok(n)
}

/// 解析 interactionC.db 全量 ZINTERACTIONS 记录 → `ios.coreduet_interaction`。
fn parse_all_interactions(
    run: &Run,
    f: &FileRef,
    conn: &rusqlite::Connection,
    attachment_utis: &HashMap<i64, Vec<String>>,
) -> lovelycarve_plugin::Result<usize> {
    if !crate::backupdb::table_exists(conn, "ZINTERACTIONS") {
        return Ok(0);
    }
    // 联系人映射
    let mut contact_by_pk: HashMap<i64, String> = HashMap::new();
    if crate::backupdb::table_exists(conn, "ZCONTACTS") {
        let cc = crate::backupdb::cols(conn, "ZCONTACTS");
        let name_col = crate::backupdb::pick(&cc, &["ZDISPLAYNAME"]).unwrap_or("ZDISPLAYNAME");
        let id_col = crate::backupdb::pick(&cc, &["ZIDENTIFIER"]).unwrap_or("ZIDENTIFIER");
        let custom_col =
            crate::backupdb::pick(&cc, &["ZCUSTOMIDENTIFIER"]).unwrap_or("ZCUSTOMIDENTIFIER");
        let sql =
            format!("SELECT [Z_PK], [{name_col}], [{id_col}], [{custom_col}] FROM [ZCONTACTS]");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let Ok(Some(pk)) = r.get::<_, Option<i64>>(0) {
                        let name = r
                            .get::<_, Option<String>>(1)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let id = r
                            .get::<_, Option<String>>(2)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let custom = r
                            .get::<_, Option<String>>(3)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let display = if !name.is_empty() {
                            name
                        } else if !id.is_empty() {
                            id
                        } else {
                            custom
                        };
                        if !display.is_empty() {
                            contact_by_pk.insert(pk, display);
                        }
                    }
                }
            }
        }
    }

    // 接收方映射
    let mut recipients_by_inter: HashMap<i64, Vec<String>> = HashMap::new();
    if crate::backupdb::table_exists(conn, "Z_2INTERACTIONRECIPIENT")
        && crate::backupdb::table_exists(conn, "ZCONTACTS")
    {
        let bc = crate::backupdb::cols(conn, "Z_2INTERACTIONRECIPIENT");
        if let (Some(rc), Some(ic)) = (
            crate::backupdb::pick(&bc, &["Z_2RECIPIENTS"]),
            crate::backupdb::pick(&bc, &["Z_3INTERACTIONRECIPIENT"]),
        ) {
            let sql = format!(
                "SELECT j.[{ic}], c.[ZDISPLAYNAME], c.[ZIDENTIFIER] FROM [Z_2INTERACTIONRECIPIENT] j \
                 LEFT JOIN [ZCONTACTS] c ON c.[Z_PK] = j.[{rc}]"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(inter)), Ok(name), Ok(id)) = (
                            r.get::<_, Option<i64>>(0),
                            r.get::<_, Option<String>>(1),
                            r.get::<_, Option<String>>(2),
                        ) {
                            let label = name.or(id).unwrap_or_default();
                            if !label.is_empty() {
                                recipients_by_inter.entry(inter).or_default().push(label);
                            }
                        }
                    }
                }
            }
        }
    }

    let ic = crate::backupdb::cols(conn, "ZINTERACTIONS");
    let q = |want: &[&str]| -> String {
        match crate::backupdb::pick(&ic, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let columns = vec![
        q(&["Z_PK"]),
        q(&["ZSTARTDATE"]),
        q(&["ZENDDATE"]),
        q(&["ZBUNDLEID"]),
        q(&["ZTARGETBUNDLEID"]),
        q(&["ZDIRECTION"]),
        q(&["ZMECHANISM"]),
        q(&["ZSENDER"]),
        q(&["ZGROUPNAME"]),
        q(&["ZDOMAINIDENTIFIER"]),
        q(&["ZCONTENTURL"]),
        q(&["ZACCOUNT"]),
    ];
    let sql = format!(
        "SELECT {} FROM [ZINTERACTIONS] ORDER BY [ZSTARTDATE]",
        columns.join(", ")
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
        let pk = r.get::<_, Option<i64>>(0).ok().flatten().unwrap_or(-1);
        let start = r.get::<_, Option<f64>>(1).ok().flatten();
        let end = r.get::<_, Option<f64>>(2).ok().flatten();
        let bundle_id = r
            .get::<_, Option<String>>(3)
            .ok()
            .flatten()
            .unwrap_or_default();
        let target_bundle_id = r
            .get::<_, Option<String>>(4)
            .ok()
            .flatten()
            .unwrap_or_default();
        let dir_val = r.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(-1);
        let mech_val = r.get::<_, Option<i64>>(6).ok().flatten().unwrap_or(-1);
        let sender_pk = r.get::<_, Option<i64>>(7).ok().flatten();
        let group_name = r
            .get::<_, Option<String>>(8)
            .ok()
            .flatten()
            .unwrap_or_default();
        let domain_id = r
            .get::<_, Option<String>>(9)
            .ok()
            .flatten()
            .unwrap_or_default();
        let content_url = r
            .get::<_, Option<String>>(10)
            .ok()
            .flatten()
            .unwrap_or_default();
        let account = r
            .get::<_, Option<String>>(11)
            .ok()
            .flatten()
            .unwrap_or_default();

        let app_name = match bundle_id.as_str() {
            "ph.telegra.Telegraph" => "Telegram",
            "net.whatsapp.WhatsApp" => "WhatsApp",
            "com.apple.MobileSMS" => "短信 (iMessage)",
            "com.apple.mobilephone" => "电话 (Phone)",
            "com.apple.facetime" => "FaceTime",
            "com.apple.mobileslideshow" => "照片 (Photos)",
            "com.apple.DocumentsApp" => "文件 (Files)",
            "com.apple.mobilesafari" => "Safari 浏览器",
            "com.apple.mobilemail" => "邮件 (Mail)",
            "com.apple.UIKit.activity.AirDrop" => "隔空投送 (AirDrop)",
            _ => {
                if bundle_id.starts_with("com.apple.") {
                    "Apple 系统应用"
                } else {
                    ""
                }
            }
        };

        let direction = match dir_val {
            0 => "接收",
            1 => "发送",
            _ => "未知",
        };

        let mechanism = match mech_val {
            1 => "短信/iMessage (1)".to_string(),
            4 => "系统通话 (4)".to_string(),
            13 => "系统分享 (Share Sheet) (13)".to_string(),
            16 => "电话/音频通话 (16)".to_string(),
            20 => "应用内交互/广播 (20)".to_string(),
            21 => "第三方应用分享 (21)".to_string(),
            _ if mech_val >= 0 => format!("机制 ({mech_val})"),
            _ => String::new(),
        };

        let sender = sender_pk
            .and_then(|id| contact_by_pk.get(&id).cloned())
            .unwrap_or_default();

        let recipients = recipients_by_inter
            .get(&pk)
            .map(|list| list.join("、"))
            .unwrap_or_default();

        let attachments = attachment_utis
            .get(&pk)
            .map(|list| list.join("、"))
            .unwrap_or_default();

        let is_airdrop = target_bundle_id.to_lowercase().contains("airdrop")
            || bundle_id.to_lowercase().contains("airdrop");
        let has_attachment = !attachments.is_empty();

        let mut rec = run
            .record_values(
                "ios.coreduet_interaction",
                vec![
                    crate::backupdb::ts_or_nil(start.and_then(crate::backupdb::apple)),
                    crate::backupdb::ts_or_nil(end.and_then(crate::backupdb::apple)),
                    (if app_name.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(app_name)
                    }),
                    (if bundle_id.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(bundle_id)
                    }),
                    direction.into(),
                    (if mechanism.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(mechanism)
                    }),
                    (if sender.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(sender)
                    }),
                    (if recipients.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(recipients)
                    }),
                    (if group_name.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(group_name)
                    }),
                    (if domain_id.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(domain_id)
                    }),
                    (if attachments.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(attachments)
                    }),
                    (if content_url.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(content_url)
                    }),
                    (if account.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(account)
                    }),
                    (if target_bundle_id.is_empty() {
                        Value::Nil
                    } else {
                        Value::from(target_bundle_id)
                    }),
                    Value::from(pk),
                ],
            )
            .source(
                crate::util::node_source(f)
                    .note(format!("interactionC.db ZINTERACTIONS row #{pk}")),
            );

        if is_airdrop || has_attachment {
            rec = rec.severity(Severity::Info);
        }
        rec.emit()?;
        n += 1;
    }
    Ok(n)
}

/// 交互 pk → 附件 UTI 列表（`Z_1INTERACTIONS` 桥表 + `ZATTACHMENT`）。
fn attachment_utis_by_interaction(conn: &rusqlite::Connection) -> HashMap<i64, Vec<String>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    if !crate::backupdb::table_exists(conn, "ZATTACHMENT")
        || !crate::backupdb::table_exists(conn, "Z_1INTERACTIONS")
    {
        return out;
    }
    let bc = crate::backupdb::cols(conn, "Z_1INTERACTIONS");
    let (att_col, inter_col) = match (
        crate::backupdb::pick(&bc, &["Z_1ATTACHMENTS"]),
        crate::backupdb::pick(&bc, &["Z_3INTERACTIONS"]),
    ) {
        (Some(a), Some(i)) => (a.to_string(), i.to_string()),
        _ => return out,
    };
    let ac = crate::backupdb::cols(conn, "ZATTACHMENT");
    let uti_col = match crate::backupdb::pick(&ac, &["ZUTI"]) {
        Some(x) => x.to_string(),
        None => return out,
    };
    let sql = format!(
        "SELECT j.[{inter_col}], a.[{uti_col}] FROM [Z_1INTERACTIONS] j \
         LEFT JOIN [ZATTACHMENT] a ON a.[Z_PK] = j.[{att_col}]"
    );
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                if let (Ok(Some(inter)), Ok(Some(uti))) =
                    (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                {
                    if !uti.is_empty() {
                        let entry = out.entry(inter).or_default();
                        if !entry.contains(&uti) {
                            entry.push(uti);
                        }
                    }
                }
            }
        }
    }
    out
}

/// com.apple.sharingd.plist → `ios.airdrop_identity`（本机 AirDrop 标识）。
pub fn identity(run: &Run, f: &FileRef) -> lovelycarve_plugin::Result<usize> {
    let bytes = match f.read_all(16 << 20) {
        Ok(b) => b,
        Err(e) => {
            run.warn_file(f, "READ", format!("sharingd plist 读取失败：{e}"))?;
            return Ok(0);
        }
    };
    let Some(root) = crate::util::read_plist(&bytes) else {
        run.warn_file(f, "PLIST_PARSE", "sharingd plist 解析失败")?;
        return Ok(0);
    };
    let Some(d) = root.as_dictionary() else {
        return Ok(0);
    };
    let mut n = 0usize;
    for (k, name) in [
        (
            "AirDropID",
            "本机 AirDrop 通告地址（通常为 AWDL/Wi-Fi 接口地址）",
        ),
        ("AirDropRandomHashUUIDKey2", "AirDrop 随机哈希 UUID"),
        (
            "HashManager-LastDeviceIDHashKey",
            "设备 ID 哈希（HashManager）",
        ),
    ] {
        let Some(v) = d.get(k).and_then(PValue::as_string) else {
            continue;
        };
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        run.record_values(
            "ios.airdrop_identity",
            vec![
                name.into(),
                v.to_string().into(),
                format!("来源 {k}").into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}
