//! CoreDuet 交互库（`interactionC.db`）深度解析 —— 分类 `ginteraction`。
//!
//! 取证手册卷一全表逐列手册：11 张表、84 列（iOS 16 v11 / iOS 17 v13）。
//! 本模块覆盖：
//! - `ios.coreduet_event`：ZINTERACTIONS 交互事件全量（来源 App、目标渠道、
//!   方向、机制、群名、对端标识/派生意图、账号、收发方、附件、时间轴）。
//! - `ios.coreduet_contact`：ZCONTACTS 联系人/设备节点（长期收发计数与首末
//!   时间——ZINTERACTIONS 会滚动清理，这些计数永不重置）。
//! - `ios.coreduet_attachment`：ZATTACHMENT 传输工件（UTI、照片 UUID、URL/文本）。
//! - `ios.coreduet_keyword`：ZKEYWORDS 关键词（仅日历/协作事件）。
//! - `ios.coreduet_store`：库元信息（ZVERSION 创建时间、Z_METADATA 实例 UUID 与
//!   模型版本、Z_PRIMARYKEY 各实体主键峰值与被删数量）。
//! - `ios.airdrop_transfer`：AirDrop 分享动作（ZTARGETBUNDLEID=AirDrop 的子集，
//!   是 AirDrop 收发的权威记录，详见 `airdrop` 模块的口径说明）。
//!
//! 时间基准：Mac Absolute Time（2001-01-01 起），Unix = 值 + 978307200。

use std::collections::HashMap;

use lovelycarve_plugin::{FileRef, Run, Value};
use rusqlite::Connection;

use crate::backupdb::{apple, cols, open_db, pick, table_exists, ts_or_nil};

/// ZDIRECTION —— 事件方向。
fn direction_label(v: i64) -> &'static str {
    match v {
        0 => "传入（收到）",
        1 => "传出（本机发起）",
        2 => "双向",
        3 => "未知/忽略",
        _ => "未知",
    }
}

/// ZMECHANISM —— 通道类别。
fn mechanism_label(v: i64) -> &'static str {
    match v {
        0 => "第三方 VoIP 音频通话 / 默认",
        1 => "邮件（Mail）",
        2 => "旧版短信",
        3 => "旧版语音呼叫",
        4 => "即时消息（IM / 短信）",
        10 => "日历",
        11 => "FaceTime",
        12 => "第三方 VoIP 视频通话",
        13 => "系统分享面板（Share Sheet / AirDrop）",
        16 => "蜂窝电话（InCallService）",
        20 => "分享面板一键直达联系人",
        _ => "未知通道",
    }
}

/// ZCONTACTS.ZTYPE —— 账号句柄类型。
fn handle_type_label(v: i64) -> &'static str {
    match v {
        0 => "平台内部 ID / 群组 JID",
        1 => "电话号码",
        2 => "电子邮箱",
        3 => "字母数字服务号",
        _ => "",
    }
}

/// 解析入口：interactionC.db → 各类记录。
pub fn scan(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = match open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            run.warn_file(
                f,
                "DB_OPEN",
                format!("交互库：interactionC.db 打开失败：{e}"),
            )?;
            return Ok(0);
        }
    };
    if !table_exists(&conn, "ZINTERACTIONS") {
        run.warn_file(f, "NO_TABLE", "interactionC.db 缺少 ZINTERACTIONS 表")?;
        return Ok(0);
    }
    let mut n = 0usize;
    n += store_info(run, f, &conn)?;
    n += events(run, f, &conn)?;
    n += contacts(run, f, &conn)?;
    n += attachments(run, f, &conn)?;
    n += keywords(run, f, &conn)?;
    n += crate::airdrop::coreduet(run, f, sides)?;
    Ok(n)
}

