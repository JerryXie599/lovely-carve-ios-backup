//! 第三方 App 数据库解析（逐 App 手写，口径来自对各家解析器的字符串级还原，
//! 表/列口径经静态分析各应用数据格式归纳。每 App 一个解析函数，按文件名路由。
//!
//! 记录类型复用通用 App 模型：
//! - ios.app_msg      ↔ 消息（ImMsgInfo 语义：发送者/接收者/内容/时间/方向）
//! - ios.app_user     ↔ 账户与联系人（ImUserInfo 语义）
//! - ios.app_search   ↔ 搜索历史（SearchHistory 语义）
//! - ios.app_geo      ↔ 位置与轨迹（GeoInfo 语义）
//! - ios.app_bill     ↔ 订单账单（Bill 语义）
//! - ios.app_media    ↔ 媒体与下载（MediaInfo 语义）
//! 各记录带 `app` 字段标注来源应用，标题模板拼 App 名+主体。

use crate::backupdb::open_db;
use crate::util::ts_plus8;
use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use rusqlite::Connection;

fn ts_or_nil(t: Option<Timestamp>) -> Value {
    t.map(|t| Value::from(t.as_str().to_string())).unwrap_or(Value::Nil)
}

/// Apple epoch 秒/纳秒（iOS App 常用）。
fn apple(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 { (v / 1e9) as i64 } else { v as i64 };
    ts_plus8(crate::util::APPLE_EPOCH + secs)
}

/// Unix epoch 秒/毫秒（App 更常用）。
fn unix(v: f64) -> Option<Timestamp> {
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e13 { (v / 1000.0) as i64 } else if v.abs() > 1e11 { (v / 1000.0) as i64 } else { v as i64 };
    ts_plus8(secs)
}

fn table_exists(conn: &Connection, t: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [t], |_| Ok(())).is_ok()
}

fn cols(conn: &Connection, t: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info([{t}])"))
        .and_then(|mut s| {
            let mut v = Vec::new();
            let mut rows = s.query([])?;
            while let Some(r) = rows.next()? {
                if let Ok(Some(n)) = r.get::<_, Option<String>>(1) {
                    v.push(n);
                }
            }
            Ok(v)
        })
        .unwrap_or_default()
}

fn pick<'a>(available: &'a [String], want: &[&str]) -> Option<&'a str> {
    want.iter().find_map(|w| available.iter().find(|c| c.eq_ignore_ascii_case(w)).map(|c| c.as_str()))
}

/// 打开 App 数据库（tmp 副本）。
fn app_db(f: &FileRef, sides: &[FileRef]) -> Result<(tempfile::TempDir, Connection), String> {
    open_db(f, sides)
}

// ================= 小红书（PrivateMessage.db / MMKV 搜索） =================
// 口径：XYPMMessageMessageModel（msgContent JSON: senderId/timestamp/content/ref_msgs）、
// XYPMMessageUserModel（user）、XYPHRecentSearchItem（MMKV 搜索词）。
pub fn xiaohongshu_msg(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in ["message_model", "MessageModel", "message"] {
        if !table_exists(&conn, table) {
            continue;
        }
        let c = cols(&conn, table);
        let Some(content) = pick(&c, &["msg_content", "msgContent", "content"]) else { continue };
        let ts = pick(&c, &["time_stamp", "timestamp", "time", "create_time"]);
        let chat = pick(&c, &["chat_id", "chatid"]);
        let sql = format!(
            "SELECT {}, {}{}, ROWID FROM [{}]",
            content,
            ts.map(|x| format!("[{x}],")).unwrap_or_default(),
            chat.map(|x| format!("[{x}]")).unwrap_or_else(|| "NULL".into()),
            table
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        while let Ok(Some(r)) = rows.next() {
            let content_v: Option<String> = r.get(0).unwrap_or(None);
            let ts_v: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let chat_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let text = content_v.unwrap_or_default();
            let (body, sender) = parse_json_field(&text, &["content", "desc"], &["sender_id", "senderid", "userId"]);
            run.record_values(
                "ios.app_msg",
                vec![
                    "小红书".into(),
                    chat_v.clone().unwrap_or_default().into(),
                    sender.into(),
                    body.into(),
                    ts_or_nil(unix(ts_v.unwrap_or_default())),
                    "".into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
        }
        if n > 0 {
            break;
        }
    }
    Ok(n)
}

/// 从 JSON 文本取第一个存在的字符串字段；日期字段单独取。
fn parse_json_field(text: &str, body_keys: &[&str], sender_keys: &[&str]) -> (String, String) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return (text.to_string(), String::new()),
    };
    let get_s = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(s) = v.get(k).and_then(serde_json::Value::as_str) {
                return s.to_string();
            }
            if let Some(n) = v.get(k).and_then(serde_json::Value::as_i64) {
                return n.to_string();
            }
        }
        String::new()
    };
    (get_s(body_keys), get_s(sender_keys))
}