/// 库元信息（`ios.coreduet_store`）：创建时间、实例 UUID、模型版本、各实体
/// 主键峰值与被删数量（Z_MAX − 实际行数 = 硬删除/滚动清理数）。
fn store_info(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let mut n = 0usize;
    // ZVERSION：库创建时间（≈ 设备激活 / 抹除 / 重建基线）。
    if table_exists(conn, "ZVERSION") {
        let vc = cols(conn, "ZVERSION");
        let sql = format!(
            "SELECT {}, {} FROM [ZVERSION]",
            match pick(&vc, &["ZVERSION"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&vc, &["ZCREATIONDATE"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    let version = r.get::<_, Option<i64>>(0).ok().flatten();
                    let created = r.get::<_, Option<f64>>(1).ok().flatten();
                    run.record_values(
                        "ios.coreduet_store",
                        vec![
                            "库创建时间".into(),
                            Value::from(version.unwrap_or(-1)),
                            ts_or_nil(created.and_then(apple)),
                            (if version.is_some() {
                                Value::from(format!("store version {}", version.unwrap_or(-1)))
                            } else {
                                Value::Nil
                            }),
                        ],
                    )
                    .source(crate::util::node_source(f))
                    .emit()?;
                    n += 1;
                }
            }
        }
    }
    // Z_METADATA：实例 UUID + 模型版本标识（bplist 里的 NSStoreModelVersionIdentifiers）。
    if table_exists(conn, "Z_METADATA") {
        let mc = cols(conn, "Z_METADATA");
        let uuid_col = pick(&mc, &["Z_UUID", "ZUUID"]).map(|x| format!("[{x}]"));
        let plist_col = pick(&mc, &["Z_PLIST", "ZPLIST"]).map(|x| format!("[{x}]"));
        if let (Some(uuid_col), Some(plist_col)) = (uuid_col, plist_col) {
            let sql = format!("SELECT {uuid_col}, {plist_col} FROM [Z_METADATA]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        let uuid = r
                            .get::<_, Option<String>>(0)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let blob = r.get::<_, Option<Vec<u8>>>(1).ok().flatten();
                        let mut versions = Vec::new();
                        if let Some(bytes) = blob.as_deref() {
                            if let Some(value) = crate::util::read_plist(bytes) {
                                if let Some(dict) = value.as_dictionary() {
                                    if let Some(plist::Value::Array(list)) =
                                        dict.get("NSStoreModelVersionIdentifiers")
                                    {
                                        for item in list {
                                            if let Some(text) = item.as_string() {
                                                versions.push(text.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // value 字段是整数型——模型版本号；UUID 与版本清单放 detail。
                        let model_version = versions
                            .first()
                            .and_then(|text| text.trim().parse::<i64>().ok())
                            .unwrap_or(-1);
                        let mut detail = String::new();
                        if !uuid.is_empty() {
                            detail.push_str(&format!("实例 UUID {uuid}"));
                        }
                        if !versions.is_empty() {
                            if !detail.is_empty() {
                                detail.push_str("；");
                            }
                            detail.push_str(&format!("模型版本 {}", versions.join("、")));
                        }
                        run.record_values(
                            "ios.coreduet_store",
                            vec![
                                "实例与模型版本".into(),
                                Value::from(model_version),
                                Value::Nil,
                                (if detail.is_empty() {
                                    Value::Nil
                                } else {
                                    Value::from(detail)
                                }),
                            ],
                        )
                        .source(crate::util::node_source(f))
                        .emit()?;
                        n += 1;
                    }
                }
            }
        }
    }
    // Z_PRIMARYKEY：实体注册表 + 主键峰值（Z_MAX）与被删数量。
    if table_exists(conn, "Z_PRIMARYKEY") {
        // 各实体的实际行数。
        let mut live: HashMap<i64, i64> = HashMap::new();
        for (ent, table) in [
            (1i64, "ZADDITIONALASSETATTRIBUTES"),
            (2, "ZCONTACTS"),
            (3, "ZINTERACTIONS"),
            (4, "ZKEYWORDS"),
            (39, "ZINTERNALRESOURCE"),
            (47, "ZPERSON"),
        ] {
            if table_exists(conn, table) {
                if let Ok(mut stmt) = conn.prepare(&format!("SELECT COUNT(*) FROM [{table}]")) {
                    if let Ok(mut rows) = stmt.query([]) {
                        if let Ok(Some(r)) = rows.next() {
                            if let Ok(count) = r.get::<_, i64>(0) {
                                live.insert(ent, count);
                            }
                        }
                    }
                }
            }
        }
        if let Ok(mut stmt) = conn.prepare("SELECT [Z_ENT], [Z_NAME], [Z_MAX] FROM [Z_PRIMARYKEY]")
        {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    let ent = r.get::<_, Option<i64>>(0).ok().flatten().unwrap_or(-1);
                    let name = r
                        .get::<_, Option<String>>(1)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let max = r.get::<_, Option<i64>>(2).ok().flatten().unwrap_or(0);
                    let live_count = live.get(&ent).copied();
                    let deleted = match live_count {
                        Some(count) => (max - count).max(0),
                        None => 0,
                    };
                    run.record_values(
                        "ios.coreduet_store",
                        vec![
                            "实体注册表".into(),
                            Value::from(ent),
                            ts_or_nil(None),
                            Value::from(format!(
                                "{name}：主键峰值 {max}，现存 {}{}",
                                live_count
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                if deleted > 0 {
                                    format!("，已删除/滚动清理 {deleted}")
                                } else {
                                    String::new()
                                }
                            )),
                        ],
                    )
                    .source(crate::util::node_source(f))
                    .emit()?;
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// 交互事件全量（`ios.coreduet_event`）。
fn events(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let ic = cols(conn, "ZINTERACTIONS");
    let q = |want: &[&str]| -> String {
        match pick(&ic, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let columns = vec![
        q(&["Z_PK"]),
        q(&["ZBUNDLEID"]),
        q(&["ZTARGETBUNDLEID"]),
        q(&["ZDIRECTION"]),
        q(&["ZMECHANISM"]),
        q(&["ZRECIPIENTCOUNT"]),
        q(&["ZGROUPNAME"]),
        q(&["ZDOMAINIDENTIFIER"]),
        q(&["ZDERIVEDINTENTIDENTIFIER"]),
        q(&["ZACCOUNT"]),
        q(&["ZSENDER"]),
        q(&["ZCONTENTURL"]),
        q(&["ZISRESPONSE"]),
        q(&["ZUUID"]),
        q(&["ZSTARTDATE"]),
        q(&["ZENDDATE"]),
        q(&["ZCREATIONDATE"]),
        q(&["ZUPDATEDATE"]),
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

    // 联系人：id → (显示名, 标识, 自定义标识)
    let mut contact_by_id: HashMap<i64, (String, String, String)> = HashMap::new();
    if table_exists(conn, "ZCONTACTS") {
        let cc = cols(conn, "ZCONTACTS");
        let sql = format!(
            "SELECT [Z_PK], {}, {}, {} FROM [ZCONTACTS]",
            match pick(&cc, &["ZDISPLAYNAME"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&cc, &["ZIDENTIFIER"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&cc, &["ZCUSTOMIDENTIFIER"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let Ok(Some(pk)) = r.get::<_, Option<i64>>(0) {
                        let name = r
                            .get::<_, Option<String>>(1)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let handle = r
                            .get::<_, Option<String>>(2)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let custom = r
                            .get::<_, Option<String>>(3)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        contact_by_id.insert(pk, (name, handle, custom));
                    }
                }
            }
        }
    }
    // 接收方：交互 pk → 联系人名列表。
    let mut recipients_by_inter: HashMap<i64, Vec<String>> = HashMap::new();
    if table_exists(conn, "Z_2INTERACTIONRECIPIENT") {
        let bc = cols(conn, "Z_2INTERACTIONRECIPIENT");
        if let (Some(rc), Some(ic)) = (
            pick(&bc, &["Z_2RECIPIENTS"]),
            pick(&bc, &["Z_3INTERACTIONRECIPIENT"]),
        ) {
            let sql = format!(
                "SELECT j.[{ic}], c.[ZDISPLAYNAME] FROM [Z_2INTERACTIONRECIPIENT] j \
                 LEFT JOIN [ZCONTACTS] c ON c.[Z_PK] = j.[{rc}]"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(inter)), Ok(Some(name))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                        {
                            if !name.is_empty() {
                                recipients_by_inter.entry(inter).or_default().push(name);
                            }
                        }
                    }
                }
            }
        }
    }
    // 附件：交互 pk → (数量, UTI 列表)。
    let mut attachments_by_inter: HashMap<i64, (usize, Vec<String>)> = HashMap::new();
    if table_exists(conn, "Z_1INTERACTIONS") && table_exists(conn, "ZATTACHMENT") {
        let bc = cols(conn, "Z_1INTERACTIONS");
        let ac = cols(conn, "ZATTACHMENT");
        if let (Some(ac_col), Some(ic_col)) = (
            pick(&bc, &["Z_1ATTACHMENTS"]),
            pick(&bc, &["Z_3INTERACTIONS"]),
        ) {
            let uti_col = pick(&ac, &["ZUTI"]).map(|x| format!("[{x}]"));
            let sql = match uti_col {
                Some(uti) => format!(
                    "SELECT j.[{ic_col}], a.{uti} FROM [Z_1INTERACTIONS] j \
                     LEFT JOIN [ZATTACHMENT] a ON a.[Z_PK] = j.[{ac_col}]"
                ),
                None => format!("SELECT j.[{ic_col}], NULL FROM [Z_1INTERACTIONS] j"),
            };
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let Ok(Some(inter)) = r.get::<_, Option<i64>>(0) {
                            let uti = r
                                .get::<_, Option<String>>(1)
                                .ok()
                                .flatten()
                                .unwrap_or_default();
                            let entry =
                                attachments_by_inter.entry(inter).or_insert((0, Vec::new()));
                            entry.0 += 1;
                            if !uti.is_empty() {
                                entry.1.push(uti);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let pk = r.get::<_, Option<i64>>(0).ok().flatten().unwrap_or(-1);
        let source_app = r
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        let target_app = r
            .get::<_, Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_default();
        let direction = r.get::<_, Option<i64>>(3).ok().flatten().unwrap_or(-1);
        let mechanism = r.get::<_, Option<i64>>(4).ok().flatten().unwrap_or(-1);
        let recipient_count = r.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(0);
        let group_name = r
            .get::<_, Option<String>>(6)
            .ok()
            .flatten()
            .unwrap_or_default();
        let domain_id = r
            .get::<_, Option<String>>(7)
            .ok()
            .flatten()
            .unwrap_or_default();
        let derived_intent = r
            .get::<_, Option<String>>(8)
            .ok()
            .flatten()
            .unwrap_or_default();
        let account = r
            .get::<_, Option<String>>(9)
            .ok()
            .flatten()
            .unwrap_or_default();
        let sender_pk = r.get::<_, Option<i64>>(10).ok().flatten();
        let content_url = r
            .get::<_, Option<String>>(11)
            .ok()
            .flatten()
            .unwrap_or_default();
        let is_response = r.get::<_, Option<i64>>(12).ok().flatten().unwrap_or(0);
        let uuid = r
            .get::<_, Option<String>>(13)
            .ok()
            .flatten()
            .unwrap_or_default();
        let start = r.get::<_, Option<f64>>(14).ok().flatten();
        let end = r.get::<_, Option<f64>>(15).ok().flatten();
        let created = r.get::<_, Option<f64>>(16).ok().flatten();
        let updated = r.get::<_, Option<f64>>(17).ok().flatten();

        let (sender_name, sender_handle, _) = sender_pk
            .and_then(|id| contact_by_id.get(&id).cloned())
            .unwrap_or((String::new(), String::new(), String::new()));
        let recipients = recipients_by_inter
            .get(&pk)
            .map(|list| list.join("、"))
            .unwrap_or_default();
        let (att_count, att_utis) = attachments_by_inter
            .get(&pk)
            .cloned()
            .unwrap_or((0, Vec::new()));
        // 通话时长：ZENDDATE − ZSTARTDATE（仅 mechanism=16 有意义）。
        let duration = match (start, end) {
            (Some(s), Some(e)) => (e - s).max(0.0),
            _ => 0.0,
        };
        // 分享类事件（13/20）附说明。
        let mut notes = String::new();
        if mechanism == 13 || mechanism == 20 {
            notes.push_str("分享动作（渠道确证）；该库只记录分享发起与对端选择，不记录传输成败");
        }
        if !group_name.is_empty() {
            notes.push_str("群名来自系统（应用卸载后仍保留）");
        }
        run.record_values(
            "ios.coreduet_event",
            vec![
                Value::from(pk),
                source_app.into(),
                (if target_app.is_empty() {
                    Value::Nil
                } else {
                    Value::from(target_app)
                }),
                direction_label(direction).into(),
                Value::from(direction),
                mechanism_label(mechanism).into(),
                Value::from(mechanism),
                Value::from(recipient_count),
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
                (if derived_intent.is_empty() {
                    Value::Nil
                } else {
                    Value::from(derived_intent)
                }),
                (if account.is_empty() {
                    Value::Nil
                } else {
                    Value::from(account)
                }),
                (if sender_name.is_empty() {
                    Value::Nil
                } else {
                    Value::from(sender_name)
                }),
                (if sender_handle.is_empty() {
                    Value::Nil
                } else {
                    Value::from(sender_handle)
                }),
                (if recipients.is_empty() {
                    Value::Nil
                } else {
                    Value::from(recipients)
                }),
                Value::from(att_count),
                (if att_utis.is_empty() {
                    Value::Nil
                } else {
                    Value::from(att_utis.join("、"))
                }),
                (if content_url.is_empty() {
                    Value::Nil
                } else {
                    Value::from(content_url)
                }),
                (if is_response == 1 { "是" } else { "" }).into(),
                ts_or_nil(start.and_then(apple)),
                ts_or_nil(end.and_then(apple)),
                Value::from(duration),
                ts_or_nil(created.and_then(apple)),
                ts_or_nil(updated.and_then(apple)),
                (if uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(uuid)
                }),
                (if notes.is_empty() {
                    Value::Nil
                } else {
                    Value::from(notes)
                }),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 联系人 / 设备节点（`ios.coreduet_contact`）。
///
/// ZINTERACTIONS 会滚动清理（1~4 个月），但 ZCONTACTS 的收发计数与首末时间
/// **永不重置**——即使交互记录已滚掉，仍能证明"曾与谁联系"。
pub fn contacts(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZCONTACTS") {
        return Ok(0);
    }
    let cc = cols(conn, "ZCONTACTS");
    let c = |want: &[&str]| -> String {
        match pick(&cc, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let columns = vec![
        c(&["ZDISPLAYNAME"]),
        c(&["ZIDENTIFIER"]),
        c(&["ZCUSTOMIDENTIFIER"]),
        c(&["ZTYPE"]),
        c(&["ZPERSONID"]),
        c(&["ZPERSONIDTYPE"]),
        c(&["ZINCOMINGSENDERCOUNT"]),
        c(&["ZOUTGOINGRECIPIENTCOUNT"]),
        c(&["ZINCOMINGRECIPIENTCOUNT"]),
        c(&["ZFIRSTINCOMINGSENDERDATE"]),
        c(&["ZLASTINCOMINGSENDERDATE"]),
        c(&["ZFIRSTOUTGOINGRECIPIENTDATE"]),
        c(&["ZLASTOUTGOINGRECIPIENTDATE"]),
        c(&["ZCREATIONDATE"]),
    ];
    let sql = format!("SELECT {} FROM [ZCONTACTS]", columns.join(", "));
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let name = r
            .get::<_, Option<String>>(0)
            .ok()
            .flatten()
            .unwrap_or_default();
        let handle = r
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        let custom = r
            .get::<_, Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_default();
        let handle_type = r.get::<_, Option<i64>>(3).ok().flatten().unwrap_or(-1);
        let person_id = r
            .get::<_, Option<String>>(4)
            .ok()
            .flatten()
            .unwrap_or_default();
        let person_id_type = r.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(-1);
        let recv = r.get::<_, Option<i64>>(6).ok().flatten().unwrap_or(0);
        let sent = r.get::<_, Option<i64>>(7).ok().flatten().unwrap_or(0);
        let cc_count = r.get::<_, Option<i64>>(8).ok().flatten().unwrap_or(0);
        let first_recv = r.get::<_, Option<f64>>(9).ok().flatten();
        let last_recv = r.get::<_, Option<f64>>(10).ok().flatten();
        let first_sent = r.get::<_, Option<f64>>(11).ok().flatten();
        let last_sent = r.get::<_, Option<f64>>(12).ok().flatten();
        let created = r.get::<_, Option<f64>>(13).ok().flatten();
        if name.is_empty() && handle.is_empty() && custom.is_empty() {
            continue;
        }
        // 空句柄 + 有显示名 = 设备类（AirDrop 对端即此类）。
        let kind = if handle.trim().is_empty() && custom.trim().is_empty() {
            "设备/未标识对端"
        } else {
            "联系人"
        };
        run.record_values(
            "ios.coreduet_contact",
            vec![
                name.into(),
                (if handle.is_empty() {
                    Value::Nil
                } else {
                    Value::from(handle)
                }),
                (if custom.is_empty() {
                    Value::Nil
                } else {
                    Value::from(custom)
                }),
                handle_type_label(handle_type).into(),
                (if person_id.is_empty() {
                    Value::Nil
                } else {
                    Value::from(person_id)
                }),
                (if person_id_type == 3 {
                    "已关联系统通讯录"
                } else {
                    "未关联通讯录"
                })
                .into(),
                Value::from(recv),
                Value::from(sent),
                Value::from(cc_count),
                ts_or_nil(first_recv.filter(|v| *v > 0.0).and_then(apple)),
                ts_or_nil(last_recv.filter(|v| *v > 0.0).and_then(apple)),
                ts_or_nil(first_sent.filter(|v| *v > 0.0).and_then(apple)),
                ts_or_nil(last_sent.filter(|v| *v > 0.0).and_then(apple)),
                ts_or_nil(created.and_then(apple)),
                kind.into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 传输工件（`ios.coreduet_attachment`）。
///
/// 取证手册卷一 §5：**这是唯一记录「实际传输了什么文件」的表**——UTI、字节数、
/// Photos 本地标识（`ZPHOTOLOCALIDENTIFIER` 前 36 位即 `Photos.sqlite`
/// `ZASSET.ZUUID`）、内容 URL/文本、照片中人物、AI 场景描述。
pub fn attachments(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZATTACHMENT") {
        return Ok(0);
    }
    let ac = cols(conn, "ZATTACHMENT");
    let att = |want: &[&str]| -> String {
        match pick(&ac, want) {
            Some(x) => format!("[{x}]"),
            None => "NULL".into(),
        }
    };
    let columns = vec![
        "[Z_PK]".to_string(),
        att(&["ZUTI"]),
        att(&["ZSIZEINBYTES"]),
        att(&["ZPHOTOLOCALIDENTIFIER"]),
        att(&["ZCONTENTURL"]),
        att(&["ZCONTENTTEXT"]),
        att(&["ZPERSONINPHOTO"]),
        att(&["ZPHOTOSCENEDESCRIPTOR"]),
    ];
    let sql = format!("SELECT {} FROM [ZATTACHMENT]", columns.join(", "));
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    // 附件 → 交互（桥表）。
    let mut link: HashMap<i64, i64> = HashMap::new();
    if table_exists(conn, "Z_1INTERACTIONS") {
        let bc = cols(conn, "Z_1INTERACTIONS");
        if let (Some(att_col), Some(inter_col)) = (
            pick(&bc, &["Z_1ATTACHMENTS"]),
            pick(&bc, &["Z_3INTERACTIONS"]),
        ) {
            let sql = format!("SELECT [{att_col}], [{inter_col}] FROM [Z_1INTERACTIONS]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(a)), Ok(Some(i))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                        {
                            link.insert(a, i);
                        }
                    }
                }
            }
        }
    }
    // 交互 → (来源 App, 目标渠道, 时间)。
    let mut meta: HashMap<i64, (String, String, Option<f64>)> = HashMap::new();
    if table_exists(conn, "ZINTERACTIONS") {
        let ic = cols(conn, "ZINTERACTIONS");
        let sql = format!(
            "SELECT [Z_PK], {}, {}, {} FROM [ZINTERACTIONS]",
            match pick(&ic, &["ZBUNDLEID"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&ic, &["ZTARGETBUNDLEID"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&ic, &["ZSTARTDATE"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let Ok(Some(pk)) = r.get::<_, Option<i64>>(0) {
                        let src = r
                            .get::<_, Option<String>>(1)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let dst = r
                            .get::<_, Option<String>>(2)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let time = r.get::<_, Option<f64>>(3).ok().flatten();
                        meta.insert(pk, (src, dst, time));
                    }
                }
            }
        }
    }
    // 交互 → 接收方名。
    let mut recipients: HashMap<i64, String> = HashMap::new();
    if table_exists(conn, "Z_2INTERACTIONRECIPIENT") && table_exists(conn, "ZCONTACTS") {
        let bc = cols(conn, "Z_2INTERACTIONRECIPIENT");
        if let (Some(rc), Some(ic)) = (
            pick(&bc, &["Z_2RECIPIENTS"]),
            pick(&bc, &["Z_3INTERACTIONRECIPIENT"]),
        ) {
            let sql = format!(
                "SELECT j.[{ic}], c.[ZDISPLAYNAME] FROM [Z_2INTERACTIONRECIPIENT] j \
                 LEFT JOIN [ZCONTACTS] c ON c.[Z_PK] = j.[{rc}]"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(inter)), Ok(Some(name))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<String>>(1))
                        {
                            if !name.is_empty() {
                                recipients.insert(inter, name);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let attachment_pk = r.get::<_, Option<i64>>(0).ok().flatten().unwrap_or(-1);
        let uti = r
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        let size = r.get::<_, Option<i64>>(2).ok().flatten().unwrap_or(0);
        let photo_uuid = r
            .get::<_, Option<String>>(3)
            .ok()
            .flatten()
            .unwrap_or_default();
        let url = r
            .get::<_, Option<String>>(4)
            .ok()
            .flatten()
            .unwrap_or_default();
        let text = r
            .get::<_, Option<String>>(5)
            .ok()
            .flatten()
            .unwrap_or_default();
        let person = r
            .get::<_, Option<String>>(6)
            .ok()
            .flatten()
            .unwrap_or_default();
        let scene = r
            .get::<_, Option<String>>(7)
            .ok()
            .flatten()
            .unwrap_or_default();
        if uti.is_empty() && url.is_empty() && text.is_empty() && photo_uuid.is_empty() {
            continue;
        }
        let interaction_pk = link.get(&attachment_pk).copied();
        let (src_app, target_app, start) = match interaction_pk.and_then(|i| meta.get(&i)) {
            Some((s, d, t)) => (s.clone(), d.clone(), *t),
            None => (String::new(), String::new(), None),
        };
        let recipient = interaction_pk
            .and_then(|i| recipients.get(&i).cloned())
            .unwrap_or_default();
        run.record_values(
            "ios.coreduet_attachment",
            vec![
                interaction_pk
                    .map(|v| Value::from(v as i64))
                    .unwrap_or(Value::Nil),
                src_app.into(),
                (if target_app.is_empty() {
                    Value::Nil
                } else {
                    Value::from(target_app)
                }),
                uti.into(),
                Value::from(size),
                (if photo_uuid.is_empty() {
                    Value::Nil
                } else {
                    Value::from(photo_uuid)
                }),
                (if url.is_empty() {
                    Value::Nil
                } else {
                    Value::from(url)
                }),
                (if text.is_empty() {
                    Value::Nil
                } else {
                    Value::from(text)
                }),
                (if person.is_empty() {
                    Value::Nil
                } else {
                    Value::from(person)
                }),
                (if scene.is_empty() {
                    Value::Nil
                } else {
                    Value::from(scene)
                }),
                ts_or_nil(start.and_then(apple)),
                (if recipient.is_empty() {
                    Value::Nil
                } else {
                    Value::from(recipient)
                }),
                (if interaction_pk.is_none() {
                    "孤儿附件（父交互已被滚动清理）".into()
                } else {
                    "".into()
                }),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// 关键词（`ios.coreduet_keyword`）——仅日历/协作事件会填充。
fn keywords(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    if !table_exists(conn, "ZKEYWORDS") {
        return Ok(0);
    }
    let kc = cols(conn, "ZKEYWORDS");
    let keyword_col = match pick(&kc, &["ZKEYWORD"]) {
        Some(x) => x.to_string(),
        None => return Ok(0),
    };
    // 关键词 ↔ 交互（Z_3KEYWORDS：Z_3INTERACTIONS1 → 交互，Z_4KEYWORDS → 关键词）。
    let mut inter_of: HashMap<i64, i64> = HashMap::new();
    if table_exists(conn, "Z_3KEYWORDS") {
        let bc = cols(conn, "Z_3KEYWORDS");
        if let (Some(kw), Some(inter)) = (
            pick(&bc, &["Z_4KEYWORDS"]),
            pick(&bc, &["Z_3INTERACTIONS1", "Z_3INTERACTIONS"]),
        ) {
            let sql = format!("SELECT [{kw}], [{inter}] FROM [Z_3KEYWORDS]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Ok(Some(k)), Ok(Some(i))) =
                            (r.get::<_, Option<i64>>(0), r.get::<_, Option<i64>>(1))
                        {
                            inter_of.insert(k, i);
                        }
                    }
                }
            }
        }
    }
    // 交互 → (来源 App, 时间)。
    let mut meta: HashMap<i64, (String, Option<f64>)> = HashMap::new();
    if table_exists(conn, "ZINTERACTIONS") {
        let ic = cols(conn, "ZINTERACTIONS");
        let sql = format!(
            "SELECT [Z_PK], {}, {} FROM [ZINTERACTIONS]",
            match pick(&ic, &["ZBUNDLEID"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
            match pick(&ic, &["ZSTARTDATE"]) {
                Some(x) => format!("[{x}]"),
                None => "NULL".into(),
            },
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let Ok(Some(pk)) = r.get::<_, Option<i64>>(0) {
                        let src = r
                            .get::<_, Option<String>>(1)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        let time = r.get::<_, Option<f64>>(2).ok().flatten();
                        meta.insert(pk, (src, time));
                    }
                }
            }
        }
    }
    let sql = format!("SELECT [Z_PK], [{keyword_col}] FROM [ZKEYWORDS]");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(0);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(0);
    };
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        run.check_cancelled()?;
        let kw_pk = r.get::<_, Option<i64>>(0).ok().flatten().unwrap_or(-1);
        let keyword = r
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .unwrap_or_default();
        if keyword.is_empty() {
            continue;
        }
        let inter = inter_of.get(&kw_pk).copied();
        let (src, time) = match inter.and_then(|i| meta.get(&i)) {
            Some((s, t)) => (s.clone(), *t),
            None => (String::new(), None),
        };
        run.record_values(
            "ios.coreduet_keyword",
            vec![
                keyword.into(),
                (if src.is_empty() {
                    Value::Nil
                } else {
                    Value::from(src)
                }),
                inter.map(|v| Value::from(v as i64)).unwrap_or(Value::Nil),
                ts_or_nil(time.and_then(apple)),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}