// ================= 知乎（_message_zhihu_im.db / local_search.db） =================
pub fn zhihu_msg(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    // 知乎 IM 表：message / messagerelation（动态探测含 content/content_text 的表）
    let tables: Vec<String> = {
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'").map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        let mut v = Vec::new();
        while let Ok(Some(r)) = rows.next() {
            if let Ok(Some(t)) = r.get::<_, Option<String>>(0) {
                v.push(t);
            }
        }
        v
    };
    for table in &tables {
        let c = cols(&conn, table);
        let Some(content) = pick(&c, &["content", "content_text", "text", "body"]) else { continue };
        let Some(created) = pick(&c, &["created_time", "created_at", "timestamp", "time"]) else { continue };
        let member = pick(&c, &["member_id", "sender_id", "user_id", "from_user_id"]);
        let sql = format!(
            "SELECT [{content}], [{created}], {} ROWID FROM [{table}]",
            member.map(|m| format!("[{m}],")).unwrap_or_default()
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        while let Ok(Some(r)) = rows.next() {
            let body: Option<String> = r.get(0).unwrap_or(None);
            let ts_v: Option<f64> = r.get(1).unwrap_or(None);
            let sender: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            run.record_values(
                "ios.app_msg",
                vec![
                    "知乎".into(),
                    table.clone().into(),
                    sender.unwrap_or_default().into(),
                    body.unwrap_or_default().into(),
                    ts_or_nil(unix(ts_v.unwrap_or_default())),
                    "".into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
        }
    }
    Ok(n)
}

// ================= B 站（BBIMDB*.db） =================
// 口径：MessagesTable、MemberInfoTable、personal/groupNodes。
pub fn bilibili_msg(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in ["MessagesTable", "message", "messages"] {
        if !table_exists(&conn, table) {
            continue;
        }
        let c = cols(&conn, table);
        let Some(content) = pick(&c, &["content", "msg_content", "text"]) else { continue };
        let ts = pick(&c, &["create_time", "time", "timestamp", "creat_time"]);
        let sender = pick(&c, &["sender_uid", "sender_id", "member_uid", "owner_uid"]);
        let talker = pick(&c, &["talker_id", "chat_id", "receiver_id"]);
        let mut sel = vec![format!("[{content}]")];
        for x in [ts, sender, talker].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let sql = format!("SELECT {} FROM [{table}]", sel.join(", "));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = stmt.query([]).map_err(|e| lovelycarve_plugin::Error::new("SQL", e.to_string()))?;
        while let Ok(Some(r)) = rows.next() {
            let body: Option<String> = r.get(0).unwrap_or(None);
            let ts_v: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let sender_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let talker_v: Option<String> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            run.record_values(
                "ios.app_msg",
                vec![
                    "哔哩哔哩".into(),
                    talker_v.unwrap_or_default().into(),
                    sender_v.unwrap_or_default().into(),
                    body.unwrap_or_default().into(),
                    ts_or_nil(unix(ts_v.unwrap_or_default())),
                    "".into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
        }
        if n > 0 {
            break;
        }
    }
    Ok(n)
}

// ================= 通用 IM 消息兜底（IM Cache 类库：im_message/message/chat 等表） =================
/// 对未逐个适配的 App 库，按列名启发式解析消息表（表名/列名匹配才输出，宁缺毋滥）。
pub fn generic_im(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let tables: Vec<String> = {
        let mut stmt = match conn.prepare("SELECT name FROM sqlite_master WHERE type='table'") {
            Ok(s) => s,
            Err(_) => return Ok(0),
        };
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => return Ok(0),
        };
        let mut v = Vec::new();
        while let Ok(Some(r)) = rows.next() {
            if let Ok(Some(t)) = r.get::<_, Option<String>>(0) {
                v.push(t);
            }
        }
        v
    };
    let mut n = 0usize;
    'tables: for table in &tables {
        if n > 20000 {
            break;
        }
        let tl = table.to_lowercase();
        if !tl.contains("message") && !tl.contains("msg") && !tl.contains("chat") {
            continue;
        }
        let c = cols(&conn, table);
        let Some(content) = pick(&c, &["content", "msg_content", "msgContent", "text", "body", "message", "ztext", "rawcontent"]) else { continue };
        let ts = pick(&c, &["create_time", "created_at", "timestamp", "time", "msg_time", "send_time", "date", "date_sent", "date_received", "send_date", "zmessagedate"]);
        let sender = pick(&c, &["sender_id", "senderid", "sender", "from_uid", "from_id", "user_id", "member_id", "sender_jid", "fromjid", "author"]);
        let talker = pick(&c, &["chat_id", "talker_id", "group_id", "session_id", "conversation_id", "peer_id", "thread_id", "zcontactjid", "dialog_id", "target_id"]);
        let is_send = pick(&c, &["is_send", "is_sender", "from_me", "isself", "send_flag", "zisfromme"]);
        let mut sel = vec![format!("[{content}]")];
        for x in [ts, sender, talker, is_send].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let sql = format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut emitted_here = 0usize;
        while let Ok(Some(r)) = rows.next() {
            let body: Option<String> = r.get(0).unwrap_or(None);
            let ts_v: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let sender_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let talker_v: Option<String> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            let is_send_v: Option<i64> = if cnt > 4 { r.get(4).unwrap_or(None) } else { None };
            let raw = body.unwrap_or_default();
            let (b, s2) = if raw.starts_with('{') {
                parse_json_field(&raw, &["content", "desc", "text", "body"], &["sender_id", "senderId", "user_id"])
            } else {
                (raw, String::new())
            };
            let sender_out = if !s2.is_empty() { s2 } else { sender_v.clone().unwrap_or_default() };
            run.record_values(
                "ios.app_msg",
                vec![
                    app.into(),
                    talker_v.unwrap_or_default().into(),
                    sender_out.into(),
                    b.chars().take(4000).collect::<String>().into(),
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                    (match is_send_v {
                        Some(1) => "发送",
                        Some(0) => "接收",
                        _ => "",
                    })
                    .into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            emitted_here += 1;
            if emitted_here > 20000 {
                continue 'tables;
            }
        }
    }
    Ok(n)
}

/// 按文件名路由 App 库 → 专属或通用解析器。
pub fn route(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let name = f.name();
    let lower = name.to_lowercase();
    if let Some(app) = crate::apps_route::app_display_name(&lower) {
        return route_known(run, f, sides, app, &lower);
    }
    if let Some((app, cls)) = crate::apps_route_glued::glued_route(&lower) {
        return route_class(run, f, sides, app, cls);
    }
    match lower.as_str() {
        "privatemessage.db" => xiaohongshu_msg(run, f, sides),
        "_message_zhihu_im.db" => zhihu_msg(run, f, sides),
        n if n.starts_with("bbimdb") => bilibili_msg(run, f, sides),
        "chat.db" if view_contains(f, "kimi") => generic_im(run, f, sides, "Kimi"),
        "imcache.db" => generic_im(run, f, sides, "91视频"),
        "message.sqlite" => generic_im(run, f, sides, "KakaoTalk"),
        "talk.sqlite" => Ok(0), // Kakao Talk.sqlite 用户表在 user 解析处理
        "im_message.db" => generic_im(run, f, sides, "美团外卖"),
        "im_groupmessage.db" => generic_im(run, f, sides, "美团外卖群"),
        "message.db" => generic_im(run, f, sides, &guess_app(f)),
        "messages.db" => generic_im(run, f, sides, "易对"),
        "messages-store.sqlite" => generic_im(run, f, sides, "VK"),
        "fmdb.sqlite" => generic_im(run, f, sides, "想聊"),
        "zim_global.db" => generic_im(run, f, sides, "MataChat"),
        "wind.sqlite" => generic_im(run, f, sides, "撩训/信信"),
        "atimdb.db" | "atkitimdb.db" => generic_im(run, f, sides, "泡泡"),
        "session.db" => generic_im(run, f, sides, "拼多多商家"),
        "iktcjchatdata.db" => generic_im(run, f, sides, "信信"),
        _ => Ok(0),
    }
}



/// glued 表命中：按语义类分发。
fn route_class(run: &Run, f: &FileRef, sides: &[FileRef], app: &str, cls: &str) -> lovelycarve_plugin::Result<usize> {
    match cls {
        "geo" => generic_geo(run, f, sides, app),
        "bill" => generic_bill(run, f, sides, app),
        "media" => generic_media(run, f, sides, app),
        "user" => generic_user(run, f, sides, app),
        "call" => generic_call(run, f, sides, app),
        _ => {
            let n = generic_im(run, f, sides, app)?;
            if n == 0 {
                generic_user(run, f, sides, app)?;
            }
            Ok(n)
        }
    }
}

/// 通话类通用解析：duration/日期列。
pub fn generic_call(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let c = cols(&conn, &table);
        let dur = pick(&c, &["duration", "zduration", "call_duration"]);
        let date = pick(&c, &["date", "zdate", "time", "timestamp", "create_time", "date_received"]);
        let num = pick(&c, &["address", "number", "phone", "zaddress", "caller", "remote_party"]);
        if date.is_none() && dur.is_none() {
            continue;
        }
        let mut sel = vec![];
        for x in [num, date, dur].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        if sel.is_empty() {
            continue;
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            let mut i = 0usize;
            let num_v: Option<String> = if num.is_some() { let v = r.get(i).unwrap_or(None); i += 1; v } else { None };
            let ts_v: Option<f64> = if date.is_some() { let v = r.get(i).unwrap_or(None); i += 1; v } else { None };
            let dur_v: Option<f64> = if dur.is_some() { let v = r.get(i).unwrap_or(None); i += 1; v } else { None };
            run.record_values(
                "ios.calllog",
                vec![
                    num_v.unwrap_or_default().into(),
                    app.into(),
                    Value::from(dur_v.unwrap_or(0.0)),
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                    "".into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

/// 路由表命中的库：按语义分类解析。
fn route_known(run: &Run, f: &FileRef, sides: &[FileRef], app: &str, lower: &str) -> lovelycarve_plugin::Result<usize> {
    if lower.contains("track") || lower.contains("favcoredata") || lower.contains("journey")
        || lower.contains("blackbird") || lower.contains("footprint") || lower.contains("ucarmonitor")
        || lower.contains("spotlightstorage") || lower.contains("objects.sqlite") || lower.contains("agentcar")
        || lower.contains("oftenarrived") || lower.contains("routehistory") || lower.contains("route_search")
        || lower.contains("location_cache") || lower.contains("line_favorite") || lower.contains("addressdatabase")
        || lower.contains("baidumap") || lower.contains("basehistory") || lower.contains("kmb")
        || lower.contains("citymapper") || lower.contains("qmcs")
    {
        return generic_geo(run, f, sides, app);
    }
    if lower.contains("ntally") || lower.contains("tokenbank") || lower.contains("shophistory")
        || lower.contains("ivylove") || lower.contains("12123") || lower.contains("imeituan")
    {
        return generic_bill(run, f, sides, app);
    }
    if lower.contains("download") || lower.contains("history.sqlite") || lower.contains("tdmvideo")
        || lower.contains("upload") || lower.contains("netdisk") || lower.contains("trans")
        || lower.contains("mbox") || lower.contains("disk.db") || lower.contains("offline")
        || lower.contains("mega") || lower.contains("cello") || lower.contains("music_storage")
        || lower.contains("_document") || lower.contains("datelib") || lower.contains("etm_map")
        || lower.contains("store.sqlite") || lower.contains("asyncupload")
    {
        return generic_media(run, f, sides, app);
    }
    if lower.contains("login") || lower.contains("account_info") || lower.contains("user.sqlite")
        || lower.contains("talk.sqlite") || lower.contains("xmppvcard") || lower.contains("profilecache")
        || lower.contains("12306") || lower.contains("mail") || lower.contains("scheduleinfo")
        || lower.contains("traininfo") || lower.contains("qqreader") || lower.contains("wemeet")
        || lower.contains("snyx") || lower.contains("suningebuy") || lower.contains("wallet-rn")
        || lower.contains("baddresses") || lower.contains("bcoins") || lower.contains("tronlink")
        || lower.contains("personalinfo") || lower.contains("relation") || lower.contains("mucmember")
        || lower.contains("userinfo") || lower.contains("followuser") || lower.contains("contact")
        || lower.contains("team_list") || lower.contains("login") || lower.contains("ecuserdata")
    {
        return generic_user(run, f, sides, app);
    }
    if lower.contains("search_history") {
        let n = generic_geo_search(run, f, sides, app)?;
        if n > 0 {
            return Ok(n);
        }
    }
    // 电商IM（conversation/profile_relation/cpmmessagesdk）
    if lower.contains("conversation") || lower.contains("profile_relation") || lower.contains("cpmmessagesdk") {
        let n = generic_im(run, f, sides, app)?;
        if n > 0 { return Ok(n); }
    }
    let n = generic_im(run, f, sides, app)?;
    if n == 0 {
        return generic_user(run, f, sides, app);
    }
    Ok(n)
}

pub fn generic_geo(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let c = cols(&conn, &table);
        let Some(lng) = pick(&c, &["longitude", "lng", "zlongitude", "lon", "dstx", "x"]) else { continue };
        let Some(lat) = pick(&c, &["latitude", "lat", "zlatitude", "dsty", "y"]) else { continue };
        let name_c = pick(&c, &["name", "zname", "title", "ztitle", "address", "zaddress", "zpoiname", "poiname"]);
        let time_c = pick(&c, &["time", "zctime", "create_time", "timestamp", "zlasttime", "zfilltime", "date"]);
        let mut sel = vec![format!("[{lng}]"), format!("[{lat}]")];
        for x in [name_c, time_c].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            let lng_v: Option<f64> = r.get(0).unwrap_or(None);
            let lat_v: Option<f64> = r.get(1).unwrap_or(None);
            let name_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let ts_v: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            let (Some(lg), Some(la)) = (lng_v, lat_v) else { continue };
            if lg.abs() > 180.0 || la.abs() > 90.0 || (lg == 0.0 && la == 0.0) {
                continue;
            }
            run.record_values(
                "ios.app_geo",
                vec![
                    app.into(),
                    name_v.unwrap_or_default().into(),
                    Value::from(lg),
                    Value::from(la),
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

pub fn generic_bill(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let c = cols(&conn, &table);
        let Some(amount) = pick(&c, &["amount", "zamount", "price", "money", "cost"]) else { continue };
        let oid = pick(&c, &["order_id", "orderid", "zorderid", "id"]).unwrap_or("ROWID");
        let title = pick(&c, &["title", "remark", "zremark", "name", "desc", "note"]);
        let time_c = pick(&c, &["date", "zdate", "time", "create_time", "timestamp"]);
        let mut sel = vec![format!("[{amount}]"), format!("{oid}")];
        for x in [title, time_c].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            let amount_v: Option<f64> = r.get(0).unwrap_or(None);
            let Some(a) = amount_v else { continue };
            use rusqlite::types::ValueRef as RV;
            let oid_s = match r.get_ref(1) {
                Ok(RV::Text(t)) => String::from_utf8_lossy(t).to_string(),
                Ok(RV::Integer(i)) => i.to_string(),
                Ok(RV::Real(x)) => format!("{x}"),
                _ => String::new(),
            };
            let title_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let ts_v: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            run.record_values(
                "ios.app_bill",
                vec![
                    app.into(),
                    oid_s.into(),
                    title_v.unwrap_or_default().into(),
                    Value::from(a),
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

pub fn generic_media(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let c = cols(&conn, &table);
        let name_c = pick(&c, &["name", "title", "file_name", "filename", "zname", "video_name"]);
        let path_c = pick(&c, &["path", "file_path", "local_path", "zlocalpath", "url", "file_url", "filepath"]);
        if name_c.is_none() && path_c.is_none() {
            continue;
        }
        let size_c = pick(&c, &["size", "file_size", "total_bytes", "filesize", "zfilesize"]);
        let time_c = pick(&c, &["time", "create_time", "created_at", "zctime", "date", "timestamp"]);
        let mut sel = vec![name_c.map(|x| format!("[{x}]")).unwrap_or_else(|| "NULL".into())];
        for x in [path_c, size_c, time_c].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            let name_v: Option<String> = r.get(0).unwrap_or(None);
            let Some(nm) = name_v.filter(|x| !x.is_empty()) else { continue };
            let path_v: Option<String> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let size_v: Option<i64> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let ts_v: Option<f64> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            run.record_values(
                "ios.app_media",
                vec![
                    app.into(),
                    nm.into(),
                    path_v.unwrap_or_default().into(),
                    size_v.unwrap_or(0).into(),
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

pub fn generic_user(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let tl = table.to_lowercase();
        if !(tl.contains("user") || tl.contains("account") || tl.contains("member")
            || tl.contains("friend") || tl.contains("contact") || tl.contains("zuser")
            || tl.contains("mail") || tl.contains("schedule") || tl.contains("train"))
        {
            continue;
        }
        let c = cols(&conn, &table);
        let nick = pick(&c, &["nickname", "nick_name", "znickname", "name", "zname", "username", "display_name", "zpartnername", "zpushname", "zfullname"]);
        let uid = pick(&c, &["user_id", "userid", "uid", "zuid", "id", "account", "zpk"]).unwrap_or("ROWID");
        let phone = pick(&c, &["phone", "mobile", "phone_number", "zphone", "cellphone", "tel"]);
        let email = pick(&c, &["email", "mail", "zemail"]);
        let gender = pick(&c, &["gender", "sex", "zgender"]);
        let mut sel = vec![format!("{uid}")];
        for x in [nick, phone, email, gender].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            use rusqlite::types::ValueRef as RV2;
            let uid_s = match r.get_ref(0) {
                Ok(RV2::Text(t)) => String::from_utf8_lossy(t).to_string(),
                Ok(RV2::Integer(i)) => i.to_string(),
                Ok(RV2::Real(x)) => format!("{x}"),
                _ => continue,
            };
            let nick_v: Option<String> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let phone_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            let email_v: Option<String> = if cnt > 3 { r.get(3).unwrap_or(None) } else { None };
            let gender_v: Option<String> = if cnt > 4 { r.get(4).unwrap_or(None) } else { None };
            run.record_values(
                "ios.app_user",
                vec![
                    app.into(),
                    uid_s.into(),
                    nick_v.unwrap_or_default().into(),
                    phone_v.unwrap_or_default().into(),
                    email_v.unwrap_or_default().into(),
                    gender_v.unwrap_or_default().into(),
                    table.clone().into(),
                ],
            )
            .from(f)
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

/// 地图搜索历史：search/route 表，_data 列 JSON。
pub fn generic_geo_search(run: &Run, f: &FileRef, sides: &[FileRef], app: &str) -> lovelycarve_plugin::Result<usize> {
    let (_t, conn) = app_db(f, sides).map_err(|e| lovelycarve_plugin::Error::new("DB_OPEN", e))?;
    let mut n = 0usize;
    for table in list_tables(&conn) {
        let tl = table.to_lowercase();
        if !(tl.contains("search") || tl.contains("route") || tl.contains("history")) {
            continue;
        }
        let c = cols(&conn, &table);
        let data_c = pick(&c, &["_data", "data", "content", "keyword"]);
        let Some(dc) = data_c else { continue };
        let time_c = pick(&c, &["_lasted_used", "time", "date", "timestamp", "create_time"]);
        let type_c = pick(&c, &["_history_type", "type"]);
        let mut sel = vec![format!("[{dc}]")];
        for x in [time_c, type_c].into_iter().flatten() {
            sel.push(format!("[{x}]"));
        }
        let mut stmt = match conn.prepare(&format!("SELECT {} FROM [{table}] LIMIT 20001", sel.join(", "))) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let cnt = stmt.column_count();
        let mut rows = match stmt.query([]) { Ok(r) => r, Err(_) => continue };
        while let Ok(Some(r)) = rows.next() {
            let data_v: Option<String> = r.get(0).unwrap_or(None);
            let Some(raw) = data_v else { continue };
            let ts_v: Option<f64> = if cnt > 1 { r.get(1).unwrap_or(None) } else { None };
            let ty_v: Option<String> = if cnt > 2 { r.get(2).unwrap_or(None) } else { None };
            // JSON {keyword:...} / {end_addr:{name:...}} / 纯文本
            let (body, _s) = if raw.starts_with('{') {
                parse_json_field(&raw, &["keyword", "name", "poiname", "end_name", "address", "title"], &[])
            } else {
                (raw.clone(), String::new())
            };
            if body.is_empty() {
                continue;
            }
            run.record_values(
                "ios.app_geo",
                vec![
                    app.into(),
                    body.chars().take(200).collect::<String>().into(),
                    Value::Nil,
                    Value::Nil,
                    ts_or_nil(unix(ts_v.unwrap_or_default()).or_else(|| apple(ts_v.unwrap_or_default()))),
                ],
            )
            .from(f)
            .note(if let Some(t) = &ty_v { format!("type={t}") } else { String::new() })
            .emit()?;
            n += 1;
            if n >= 20000 { return Ok(n); }
        }
    }
    Ok(n)
}

fn list_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare("SELECT name FROM sqlite_master WHERE type='table'") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut v = Vec::new();
    while let Ok(Some(r)) = rows.next() {
        if let Ok(Some(t)) = r.get::<_, Option<String>>(0) {
            v.push(t);
        }
    }
    v
}

fn guess_app(f: &FileRef) -> String {
    let vp = f.view_path().unwrap_or("");
    for (k, app) in [
        ("sugar", "Sugar"), ("nico", "niconico"), ("tashuo", "塔说"),
        ("zhaopin", "智联招聘"), ("pdd", "拼多多商家"),
    ] {
        if vp.to_lowercase().contains(k) {
            return app.to_string();
        }
    }
    "未知应用".into()
}

fn view_contains(f: &FileRef, kw: &str) -> bool {
    f.view_path().unwrap_or("").to_lowercase().contains(kw)
}

/// 是否可路由的 App 数据库文件名。
pub fn is_app_db(lower_name: &str) -> bool {
    if crate::apps_route::app_display_name(lower_name).is_some() {
        return true;
    }
    if crate::apps_route_glued::glued_route(lower_name).is_some() {
        return true;
    }
    matches!(lower_name,
        "privatemessage.db" | "_message_zhihu_im.db" | "local_search.db" | "chat.db"
        | "imcache.db" | "message.sqlite" | "message.db" | "messages.db"
        | "messages-store.sqlite" | "fmdb.sqlite" | "zim_global.db" | "wind.sqlite"
        | "atimdb.db" | "atkitimdb.db" | "session.db" | "iktcjchatdata.db"
        | "im_message.db" | "im_groupmessage.db" | "infos.db" | "xmppmessagearchiving.sqlite"
    ) || lower_name.starts_with("bbimdb")
}
