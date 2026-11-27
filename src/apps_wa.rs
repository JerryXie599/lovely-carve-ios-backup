//! WhatsApp（ChatStorage.sqlite + ContactsV2.sqlite + LID.sqlite + MessagingInfraDatabase.sqlite
//! + BackedUpStorageDatabase.sqlite + ExtChatDatabase.sqlite）深度取证解析。
//!
//! 覆盖消息流（支持会话分组/回复上下文/媒体路径定位）、已读未读/已播放回执（兼容早期
//! bplist00、Protobuf ZRECEIPTINFO 以及现代 iOS MessagingInfraDB_v2/receipt_device 按设备
//! 秒级时间戳；结合 ZFILTEREDRECIPIENTCOUNT 还原发信时刻历史群规模与未读成员）、
//! 投票主题与选项得票详情（ZMESSAGETYPE=46/66 + SHA-256 投票映射）与消息回应。

use std::collections::{BTreeMap, HashMap, HashSet};

use lovelycarve_plugin::{FileRef, Run, Timestamp, Value};
use rusqlite::Connection;

use crate::wa_proto::{
    canonical_jid, clean_display_text, decode_poll_definition, decode_poll_vote_payload,
    decode_receipt_info_with_base, extract_jid_from_b64_proto, hex_lower, is_usable_display_name,
    norm_wa_ts, sha256_bytes, ParsedReceiptBlob, ParsedReceiptPerson,
};

use super::{apple, cols, list_tables, pick, ts_or_nil};

/// 单表输出上限。
const LIMIT: usize = 20_000;

/// 是否存在同名表（大小写不敏感），返回库中实际表名。
fn find_table(conn: &Connection, candidates: &[&str]) -> Option<String> {
    let tables = list_tables(conn);
    for want in candidates {
        if let Some(t) = tables.iter().find(|t| t.eq_ignore_ascii_case(want)) {
            return Some(t.clone());
        }
    }
    None
}

/// 构造 `别名.[列]`；列缺失时用 NULL 占位，保证查询列数固定。
fn q(alias: &str, col: Option<&str>) -> String {
    match col {
        Some(c) => format!("{alias}.[{c}]"),
        None => "NULL".to_string(),
    }
}

/// 无别名列引用；列缺失时用 NULL 占位。
fn q1(col: Option<&str>) -> String {
    match col {
        Some(c) => format!("[{c}]"),
        None => "NULL".to_string(),
    }
}

/// 截断到 n 个字符（按 Unicode 标量，避免切断多字节字符）。
fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 折叠换行与连续空白，便于单行展示。
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 读取整数列：兼容 INTEGER / REAL / 文本数字存储。
fn get_i64(r: &rusqlite::Row<'_>, i: usize) -> Option<i64> {
    if let Ok(v) = r.get::<_, Option<i64>>(i) {
        if v.is_some() {
            return v;
        }
    }
    if let Ok(v) = r.get::<_, Option<f64>>(i) {
        if let Some(x) = v {
            return Some(x as i64);
        }
    }
    r.get::<_, Option<String>>(i)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

/// 读取浮点列：兼容 REAL / INTEGER / 文本数字存储。
fn get_f64(r: &rusqlite::Row<'_>, i: usize) -> Option<f64> {
    if let Ok(v) = r.get::<_, Option<f64>>(i) {
        if v.is_some() {
            return v;
        }
    }
    if let Ok(v) = r.get::<_, Option<i64>>(i) {
        if let Some(x) = v {
            return Some(x as f64);
        }
    }
    r.get::<_, Option<String>>(i)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<f64>().ok())
}

/// WhatsApp 时间：正常备份为 Apple epoch（2001 基准）；部分导出/测试库用 Unix 秒。
/// 2024-2033 的 Unix 秒落在 1.7e9~2.0e9，而 Apple 秒仅 7e8~1e9，据此自动判别。
fn wa_ts(v: Option<f64>) -> Option<Timestamp> {
    let v = v?;
    if !v.is_finite() || v == 0.0 {
        return None;
    }
    let secs = if v.abs() > 1e14 {
        (v / 1e9) as i64
    } else {
        v as i64
    };
    if secs > 1_200_000_000 {
        crate::util::ts_plus8(secs)
    } else {
        apple(v)
    }
}

fn get_text(r: &rusqlite::Row<'_>, i: usize) -> Option<String> {
    r.get::<_, Option<String>>(i).ok().flatten()
}

fn get_blob(r: &rusqlite::Row<'_>, i: usize) -> Option<Vec<u8>> {
    r.get::<_, Option<Vec<u8>>>(i).ok().flatten()
}

/// 消息类型值 → 可读名称；未知值保留原数字。
fn msg_type_name(v: Option<i64>) -> String {
    match v {
        Some(0) => "文本".into(),
        Some(1) => "图片".into(),
        Some(2) => "音频".into(),
        Some(3) => "视频".into(),
        Some(4) => "联系人".into(),
        Some(5) => "位置".into(),
        Some(6) => "群事件".into(),
        Some(7) => "链接".into(),
        Some(8) => "文档".into(),
        Some(14) => "被撤回/删除消息".into(),
        Some(15) => "贴纸".into(),
        Some(46) => "投票".into(),
        Some(56) | Some(57) => "消息回应".into(),
        Some(66) => "投票更新".into(),
        Some(n) => n.to_string(),
        None => String::new(),
    }
}

/// 会话类型：iOS WhatsApp 的 ZSESSIONTYPE 取值经样本归纳为
/// 0=单聊、1=群、2=状态广播、3=个状态、4=社群、5=频道；缺失时按 JID 后缀兜底。
fn chat_type_name(v: Option<i64>, jid: &str) -> String {
    let by_jid = || {
        let j = jid.to_lowercase();
        if j.ends_with("@g.us") {
            "群"
        } else if j.ends_with("@newsletter") {
            "频道"
        } else if j.ends_with("@broadcast") {
            "状态广播"
        } else if j.ends_with("@status") {
            "状态"
        } else {
            "单聊"
        }
    };
    match v {
        Some(0) => "单聊".into(),
        Some(1) => "群".into(),
        Some(2) => "状态广播".into(),
        Some(3) => "状态".into(),
        Some(4) => "社群".into(),
        Some(5) => "频道".into(),
        Some(n) => format!("{n}（{}）", by_jid()),
        None => by_jid().into(),
    }
}

fn warn(run: &Run, f: &FileRef, msg: impl Into<String>) {
    let _ = run.warn_file(f, "WA_SCHEMA", msg.into());
}

fn jmap(pairs: Vec<(&str, Value)>) -> Value {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        if !v.is_nil() && seen.insert(k) {
            out.push((Value::from(k), v));
        }
    }
    Value::Map(out)
}

#[derive(Clone, Debug, Default)]
struct ContactEntry {
    name: String,
    phone: String,
}

#[derive(Clone, Debug)]
struct GroupMember {
    jid: String,
    canon_jid: String,
    contact_name: String,
}

#[derive(Default)]
struct ContactDirectory {
    by_canon: HashMap<String, ContactEntry>,
    by_digits: HashMap<String, ContactEntry>,
    lid_to_pn: HashMap<String, String>,
    group_members: HashMap<i64, Vec<GroupMember>>,
    self_canons: HashSet<String>,
}

fn digits_only(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

fn bare_user_part(jid: &str) -> &str {
    jid.split('@')
        .next()
        .unwrap_or(jid)
        .split(':')
        .next()
        .unwrap_or(jid)
}

impl ContactDirectory {
    fn mark_self_jid(&mut self, jid: &str) {
        let canon = canonical_jid(jid);
        if !canon.is_empty() {
            if let Some(pn) = self.lid_to_pn.get(&canon).cloned() {
                self.self_canons.insert(pn);
            }
            self.self_canons.insert(canon);
        }
    }

    fn is_self_jid(&self, jid: &str) -> bool {
        let canon = canonical_jid(jid);
        if canon.is_empty() {
            return false;
        }
        if self.self_canons.contains(&canon) {
            return true;
        }
        if let Some(pn) = self.lid_to_pn.get(&canon) {
            if self.self_canons.contains(pn) {
                return true;
            }
        }
        false
    }

    fn insert_contact(&mut self, jid: &str, name: &str, phone: &str) {
        let cleaned_name = clean_display_text(name);
        let name = if is_usable_display_name(&cleaned_name) {
            cleaned_name.as_str()
        } else {
            ""
        };
        let cleaned_phone = clean_display_text(phone);
        let phone = cleaned_phone.as_str();
        if jid.trim().is_empty() && phone.is_empty() {
            return;
        }
        let canon = canonical_jid(jid);
        if !canon.is_empty() {
            let e = self.by_canon.entry(canon).or_default();
            if e.name.is_empty() && !name.is_empty() {
                e.name = name.to_owned();
            }
            if e.phone.is_empty() && !phone.is_empty() {
                e.phone = phone.to_owned();
            }
        }
        let d = if !phone.is_empty() {
            digits_only(phone)
        } else {
            digits_only(bare_user_part(jid))
        };
        if d.len() >= 5 {
            let e = self.by_digits.entry(d).or_default();
            if e.name.is_empty() && !name.is_empty() {
                e.name = name.to_owned();
            }
            if e.phone.is_empty() && !phone.is_empty() {
                e.phone = phone.to_owned();
            }
        }
    }

    fn load(run: &Run, f: &FileRef, sides: &[FileRef], chat_conn: &Connection) -> Self {
        let mut dir = Self::default();
        if let Some((_t, lconn)) = open_wa_side_db(run, f, sides, "lid.sqlite", &["LID.sqlite"]) {
            dir.load_lid_db(&lconn);
        }
        if let Some((_t, cconn)) =
            open_wa_side_db(run, f, sides, "contactsv2.sqlite", &["ContactsV2.sqlite"])
        {
            dir.load_contacts_db(&cconn);
        }
        if let Some(t_msg) = find_table(chat_conn, &["ZWAMESSAGE"]) {
            let mc = cols(chat_conn, &t_msg);
            if let (Some(c_me), Some(c_push)) =
                (pick(&mc, &["ZISFROMME"]), pick(&mc, &["ZPUSHNAME"]))
            {
                let sql = format!(
                    "SELECT [{c_push}] FROM [{t_msg}] WHERE [{c_me}] = 1 AND [{c_push}] IS NOT NULL LIMIT 60"
                );
                if let Ok(mut stmt) = chat_conn.prepare(&sql) {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            if let Some(pn) = get_text(&r, 0) {
                                if let Some(self_jid) = extract_jid_from_b64_proto(&pn) {
                                    dir.mark_self_jid(&self_jid);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(t_sess) = find_table(chat_conn, &["ZWACHATSESSION"]) {
            let sc = cols(chat_conn, &t_sess);
            if let (Some(c_jid), Some(c_name)) =
                (pick(&sc, &["ZCONTACTJID"]), pick(&sc, &["ZPARTNERNAME"]))
            {
                let sql = format!("SELECT [{c_jid}], [{c_name}] FROM [{t_sess}]");
                if let Ok(mut stmt) = chat_conn.prepare(&sql) {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            let j = get_text(&r, 0).unwrap_or_default();
                            let n = clean_display_text(&get_text(&r, 1).unwrap_or_default());
                            if j.trim().is_empty() || n.is_empty() {
                                continue;
                            }
                            if n.eq_ignore_ascii_case("you") {
                                dir.mark_self_jid(&j);
                            } else if !j.ends_with("@g.us")
                                && !j.ends_with("@status")
                                && !j.ends_with("@broadcast")
                                && is_usable_display_name(&n)
                            {
                                dir.insert_contact(&j, &n, "");
                            }
                        }
                    }
                }
            }
        }
        if let Some(t_push) = find_table(chat_conn, &["ZWAPROFILEPUSHNAME"]) {
            let pc = cols(chat_conn, &t_push);
            if let (Some(c_jid), Some(c_name)) = (
                pick(&pc, &["ZJID", "JID"]),
                pick(&pc, &["ZPUSHNAME", "ZNAME"]),
            ) {
                let sql = format!("SELECT [{c_jid}], [{c_name}] FROM [{t_push}]");
                if let Ok(mut stmt) = chat_conn.prepare(&sql) {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            if let (Some(j), Some(n)) = (get_text(&r, 0), get_text(&r, 1)) {
                                if is_usable_display_name(&n) {
                                    dir.insert_contact(&j, &n, "");
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some((_t, bconn)) = open_wa_side_db(
            run,
            f,
            sides,
            "backedupstoragedatabase.sqlite",
            &[
                "BackedUpStorageDB/BackedUpStorageDatabase.sqlite",
                "BackedUpStorageDatabase.sqlite",
            ],
        ) {
            dir.load_backed_up_storage_db(&bconn);
        }
        if let Some((_t, econn)) = open_wa_side_db(
            run,
            f,
            sides,
            "extchatdatabase.sqlite",
            &["ExtChatDB/ExtChatDatabase.sqlite", "ExtChatDatabase.sqlite"],
        ) {
            dir.load_ext_chat_db(&econn);
        }
        if let Some(t_gm) = find_table(chat_conn, &["ZWAGROUPMEMBER"]) {
            let gc = cols(chat_conn, &t_gm);
            let c_sess = pick(&gc, &["ZCHATSESSION"]);
            let c_jid = pick(&gc, &["ZMEMBERJID", "ZJID"]);
            let c_cname = pick(&gc, &["ZCONTACTNAME"]);
            let c_fname = pick(&gc, &["ZFIRSTNAME", "ZPUSHNAME"]);
            let c_active = pick(&gc, &["ZISACTIVE"]);
            if let (Some(cs), Some(cj)) = (c_sess, c_jid) {
                let sql = format!(
                    "SELECT m.[{cs}], m.[{cj}], {}, {}, {} FROM [{t_gm}] m",
                    q("m", c_cname),
                    q("m", c_fname),
                    q("m", c_active)
                );
                if let Ok(mut stmt) = chat_conn.prepare(&sql) {
                    if let Ok(mut rows) = stmt.query([]) {
                        while let Ok(Some(r)) = rows.next() {
                            let Some(sess_id) = get_i64(&r, 0) else {
                                continue;
                            };
                            let Some(mjid) = get_text(&r, 1) else {
                                continue;
                            };
                            let raw_cname = get_text(&r, 2).unwrap_or_default();
                            let raw_fname = get_text(&r, 3).unwrap_or_default();
                            let active = get_i64(&r, 4).unwrap_or(1);
                            let cname = if is_usable_display_name(&raw_cname) {
                                clean_display_text(&raw_cname)
                            } else if is_usable_display_name(&raw_fname) {
                                clean_display_text(&raw_fname)
                            } else {
                                String::new()
                            };
                            if !cname.is_empty() {
                                dir.insert_contact(&mjid, &cname, "");
                            }
                            if active != 0 && !dir.is_self_jid(&mjid) {
                                let c = canonical_jid(&mjid);
                                let list = dir.group_members.entry(sess_id).or_default();
                                if !list.iter().any(|gm| gm.canon_jid == c) {
                                    list.push(GroupMember {
                                        jid: mjid,
                                        canon_jid: c,
                                        contact_name: cname,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        dir
    }

    fn load_lid_db(&mut self, conn: &Connection) {
        for tbl in list_tables(conn) {
            if tbl.eq_ignore_ascii_case("sqlite_sequence") || tbl.starts_with("Z_") {
                continue;
            }
            let lc = cols(conn, &tbl);
            let c_lid = lc
                .iter()
                .find(|c| {
                    c.eq_ignore_ascii_case("ZLID")
                        || c.eq_ignore_ascii_case("LID")
                        || c.to_ascii_uppercase().ends_with("LIDJID")
                })
                .or_else(|| lc.iter().find(|c| c.to_ascii_uppercase().contains("LID")));
            let c_pn = lc.iter().find(|c| {
                let u = c.to_ascii_uppercase();
                u.contains("PHONE")
                    || u.contains("PN")
                    || u == "ZJID"
                    || u == "JID"
                    || u.contains("USERJID")
            });
            let c_name = lc.iter().find(|c| {
                let u = c.to_ascii_uppercase();
                u.contains("DISPLAY")
                    || u.contains("PUSHNAME")
                    || u.contains("FULLNAME")
                    || u == "ZNAME"
                    || u == "NAME"
            });
            let (Some(cl), Some(cp)) = (c_lid, c_pn) else {
                continue;
            };
            let cname_sql = c_name.map_or_else(|| "NULL".into(), |c| format!("[{c}]"));
            let sql = format!("SELECT [{cl}], [{cp}], {cname_sql} FROM [{tbl}]");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        let lid = get_text(&r, 0).unwrap_or_default();
                        let pn = get_text(&r, 1).unwrap_or_default();
                        let name = get_text(&r, 2).unwrap_or_default();
                        let clid = canonical_jid(&lid);
                        if !clid.is_empty() && !pn.trim().is_empty() {
                            let pn_jid = if pn.contains('@') {
                                canonical_jid(&pn)
                            } else {
                                let d = digits_only(&pn);
                                if !d.is_empty() {
                                    format!("{d}@s.whatsapp.net")
                                } else {
                                    pn.trim().to_owned()
                                }
                            };
                            self.lid_to_pn.insert(clid, pn_jid.clone());
                            self.insert_contact(&lid, &name, &digits_only(&pn));
                            self.insert_contact(&pn_jid, &name, &digits_only(&pn));
                        }
                    }
                }
            }
        }
    }

    fn load_contacts_db(&mut self, conn: &Connection) {
        let Some(tbl) = find_table(conn, &["ZWAADDRESSBOOKCONTACT", "ZWACONTACT", "CONTACTS"])
        else {
            return;
        };
        let cc = cols(conn, &tbl);
        let c_jid = pick(&cc, &["ZWHATSAPPID", "ZJID", "JID"]);
        let c_lid = pick(&cc, &["ZLID", "ZLIDJID"]);
        let c_full = pick(&cc, &["ZFULLNAME", "ZDISPLAYNAME", "ZSAVEDNAME"]);
        let c_given = pick(&cc, &["ZGIVENNAME", "ZFIRSTNAME"]);
        let c_phone = pick(&cc, &["ZPHONENUMBER", "ZPHONE"]);
        let sql = format!(
            "SELECT {}, {}, {}, {}, {} FROM [{tbl}]",
            q1(c_jid),
            q1(c_lid),
            q1(c_full),
            q1(c_given),
            q1(c_phone),
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    let jid = get_text(&r, 0).unwrap_or_default();
                    let lid = get_text(&r, 1).unwrap_or_default();
                    let full = get_text(&r, 2).unwrap_or_default();
                    let given = get_text(&r, 3).unwrap_or_default();
                    let phone = get_text(&r, 4).unwrap_or_default();
                    let name = if !full.trim().is_empty() {
                        &full
                    } else {
                        &given
                    };
                    if !jid.is_empty() {
                        self.insert_contact(&jid, name, &phone);
                    }
                    if !lid.is_empty() {
                        self.insert_contact(&lid, name, &phone);
                        if !jid.is_empty() {
                            self.lid_to_pn
                                .insert(canonical_jid(&lid), canonical_jid(&jid));
                        }
                    }
                }
            }
        }
    }

    /// 现代 iOS WhatsApp 的 Push Name 迁移至 `BackedUpStorageDB/BackedUpStorageDatabase.sqlite`。
    fn load_backed_up_storage_db(&mut self, conn: &Connection) {
        let Some(tbl) = find_table(conn, &["profile_push_name"]) else {
            return;
        };
        let pc = cols(conn, &tbl);
        let Some(c_jid) = pick(&pc, &["jid", "zjid"]) else {
            return;
        };
        let Some(c_name) = pick(&pc, &["push_name", "zpushname"]) else {
            return;
        };
        let sql = format!("SELECT [{c_jid}], [{c_name}] FROM [{tbl}]");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    if let (Some(j), Some(n)) = (get_text(&r, 0), get_text(&r, 1)) {
                        if is_usable_display_name(&n) {
                            self.insert_contact(&j, &n, "");
                        }
                    }
                }
            }
        }
    }

    /// 现代 iOS WhatsApp 的 `ExtChatDB/ExtChatDatabase.sqlite` 含 `contact_metadata` 映射；
    /// `message_parent_association` 为相册/容器关联而非引用回复，不参与 `reply_to` 解析。
    fn load_ext_chat_db(&mut self, conn: &Connection) {
        let Some(tbl) = find_table(conn, &["contact_metadata"]) else {
            return;
        };
        let cc = cols(conn, &tbl);
        let Some(c_lid) = pick(&cc, &["contact_lid"]) else {
            return;
        };
        let c_pn = pick(&cc, &["contact_pn"]);
        let c_push = pick(&cc, &["contact_push_name"]);
        let c_user = pick(&cc, &["contact_username"]);
        let sql = format!(
            "SELECT [{c_lid}], {}, {}, {} FROM [{tbl}]",
            q1(c_pn),
            q1(c_push),
            q1(c_user),
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    let lid = get_text(&r, 0).unwrap_or_default();
                    let pn = get_text(&r, 1).unwrap_or_default();
                    let push = get_text(&r, 2).unwrap_or_default();
                    let user = get_text(&r, 3).unwrap_or_default();
                    let name = if is_usable_display_name(&push) {
                        push
                    } else if is_usable_display_name(&user) {
                        user
                    } else {
                        String::new()
                    };
                    let clid = canonical_jid(&lid);
                    let digits = digits_only(&pn);
                    let pn_jid = if !pn.trim().is_empty() {
                        if pn.contains('@') {
                            canonical_jid(&pn)
                        } else if !digits.is_empty() {
                            format!("{digits}@s.whatsapp.net")
                        } else {
                            pn.trim().to_owned()
                        }
                    } else {
                        String::new()
                    };
                    if !clid.is_empty() && !pn_jid.is_empty() {
                        self.lid_to_pn.insert(clid, pn_jid.clone());
                    }
                    if !lid.trim().is_empty() {
                        self.insert_contact(&lid, &name, &digits);
                    }
                    if !pn_jid.is_empty() {
                        self.insert_contact(&pn_jid, &name, &digits);
                    }
                }
            }
        }
    }

    fn resolve(&self, raw_jid: &str, fallback_name: &str) -> (String, String, String) {
        let trimmed = raw_jid.trim();
        let canon = canonical_jid(trimmed);
        let mapped_pn = self.lid_to_pn.get(&canon).cloned().unwrap_or_default();
        let lookup_jid = if !mapped_pn.is_empty() {
            &mapped_pn
        } else {
            &canon
        };
        let d = digits_only(bare_user_part(lookup_jid));

        let candidates = [
            self.by_canon.get(&canon),
            (!mapped_pn.is_empty())
                .then(|| self.by_canon.get(&mapped_pn))
                .flatten(),
            (d.len() >= 5).then(|| self.by_digits.get(&d)).flatten(),
        ];

        let mut best_name = String::new();
        let mut best_phone = String::new();
        for c in candidates.into_iter().flatten() {
            if best_name.is_empty() && is_usable_display_name(&c.name) {
                best_name = clean_display_text(&c.name);
            }
            if best_phone.is_empty() && !c.phone.trim().is_empty() {
                best_phone = clean_display_text(&c.phone);
            }
        }

        let is_unmapped_lid = mapped_pn.is_empty() && canon.ends_with("@lid");
        let phone = if !best_phone.is_empty() {
            best_phone
        } else if !is_unmapped_lid {
            let b = bare_user_part(lookup_jid);
            if b.bytes().all(|c| c.is_ascii_digit()) {
                b.to_owned()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let cleaned_fallback = clean_display_text(fallback_name);
        let name = if !best_name.is_empty() {
            best_name
        } else if is_usable_display_name(&cleaned_fallback) {
            cleaned_fallback
        } else if !phone.is_empty() {
            phone.clone()
        } else {
            let b = bare_user_part(lookup_jid);
            if !b.is_empty() {
                b.to_owned()
            } else {
                trimmed.to_owned()
            }
        };

        (
            name,
            phone,
            if !mapped_pn.is_empty() {
                mapped_pn
            } else {
                trimmed.to_owned()
            },
        )
    }
}

/// 在伴随文件或检材视图中按相对子路径定位并打开 WhatsApp 同容器辅助 SQLite 库。
fn open_wa_side_db(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
    filename: &str,
    rel_subpaths: &[&str],
) -> Option<(tempfile::TempDir, Connection)> {
    if let Some(sf) = sides
        .iter()
        .find(|s| s.name().eq_ignore_ascii_case(filename))
    {
        if let Ok(db) = crate::backupdb::open_db(sf, sides) {
            return Some(db);
        }
    }
    let view = run.view()?;
    let mut candidates: Vec<String> = Vec::new();
    if let Some(base_vp) = f.view_path() {
        if let Some((prefix, _)) = base_vp.rsplit_once('/') {
            for &sub in rel_subpaths {
                candidates.push(format!("{prefix}/{sub}"));
            }
            if let Some((parent, _)) = prefix.rsplit_once('/') {
                for &sub in rel_subpaths {
                    candidates.push(format!("{parent}/{sub}"));
                }
            }
        }
    }
    for &sub in rel_subpaths {
        candidates.push(format!(
            "AppDomainGroup-group.net.whatsapp.WhatsApp.shared/{sub}"
        ));
        candidates.push((*sub).to_owned());
    }
    for cand in &candidates {
        if let Ok(Some(rf)) = view.open(cand) {
            let mut extra_sides = sides.to_vec();
            for suffix in ["-wal", "-shm"] {
                if let Ok(Some(sf)) = view.open(&format!("{cand}{suffix}")) {
                    extra_sides.push(sf);
                }
            }
            if let Ok(db) = crate::backupdb::open_db(&rf, &extra_sides) {
                return Some(db);
            }
        }
    }
    None
}

fn resolve_wa_media(run: &Run, f: &FileRef, rel_paths: &[&str]) -> (String, Option<FileRef>) {
    let mut first_non_empty = String::new();
    for &rel_path in rel_paths {
        let rel = rel_path.trim().trim_start_matches('/');
        if rel.is_empty() {
            continue;
        }
        if first_non_empty.is_empty() {
            first_non_empty = rel.to_owned();
        }
        if let Some(view) = run.view() {
            let mut candidates = vec![
                format!("AppDomainGroup-group.net.whatsapp.WhatsApp.shared/Message/{rel}"),
                format!("AppDomainGroup-group.net.whatsapp.WhatsApp.shared/{rel}"),
                format!("Message/{rel}"),
                format!("Library/{rel}"),
                rel.to_owned(),
            ];
            if let Some(base_vp) = f.view_path() {
                if let Some((prefix, _)) = base_vp.rsplit_once('/') {
                    candidates.insert(0, format!("{prefix}/Message/{rel}"));
                    candidates.insert(0, format!("{prefix}/{rel}"));
                    if let Some((parent, _)) = prefix.rsplit_once('/') {
                        candidates.insert(2, format!("{parent}/Message/{rel}"));
                        candidates.insert(2, format!("{parent}/{rel}"));
                    }
                }
            }
            for cand in &candidates {
                if let Ok(Some(rf)) = view.open(cand) {
                    let vp = rf.view_path().unwrap_or(cand).to_owned();
                    return (vp, Some(rf));
                }
            }
        }
    }
    (first_non_empty, None)
}

struct RawWaMsg {
    z_pk: i64,
    stanza_id: String,
    text: String,
    date: Option<f64>,
    from_jid: String,
    from_me: Option<i64>,
    msg_type: Option<i64>,
    msg_status: Option<i64>,
    sess_id: Option<i64>,
    sess_name: String,
    sess_jid: String,
    sess_type: Option<i64>,
    media_local_path: String,
    thumb_local_path: String,
    xmpp_thumb_path: String,
    media_title: String,
    vcard_name: String,
    vcard_string: String,
    media_meta: Option<Vec<u8>>,
    receipt_blob: Option<Vec<u8>>,
    gm_jid: String,
    gm_name: String,
    parent_pk: Option<i64>,
    parent_stanza: String,
    push_name: String,
    filtered_recipient_count: Option<i64>,
}

#[derive(Default)]
struct DeviceReceiptAgg {
    deliv_dev0: Option<i64>,
    deliv_min: Option<i64>,
    read_dev0: Option<i64>,
    read_min: Option<i64>,
    play_dev0: Option<i64>,
    play_min: Option<i64>,
}

fn update_agg_ts(
    dev0_slot: &mut Option<i64>,
    min_slot: &mut Option<i64>,
    dev_id: i64,
    val: Option<i64>,
) {
    let Some(ts) = val.filter(|&v| v > 0) else {
        return;
    };
    if dev_id == 0 {
        *dev0_slot = Some(dev0_slot.map_or(ts, |cur| cur.min(ts)));
    }
    *min_slot = Some(min_slot.map_or(ts, |cur| cur.min(ts)));
}

/// 解析现代 iOS WhatsApp `MessagingInfraDB_v2/MessagingInfraDatabase.sqlite -> receipt_device`
/// 按 `(stanza_id, canonical_jid(user_jid))` 聚合多设备精确送达/已读/已播放秒级时间戳。
fn load_messaging_infra_receipts(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
    dir: &ContactDirectory,
) -> HashMap<String, ParsedReceiptBlob> {
    let mut out: HashMap<String, ParsedReceiptBlob> = HashMap::new();
    let Some((_t, conn)) = open_wa_side_db(
        run,
        f,
        sides,
        "messaginginfradatabase.sqlite",
        &[
            "MessagingInfraDB_v2/MessagingInfraDatabase.sqlite",
            "MessagingInfraDB/MessagingInfraDatabase.sqlite",
            "MessagingInfraDatabase.sqlite",
        ],
    ) else {
        return out;
    };
    let Some(t_rcpt) = find_table(&conn, &["receipt_device"]) else {
        return out;
    };
    let rc = cols(&conn, &t_rcpt);
    let Some(c_stanza) = pick(&rc, &["stanza_id"]) else {
        return out;
    };
    let Some(c_user) = pick(&rc, &["user_jid"]) else {
        return out;
    };
    let c_chat = pick(&rc, &["chat_jid"]);
    let c_dev = pick(&rc, &["device_id"]);
    let c_deliv = pick(&rc, &["delivered_timestamp"]);
    let c_read = pick(&rc, &["read_timestamp"]);
    let c_play = pick(&rc, &["played_timestamp"]);

    let sql = format!(
        "SELECT [{c_stanza}], {}, [{c_user}], {}, {}, {}, {} FROM [{t_rcpt}] ORDER BY rowid ASC",
        q1(c_chat),
        q1(c_dev),
        q1(c_deliv),
        q1(c_read),
        q1(c_play),
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return out;
    };
    let Ok(mut rows) = stmt.query([]) else {
        return out;
    };

    let mut grouped: HashMap<String, BTreeMap<String, DeviceReceiptAgg>> = HashMap::new();
    while let Ok(Some(r)) = rows.next() {
        let stanza_id = get_text(&r, 0).unwrap_or_default();
        let chat_jid = get_text(&r, 1).unwrap_or_default();
        let user_jid = get_text(&r, 2).unwrap_or_default();
        let user_canon = canonical_jid(&user_jid);
        if stanza_id.trim().is_empty() || user_canon.is_empty() {
            continue;
        }
        let is_self_chat = !chat_jid.is_empty() && dir.is_self_jid(&chat_jid);
        if !is_self_chat && dir.is_self_jid(&user_jid) {
            continue;
        }
        let dev_id = get_i64(&r, 3).unwrap_or(0);
        let agg = grouped
            .entry(stanza_id)
            .or_default()
            .entry(user_canon)
            .or_default();
        update_agg_ts(
            &mut agg.deliv_dev0,
            &mut agg.deliv_min,
            dev_id,
            get_i64(&r, 4),
        );
        update_agg_ts(
            &mut agg.read_dev0,
            &mut agg.read_min,
            dev_id,
            get_i64(&r, 5),
        );
        update_agg_ts(
            &mut agg.play_dev0,
            &mut agg.play_min,
            dev_id,
            get_i64(&r, 6),
        );
    }

    for (stanza_id, users) in grouped {
        let mut blob = ParsedReceiptBlob {
            mode: "receipt_device",
            ..Default::default()
        };
        for (user_jid, agg) in users {
            let deliv_s = agg
                .deliv_dev0
                .or(agg.deliv_min)
                .and_then(|v| norm_wa_ts(v as f64));
            let read_s = agg
                .read_dev0
                .or(agg.read_min)
                .and_then(|v| norm_wa_ts(v as f64));
            let play_s = agg
                .play_dev0
                .or(agg.play_min)
                .and_then(|v| norm_wa_ts(v as f64));
            if let Some(p) = play_s {
                blob.played.push(ParsedReceiptPerson {
                    jid: user_jid,
                    time: Some(p.clone()),
                    delivered_at: deliv_s,
                    read_at: read_s,
                    played_at: Some(p),
                    status: None,
                });
            } else if let Some(rd) = read_s {
                blob.read.push(ParsedReceiptPerson {
                    jid: user_jid,
                    time: Some(rd.clone()),
                    delivered_at: deliv_s,
                    read_at: Some(rd),
                    played_at: None,
                    status: None,
                });
            } else if let Some(dv) = deliv_s {
                blob.delivered.push(ParsedReceiptPerson {
                    jid: user_jid,
                    time: Some(dv.clone()),
                    delivered_at: Some(dv),
                    read_at: None,
                    played_at: None,
                    status: None,
                });
            } else {
                blob.pending.push(ParsedReceiptPerson {
                    jid: user_jid,
                    ..Default::default()
                });
            }
        }
        out.insert(stanza_id, blob);
    }
    out
}

fn receipt_person_value(p: &ParsedReceiptPerson, dir: &ContactDirectory) -> Value {
    let (name, phone, _) = dir.resolve(&p.jid, "");
    jmap(vec![
        ("jid", Value::from(p.jid.as_str())),
        (
            "name",
            (!name.is_empty())
                .then(|| Value::from(name))
                .unwrap_or(Value::Nil),
        ),
        (
            "phone",
            (!phone.is_empty())
                .then(|| Value::from(phone))
                .unwrap_or(Value::Nil),
        ),
        ("time", p.time.as_deref().map_or(Value::Nil, Value::from)),
        (
            "delivered_at",
            p.delivered_at.as_deref().map_or(Value::Nil, Value::from),
        ),
        (
            "read_at",
            p.read_at.as_deref().map_or(Value::Nil, Value::from),
        ),
        (
            "played_at",
            p.played_at.as_deref().map_or(Value::Nil, Value::from),
        ),
        (
            "status",
            p.status.as_deref().map_or(Value::Nil, Value::from),
        ),
    ])
}

fn load_external_polls(
    conn: &Connection,
) -> (
    HashMap<i64, Vec<String>>,
    HashMap<i64, Vec<crate::wa_proto::ParsedPollVote>>,
) {
    let mut extra_options: HashMap<i64, Vec<String>> = HashMap::new();
    let mut extra_votes: HashMap<i64, Vec<crate::wa_proto::ParsedPollVote>> = HashMap::new();

    if let Some(t_opt) = find_table(conn, &["ZWAPOLLOPTION", "ZWAPOLLOPTIONS"]) {
        let oc = cols(conn, &t_opt);
        if let (Some(c_msg), Some(c_name)) = (
            pick(&oc, &["ZMESSAGE", "ZPOLL", "ZPARENTMESSAGE"]),
            pick(&oc, &["ZOPTIONNAME", "ZNAME", "ZTEXT"]),
        ) {
            let order = pick(&oc, &["ZOPTIONINDEX", "Z_PK"]).unwrap_or(c_msg);
            let sql = format!("SELECT [{c_msg}], [{c_name}] FROM [{t_opt}] ORDER BY [{order}] ASC");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        if let (Some(mid), Some(name)) = (get_i64(&r, 0), get_text(&r, 1)) {
                            if !name.trim().is_empty() {
                                extra_options
                                    .entry(mid)
                                    .or_default()
                                    .push(name.trim().to_owned());
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(t_vote) = find_table(conn, &["ZWAPOLLVOTE", "ZWAPOLLVOTES"]) {
        let vc = cols(conn, &t_vote);
        let c_msg = pick(&vc, &["ZMESSAGE", "ZPARENTMESSAGE", "ZPOLL"]);
        let c_voter = pick(&vc, &["ZVOTERJID", "ZSENDERJID", "ZJID"]);
        let c_ts = pick(&vc, &["ZTIMESTAMP", "ZMESSAGEDATE", "ZDATE"]);
        let c_blob = pick(&vc, &["ZSELECTEDOPTIONS", "ZDATA", "ZPAYLOAD"]);
        let c_opt_name = pick(&vc, &["ZOPTIONNAME", "ZTEXT"]);
        if let Some(cm) = c_msg {
            let sql = format!(
                "SELECT [{cm}], {}, {}, {}, {} FROM [{t_vote}]",
                q1(c_voter),
                q1(c_ts),
                q1(c_blob),
                q1(c_opt_name),
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(r)) = rows.next() {
                        let Some(mid) = get_i64(&r, 0) else { continue };
                        let voter = get_text(&r, 1).unwrap_or_default();
                        let ts = get_f64(&r, 2).and_then(norm_wa_ts);
                        let blob = get_blob(&r, 3);
                        let opt_name = get_text(&r, 4).unwrap_or_default();
                        if let Some(pv) =
                            decode_poll_vote_payload(&voter, ts, blob.as_deref(), &opt_name)
                        {
                            extra_votes.entry(mid).or_default().push(pv);
                        }
                    }
                }
            }
        }
    }

    (extra_options, extra_votes)
}

/// 入口：依次解析消息（含对话/回执/投票/回应）、位置、会话、成员变更。
pub fn scan(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_tmp, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            warn(run, f, format!("WhatsApp 数据库打开失败: {e}"));
            return Err(lovelycarve_plugin::Error::new("DB_OPEN", e));
        }
    };
    let dir = ContactDirectory::load(run, f, sides, &conn);
    let mut n = 0usize;
    n += messages(run, f, sides, &conn, &dir)?;
    n += geos(run, f, &conn)?;
    n += chats(run, f, &conn)?;
    n += member_changes(run, f, &conn)?;
    Ok(n)
}

/// ZWAMESSAGE → ios.wa_msg & ios.wa_poll。
fn messages(
    run: &Run,
    f: &FileRef,
    sides: &[FileRef],
    conn: &Connection,
    dir: &ContactDirectory,
) -> lovelycarve_plugin::Result<usize> {
    let Some(t_msg) = find_table(conn, &["ZWAMESSAGE"]) else {
        return Ok(0);
    };
    let mc = cols(conn, &t_msg);

    let c_pk = pick(&mc, &["Z_PK"]);
    let c_stanza = pick(&mc, &["ZSTANZAID"]);
    let c_text = pick(&mc, &["ZTEXT", "ZMESSAGE"]);
    let c_date = pick(&mc, &["ZMESSAGEDATE", "ZSENTDATE"]);
    let c_from = pick(&mc, &["ZFROMJID"]);
    let c_push = pick(&mc, &["ZPUSHNAME"]);
    let c_me = pick(&mc, &["ZISFROMME"]);
    let c_type = pick(&mc, &["ZMESSAGETYPE"]);
    let c_status = pick(&mc, &["ZMESSAGESTATUS"]);
    let c_sess = pick(&mc, &["ZCHATSESSION"]);
    let c_media = pick(&mc, &["ZMEDIAITEM"]);
    let c_info_fk = pick(&mc, &["ZMESSAGEINFO"]);
    let c_gm_fk = pick(&mc, &["ZGROUPMEMBER"]);
    let c_parent = pick(&mc, &["ZPARENTMESSAGE"]);
    let c_rcpt_cnt = pick(&mc, &["ZFILTEREDRECIPIENTCOUNT"]);

    let t_sess = find_table(conn, &["ZWACHATSESSION"]);
    let sess_cols = t_sess.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let sess_join_on =
        t_sess.is_some() && c_sess.is_some() && pick(&sess_cols, &["Z_PK"]).is_some();
    let s_name = if sess_join_on {
        pick(&sess_cols, &["ZPARTNERNAME"])
    } else {
        None
    };
    let s_jid = if sess_join_on {
        pick(&sess_cols, &["ZCONTACTJID"])
    } else {
        None
    };
    let s_type = if sess_join_on {
        pick(&sess_cols, &["ZSESSIONTYPE"])
    } else {
        None
    };
    let s_pk = pick(&sess_cols, &["Z_PK"]).unwrap_or("Z_PK");
    let sess_join = match (&t_sess, c_sess) {
        (Some(t), Some(c)) if sess_join_on => format!(" LEFT JOIN [{t}] s ON s.[{s_pk}] = m.[{c}]"),
        _ => String::new(),
    };

    let t_media = find_table(conn, &["ZWAMEDIAITEM"]);
    let media_cols = t_media.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let media_join_on =
        t_media.is_some() && c_media.is_some() && pick(&media_cols, &["Z_PK"]).is_some();
    let m_path = if media_join_on {
        pick(&media_cols, &["ZMEDIALOCALPATH"])
    } else {
        None
    };
    let m_thumb = if media_join_on {
        pick(&media_cols, &["ZTHUMBNAILLOCALPATH"])
    } else {
        None
    };
    let m_xthumb = if media_join_on {
        pick(&media_cols, &["ZXMPPTHUMBPATH"])
    } else {
        None
    };
    let m_title = if media_join_on {
        pick(&media_cols, &["ZTITLE"])
    } else {
        None
    };
    let m_vname = if media_join_on {
        pick(&media_cols, &["ZVCARDNAME"])
    } else {
        None
    };
    let m_vstr = if media_join_on {
        pick(&media_cols, &["ZVCARDSTRING"])
    } else {
        None
    };
    let m_meta = if media_join_on {
        pick(&media_cols, &["ZMETADATA"])
    } else {
        None
    };
    let m_pk = pick(&media_cols, &["Z_PK"]).unwrap_or("Z_PK");
    let media_join = match (&t_media, c_media) {
        (Some(t), Some(c)) if media_join_on => {
            format!(" LEFT JOIN [{t}] mi ON mi.[{m_pk}] = m.[{c}]")
        }
        _ => String::new(),
    };

    let t_info = find_table(conn, &["ZWAMESSAGEINFO"]);
    let info_cols = t_info.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let (info_receipt_col, info_join) = match &t_info {
        Some(t) => {
            let r_col = pick(&info_cols, &["ZRECEIPTINFO"]);
            if let (Some(fk), Some(ipk)) = (c_info_fk, pick(&info_cols, &["Z_PK"])) {
                (
                    r_col,
                    format!(" LEFT JOIN [{t}] info ON info.[{ipk}] = m.[{fk}]"),
                )
            } else if let (Some(mpk), Some(imsg)) = (c_pk, pick(&info_cols, &["ZMESSAGE"])) {
                (
                    r_col,
                    format!(" LEFT JOIN [{t}] info ON info.[{imsg}] = m.[{mpk}]"),
                )
            } else {
                (None, String::new())
            }
        }
        None => (None, String::new()),
    };

    let t_gm = find_table(conn, &["ZWAGROUPMEMBER"]);
    let gm_cols = t_gm.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let (gm_jid_col, gm_name_col, gm_join) = match (&t_gm, c_gm_fk) {
        (Some(t), Some(fk)) if pick(&gm_cols, &["Z_PK"]).is_some() => (
            pick(&gm_cols, &["ZMEMBERJID", "ZJID"]),
            pick(&gm_cols, &["ZCONTACTNAME", "ZFIRSTNAME"]),
            format!(" LEFT JOIN [{t}] gm ON gm.[Z_PK] = m.[{fk}]"),
        ),
        _ => (None, None, String::new()),
    };

    let parent_stanza_col = if c_pk.is_some() && c_parent.is_some() {
        c_stanza
    } else {
        None
    };
    let parent_join = if let (Some(cpk), Some(cpar)) = (c_pk, c_parent) {
        format!(" LEFT JOIN [{t_msg}] pm ON pm.[{cpk}] = m.[{cpar}]")
    } else {
        String::new()
    };

    let order_clause = match (c_date, c_pk) {
        (Some(cd), Some(cp)) => format!(" ORDER BY COALESCE(m.[{cd}], 0) ASC, m.[{cp}] ASC"),
        (Some(cd), None) => format!(" ORDER BY COALESCE(m.[{cd}], 0) ASC"),
        (None, Some(cp)) => format!(" ORDER BY m.[{cp}] ASC"),
        _ => String::new(),
    };

    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM [{t_msg}] m{sess_join}{media_join}{info_join}{gm_join}{parent_join}{order_clause} LIMIT {}",
        q("m", c_pk),
        q("m", c_stanza),
        q("m", c_text),
        q("m", c_date),
        q("m", c_from),
        q("m", c_me),
        q("m", c_type),
        q("m", c_status),
        q("m", c_sess),
        q("s", s_name),
        q("s", s_jid),
        q("s", s_type),
        q("mi", m_path),
        q("mi", m_thumb),
        q("mi", m_xthumb),
        q("mi", m_title),
        q("mi", m_vname),
        q("mi", m_vstr),
        q("mi", m_meta),
        q("info", info_receipt_col),
        q("gm", gm_jid_col),
        q("gm", gm_name_col),
        q("m", c_parent),
        q("pm", parent_stanza_col),
        q("m", c_push),
        q("m", c_rcpt_cnt),
        LIMIT + 1
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn(run, f, format!("WhatsApp 消息查询失败: {e}"));
            return Ok(0);
        }
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => {
            warn(run, f, format!("WhatsApp 消息读取失败: {e}"));
            return Ok(0);
        }
    };

    let mut raw_msgs = Vec::new();
    while let Ok(Some(r)) = rows.next() {
        if raw_msgs.len() >= LIMIT {
            break;
        }
        raw_msgs.push(RawWaMsg {
            z_pk: get_i64(&r, 0).unwrap_or(0),
            stanza_id: get_text(&r, 1).unwrap_or_default(),
            text: get_text(&r, 2).unwrap_or_default(),
            date: get_f64(&r, 3),
            from_jid: get_text(&r, 4).unwrap_or_default(),
            from_me: get_i64(&r, 5),
            msg_type: get_i64(&r, 6),
            msg_status: get_i64(&r, 7),
            sess_id: get_i64(&r, 8),
            sess_name: get_text(&r, 9).unwrap_or_default(),
            sess_jid: get_text(&r, 10).unwrap_or_default(),
            sess_type: get_i64(&r, 11),
            media_local_path: get_text(&r, 12).unwrap_or_default(),
            thumb_local_path: get_text(&r, 13).unwrap_or_default(),
            xmpp_thumb_path: get_text(&r, 14).unwrap_or_default(),
            media_title: get_text(&r, 15).unwrap_or_default(),
            vcard_name: get_text(&r, 16).unwrap_or_default(),
            vcard_string: get_text(&r, 17).unwrap_or_default(),
            media_meta: get_blob(&r, 18),
            receipt_blob: get_blob(&r, 19),
            gm_jid: get_text(&r, 20).unwrap_or_default(),
            gm_name: get_text(&r, 21).unwrap_or_default(),
            parent_pk: get_i64(&r, 22),
            parent_stanza: get_text(&r, 23).unwrap_or_default(),
            push_name: get_text(&r, 24).unwrap_or_default(),
            filtered_recipient_count: get_i64(&r, 25),
        });
    }

    let mut by_pk: HashMap<i64, usize> = HashMap::new();
    let mut by_stanza: HashMap<String, usize> = HashMap::new();
    for (idx, m) in raw_msgs.iter().enumerate() {
        if m.z_pk > 0 {
            by_pk.insert(m.z_pk, idx);
        }
        if !m.stanza_id.is_empty() {
            by_stanza.insert(m.stanza_id.clone(), idx);
        }
    }

    let (extra_poll_options, extra_poll_votes) = load_external_polls(conn);
    let infra_receipts = load_messaging_infra_receipts(run, f, sides, dir);
    let mut parsed_receipts: Vec<ParsedReceiptBlob> = Vec::with_capacity(raw_msgs.len());
    let mut poll_votes_by_target: HashMap<usize, Vec<crate::wa_proto::ParsedPollVote>> =
        HashMap::new();
    let mut reactions_by_target: HashMap<usize, Vec<crate::wa_proto::ParsedReaction>> =
        HashMap::new();

    for (idx, m) in raw_msgs.iter().enumerate() {
        let mut rblob = m
            .receipt_blob
            .as_deref()
            .map(|b| decode_receipt_info_with_base(b, m.date))
            .unwrap_or_default();
        if let Some(infra) = infra_receipts.get(&m.stanza_id) {
            if !infra.played.is_empty()
                || !infra.read.is_empty()
                || !infra.delivered.is_empty()
                || !infra.pending.is_empty()
            {
                rblob.played = infra.played.clone();
                rblob.read = infra.read.clone();
                rblob.delivered = infra.delivered.clone();
                rblob.pending = infra.pending.clone();
                rblob.mode = infra.mode;
            }
        }
        if let Some(ext_votes) = extra_poll_votes.get(&m.z_pk) {
            rblob.poll_votes.extend(ext_votes.clone());
        }
        let parent_idx = m
            .parent_pk
            .and_then(|pk| by_pk.get(&pk).copied())
            .or_else(|| {
                (!m.parent_stanza.is_empty())
                    .then(|| by_stanza.get(&m.parent_stanza).copied())
                    .flatten()
            });

        if m.msg_type == Some(66) {
            if let Some(pidx) = parent_idx {
                let voter = if m.from_me == Some(1) {
                    "我 (本机账户)".to_owned()
                } else if !m.gm_jid.is_empty() {
                    m.gm_jid.clone()
                } else {
                    m.from_jid.clone()
                };
                let ts = m
                    .date
                    .and_then(|d| wa_ts(Some(d)).map(|t| t.as_str().to_owned()));
                if let Some(pv) = decode_poll_vote_payload(
                    &voter,
                    ts,
                    m.media_meta.as_deref().or(m.receipt_blob.as_deref()),
                    &m.text,
                ) {
                    poll_votes_by_target.entry(pidx).or_default().push(pv);
                }
            }
        }
        if !rblob.poll_votes.is_empty() {
            let target = parent_idx.unwrap_or(idx);
            poll_votes_by_target
                .entry(target)
                .or_default()
                .extend(rblob.poll_votes.clone());
        }
        if !rblob.reactions.is_empty() {
            let target = parent_idx.unwrap_or(idx);
            reactions_by_target
                .entry(target)
                .or_default()
                .extend(rblob.reactions.clone());
        }
        parsed_receipts.push(rblob);
    }

    let mut n = 0usize;
    for (idx, m) in raw_msgs.iter().enumerate() {
        if n % 256 == 0 {
            run.check_cancelled()?;
        }

        let outgoing = m.from_me == Some(1);
        let chat_jid = if !m.sess_jid.is_empty() {
            m.sess_jid.clone()
        } else {
            m.from_jid.clone()
        };
        let is_group = chat_jid.ends_with("@g.us")
            || m.sess_type == Some(1)
            || m.sess_id
                .and_then(|id| dir.group_members.get(&id))
                .is_some_and(|v| v.len() > 1);

        let cleaned_sess_name = clean_display_text(&m.sess_name);
        let chat_title = if !cleaned_sess_name.is_empty() {
            one_line(&cleaned_sess_name)
        } else {
            let (resolved_name, _, _) = dir.resolve(&chat_jid, "");
            if !resolved_name.is_empty() {
                one_line(&resolved_name)
            } else {
                one_line(&chat_jid)
            }
        };

        let raw_sender_jid = if outgoing {
            String::new()
        } else if !m.gm_jid.is_empty() {
            m.gm_jid.clone()
        } else if !m.from_jid.is_empty() && (!is_group || !m.from_jid.ends_with("@g.us")) {
            m.from_jid.clone()
        } else {
            String::new()
        };
        let sender_display = if outgoing {
            "我 (本机账户)".to_owned()
        } else if !raw_sender_jid.is_empty() {
            let fallback = if is_usable_display_name(&m.gm_name) {
                &m.gm_name
            } else if is_usable_display_name(&m.push_name) {
                &m.push_name
            } else {
                ""
            };
            let (name, phone, _) = dir.resolve(&raw_sender_jid, fallback);
            if !name.is_empty() && !phone.is_empty() && !name.contains(&phone) {
                format!("{name} ({phone})")
            } else if !name.is_empty() {
                name
            } else {
                raw_sender_jid.clone()
            }
        } else if is_usable_display_name(&m.push_name) {
            clean_display_text(&m.push_name)
        } else if !chat_title.is_empty() {
            chat_title.clone()
        } else {
            "未知".to_owned()
        };

        let media_display = if !m.media_local_path.trim().is_empty() {
            one_line(&m.media_local_path)
        } else if !m.thumb_local_path.trim().is_empty() {
            one_line(&m.thumb_local_path)
        } else if !m.xmpp_thumb_path.trim().is_empty() {
            one_line(&m.xmpp_thumb_path)
        } else if !m.media_title.trim().is_empty() {
            one_line(&m.media_title)
        } else {
            one_line(&m.vcard_name)
        };
        let (media_path_str, media_fref) = resolve_wa_media(
            run,
            f,
            &[&m.media_local_path, &m.thumb_local_path, &m.xmpp_thumb_path],
        );

        let reply_val = m
            .parent_pk
            .and_then(|pk| by_pk.get(&pk).copied())
            .or_else(|| {
                (!m.parent_stanza.is_empty())
                    .then(|| by_stanza.get(&m.parent_stanza).copied())
                    .flatten()
            })
            .map(|pidx| {
                let pm = &raw_msgs[pidx];
                let psender = if pm.from_me == Some(1) {
                    "我".to_owned()
                } else {
                    let j = if !pm.gm_jid.is_empty() {
                        &pm.gm_jid
                    } else {
                        &pm.from_jid
                    };
                    dir.resolve(j, &pm.push_name).0
                };
                let pcontent = if !pm.text.trim().is_empty() {
                    cap(&pm.text, 240)
                } else if !pm.media_title.trim().is_empty() {
                    cap(&pm.media_title, 240)
                } else {
                    msg_type_name(pm.msg_type)
                };
                jmap(vec![
                    ("sender", Value::from(psender)),
                    ("content", Value::from(pcontent)),
                    ("msg_type", Value::from(msg_type_name(pm.msg_type))),
                    (
                        "stanza_id",
                        (!pm.stanza_id.is_empty())
                            .then(|| Value::from(pm.stanza_id.as_str()))
                            .unwrap_or(Value::Nil),
                    ),
                ])
            })
            .unwrap_or(Value::Nil);

        let mut rblob = parsed_receipts[idx].clone();
        // Exclude self multi-device sync LID (e.g. You = 92346846855171@lid) from recipient buckets
        // unless the conversation itself is the self-chat ("You").
        if !dir.is_self_jid(&chat_jid) {
            rblob.played.retain(|p| !dir.is_self_jid(&p.jid));
            rblob.read.retain(|p| !dir.is_self_jid(&p.jid));
            rblob.delivered.retain(|p| !dir.is_self_jid(&p.jid));
            rblob.pending.retain(|p| !dir.is_self_jid(&p.jid));
        }

        // Companion-device outgoing messages (WhatsApp Web/Desktop) store per-recipient device fanout
        // in ZRECEIPTINFO (parsed into rblob.pending) while ZMESSAGESTATUS holds the aggregate receipt state.
        if outgoing
            && rblob.mode != "receipt_device"
            && rblob.played.is_empty()
            && rblob.read.is_empty()
            && rblob.delivered.is_empty()
            && !rblob.pending.is_empty()
        {
            match m.msg_status {
                Some(3 | 4 | 5 | 8 | 11 | 12 | 13) => {
                    let mut promoted = std::mem::take(&mut rblob.pending);
                    for p in &mut promoted {
                        if p.status.is_none() {
                            p.status = Some(
                                "已读（缺少 MessagingInfraDatabase.sqlite，无精确时间）".to_owned(),
                            );
                        }
                    }
                    rblob.read = promoted;
                }
                Some(2 | 6) => {
                    rblob.delivered = std::mem::take(&mut rblob.pending);
                }
                _ => {}
            }
        }

        if !is_group
            && outgoing
            && rblob.played.is_empty()
            && rblob.read.is_empty()
            && rblob.delivered.is_empty()
            && rblob.pending.is_empty()
            && !chat_jid.is_empty()
        {
            match m.msg_status {
                Some(3 | 4 | 5 | 8 | 11 | 12 | 13) => rblob.read.push(ParsedReceiptPerson {
                    jid: chat_jid.clone(),
                    ..Default::default()
                }),
                Some(2 | 6) => rblob.delivered.push(ParsedReceiptPerson {
                    jid: chat_jid.clone(),
                    ..Default::default()
                }),
                Some(0 | 1) => rblob.pending.push(ParsedReceiptPerson {
                    jid: chat_jid.clone(),
                    ..Default::default()
                }),
                _ => {}
            }
        }

        let mut delivered_by_canon: HashMap<String, Option<String>> = HashMap::new();
        for d in &rblob.delivered {
            delivered_by_canon.insert(
                canonical_jid(&d.jid),
                d.delivered_at.clone().or_else(|| d.time.clone()),
            );
        }
        let seen_set: HashSet<String> = rblob
            .played
            .iter()
            .chain(rblob.read.iter())
            .map(|p| canonical_jid(&p.jid))
            .collect();

        let played_vals: Vec<Value> = rblob
            .played
            .iter()
            .take(500)
            .map(|p| {
                let mut enriched = p.clone();
                if enriched.delivered_at.is_none() {
                    enriched.delivered_at = delivered_by_canon
                        .get(&canonical_jid(&p.jid))
                        .cloned()
                        .flatten();
                }
                receipt_person_value(&enriched, dir)
            })
            .collect();
        let read_vals: Vec<Value> = rblob
            .read
            .iter()
            .take(500)
            .map(|p| {
                let mut enriched = p.clone();
                if enriched.delivered_at.is_none() {
                    enriched.delivered_at = delivered_by_canon
                        .get(&canonical_jid(&p.jid))
                        .cloned()
                        .flatten();
                }
                receipt_person_value(&enriched, dir)
            })
            .collect();
        let delivered_only_vals: Vec<Value> = rblob
            .delivered
            .iter()
            .filter(|d| !seen_set.contains(&canonical_jid(&d.jid)))
            .take(500)
            .map(|p| receipt_person_value(p, dir))
            .collect();

        let mut unread_list: Vec<ParsedReceiptPerson> = Vec::new();
        let mut unread_seen_canons: HashSet<String> = HashSet::new();
        let sender_canon = canonical_jid(&raw_sender_jid);
        let group_list = m.sess_id.and_then(|id| dir.group_members.get(&id));
        let expected_rcpts = m.filtered_recipient_count.unwrap_or(0).max(0) as usize;
        let has_explicit_member_receipts =
            !rblob.played.is_empty() || !rblob.read.is_empty() || !rblob.delivered.is_empty();
        // Step A: 群消息中已送达但尚未读取的成员同样属于“未读成员”。
        if is_group {
            for d in &rblob.delivered {
                let c = canonical_jid(&d.jid);
                if !seen_set.contains(&c) && unread_seen_canons.insert(c) {
                    unread_list.push(ParsedReceiptPerson {
                        jid: d.jid.clone(),
                        delivered_at: d.delivered_at.clone().or_else(|| d.time.clone()),
                        ..Default::default()
                    });
                }
            }
        }
        // Step B: 显式处于待送达（pending）的接收者。
        for pend in &rblob.pending {
            let c = canonical_jid(&pend.jid);
            if !seen_set.contains(&c) && unread_seen_canons.insert(c) {
                unread_list.push(pend.clone());
            }
        }
        // Step C: receipt_device 已完整记录发信时刻的全量群接收者（含未送达成员）；
        // 仅在非 receipt_device 且未达到发信时刻历史接收人数（ZFILTEREDRECIPIENTCOUNT）时才从当前群成员表补齐。
        if is_group
            && (outgoing || has_explicit_member_receipts)
            && rblob.mode != "receipt_device"
            && (expected_rcpts == 0 || (seen_set.len() + unread_seen_canons.len()) < expected_rcpts)
        {
            if let Some(members) = group_list {
                for gm in members {
                    if expected_rcpts > 0
                        && (seen_set.len() + unread_seen_canons.len()) >= expected_rcpts
                    {
                        break;
                    }
                    if (!sender_canon.is_empty() && gm.canon_jid == sender_canon)
                        || dir.is_self_jid(&gm.jid)
                    {
                        continue;
                    }
                    if seen_set.contains(&gm.canon_jid) {
                        continue;
                    }
                    if unread_seen_canons.insert(gm.canon_jid.clone()) {
                        let del_ts = delivered_by_canon.get(&gm.canon_jid).cloned().flatten();
                        unread_list.push(ParsedReceiptPerson {
                            jid: gm.jid.clone(),
                            delivered_at: del_ts,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        let unread_vals: Vec<Value> = unread_list
            .iter()
            .take(500)
            .map(|p| receipt_person_value(p, dir))
            .collect();

        let read_cnt = read_vals.len();
        let played_cnt = played_vals.len();
        let delivered_cnt = delivered_only_vals.len();
        let unread_cnt = unread_vals.len();
        let non_self_group_count = group_list
            .map(|v| {
                v.iter()
                    .filter(|gm| {
                        (sender_canon.is_empty() || gm.canon_jid != sender_canon)
                            && !dir.is_self_jid(&gm.jid)
                    })
                    .count()
            })
            .unwrap_or(0);
        let accounted = played_cnt + read_cnt + unread_cnt;
        let recipient_denom = if rblob.mode == "receipt_device"
            || (expected_rcpts > 0 && accounted >= expected_rcpts)
        {
            accounted
        } else {
            accounted.max(non_self_group_count)
        };
        let total_members = if is_group && recipient_denom > 0 {
            recipient_denom + usize::from(outgoing)
        } else {
            0
        };

        let mut incoming_local_read = false;
        let read_summary = if played_cnt > 0 {
            if is_group && recipient_denom > 0 {
                format!("已播放 {}/{recipient_denom}", played_cnt + read_cnt)
            } else if let Some(t) = rblob.played.first().and_then(|p| p.time.as_deref()) {
                format!("已播放 {t}")
            } else {
                "已播放".to_owned()
            }
        } else if read_cnt > 0 {
            if is_group && recipient_denom > 0 {
                format!("已读 {read_cnt}/{recipient_denom}")
            } else if let Some(t) = rblob.read.first().and_then(|p| p.time.as_deref()) {
                format!("已读 {t}")
            } else {
                "已读".to_owned()
            }
        } else if delivered_cnt > 0 {
            if is_group {
                format!("已送达 {delivered_cnt} 人（未读 {unread_cnt}）")
            } else if let Some(t) = rblob.delivered.first().and_then(|p| p.time.as_deref()) {
                format!("已送达 {t}")
            } else {
                "已送达".to_owned()
            }
        } else if unread_cnt > 0 {
            if is_group {
                format!("未读 {unread_cnt}")
            } else {
                "未读".to_owned()
            }
        } else if !outgoing && !matches!(m.msg_type, Some(6 | 10 | 66)) {
            match m.msg_status {
                Some(3 | 4 | 5 | 8 | 11 | 12 | 13) => {
                    incoming_local_read = true;
                    "本机已读".to_owned()
                }
                Some(0) => "本机未读".to_owned(),
                _ => String::new(),
            }
        } else {
            String::new()
        };

        let has_receipt_data =
            played_cnt + read_cnt + delivered_cnt + unread_cnt > 0 || !read_summary.is_empty();
        let receipts_val = if incoming_local_read {
            let self_read_entry = jmap(vec![
                ("jid", Value::from("self")),
                ("name", Value::from("我 (本机账户)")),
                (
                    "status",
                    Value::from(if is_group {
                        "本机已读（WhatsApp 仅向消息发送方下发其他群成员回执）"
                    } else {
                        "本机已读"
                    }),
                ),
            ]);
            jmap(vec![
                ("summary", Value::from(read_summary.as_str())),
                ("mode", Value::from("incoming_local")),
                ("is_group", Value::Boolean(is_group)),
                ("read_count", Value::from(1i64)),
                ("played_count", Value::from(0i64)),
                ("delivered_count", Value::from(0i64)),
                ("unread_count", Value::from(0i64)),
                ("played", Value::Array(Vec::new())),
                ("read", Value::Array(vec![self_read_entry])),
                ("delivered", Value::Array(Vec::new())),
                ("unread", Value::Array(Vec::new())),
            ])
        } else if has_receipt_data {
            jmap(vec![
                (
                    "summary",
                    (!read_summary.is_empty())
                        .then(|| Value::from(read_summary.as_str()))
                        .unwrap_or(Value::Nil),
                ),
                (
                    "mode",
                    (!rblob.mode.is_empty())
                        .then(|| Value::from(rblob.mode))
                        .unwrap_or(Value::Nil),
                ),
                ("is_group", Value::Boolean(is_group)),
                (
                    "total_members",
                    (total_members > 0)
                        .then(|| Value::from(total_members as i64))
                        .unwrap_or(Value::Nil),
                ),
                ("read_count", Value::from(read_cnt as i64)),
                ("played_count", Value::from(played_cnt as i64)),
                ("delivered_count", Value::from(delivered_cnt as i64)),
                ("unread_count", Value::from(unread_cnt as i64)),
                ("played", Value::Array(played_vals)),
                ("read", Value::Array(read_vals)),
                ("delivered", Value::Array(delivered_only_vals)),
                ("unread", Value::Array(unread_vals)),
            ])
        } else {
            Value::Nil
        };

        let mut poll_val = Value::Nil;
        let mut msg_content = m.text.clone();
        if msg_content.trim().is_empty() && m.msg_type != Some(46) {
            if !m.media_title.trim().is_empty() {
                msg_content = m.media_title.clone();
            } else if m.msg_type == Some(4) && !m.vcard_name.trim().is_empty() {
                let vphone = m
                    .vcard_string
                    .lines()
                    .find_map(|line| {
                        let u = line.to_ascii_uppercase();
                        if u.starts_with("TEL") {
                            line.split_once(':').map(|(_, rhs)| rhs.trim().to_owned())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                if !vphone.is_empty() {
                    msg_content = format!("[联系人名片] {} ({vphone})", m.vcard_name.trim());
                } else {
                    msg_content = format!("[联系人名片] {}", m.vcard_name.trim());
                }
            }
        }
        if m.msg_type == Some(46) || extra_poll_options.contains_key(&m.z_pk) {
            if let Some(mut pdef) =
                decode_poll_definition(&m.text, &m.media_title, m.media_meta.as_deref())
            {
                if pdef.options.is_empty() {
                    if let Some(ext_opts) = extra_poll_options.get(&m.z_pk) {
                        for o in ext_opts {
                            pdef.options.push((o.clone(), sha256_bytes(o)));
                        }
                    }
                }
                let mut opt_voters: Vec<Vec<(String, String, Option<String>)>> =
                    vec![Vec::new(); pdef.options.len()];
                let mut unique_voters: HashSet<String> = HashSet::new();
                let mut latest_vote_by_voter: BTreeMap<String, crate::wa_proto::ParsedPollVote> =
                    BTreeMap::new();
                if let Some(votes) = poll_votes_by_target.get(&idx) {
                    for v in votes {
                        let key = canonical_jid(&v.voter_jid);
                        latest_vote_by_voter.insert(key, v.clone());
                    }
                }
                for vote in latest_vote_by_voter.values() {
                    let (vname, _vphone, vcanon) = dir.resolve(&vote.voter_jid, "");
                    unique_voters.insert(vcanon);
                    let mut matched_indices: Vec<usize> = Vec::new();
                    for h in &vote.option_hashes {
                        if let Some(pos) = pdef.options.iter().position(|(_, oh)| oh == h) {
                            if !matched_indices.contains(&pos) {
                                matched_indices.push(pos);
                            }
                        }
                    }
                    if matched_indices.is_empty() {
                        for t in &vote.option_texts {
                            if let Some(pos) = pdef.options.iter().position(|(ot, _)| ot == t) {
                                if !matched_indices.contains(&pos) {
                                    matched_indices.push(pos);
                                }
                            }
                        }
                    }
                    if matched_indices.is_empty() {
                        for &oi in &vote.option_indices {
                            if oi < pdef.options.len() && !matched_indices.contains(&oi) {
                                matched_indices.push(oi);
                            }
                        }
                    }
                    for oi in matched_indices {
                        opt_voters[oi].push((
                            vote.voter_jid.clone(),
                            vname.clone(),
                            vote.time.clone(),
                        ));
                    }
                }
                let total_votes: usize = opt_voters.iter().map(Vec::len).sum();
                let mut summary_parts = Vec::new();
                let opt_values: Vec<Value> = pdef
                    .options
                    .iter()
                    .enumerate()
                    .map(|(oi, (otext, ohash))| {
                        let voters_list = &opt_voters[oi];
                        let votes = voters_list.len();
                        let pct = if total_votes > 0 {
                            ((votes as f64) * 1000.0 / (total_votes as f64)).round() / 10.0
                        } else {
                            0.0
                        };
                        summary_parts.push(format!("{otext}: {votes}票"));
                        let voter_vals: Vec<Value> = voters_list
                            .iter()
                            .take(200)
                            .map(|(vjid, vname, vtime)| {
                                jmap(vec![
                                    ("jid", Value::from(vjid.as_str())),
                                    ("name", Value::from(vname.as_str())),
                                    ("time", vtime.as_deref().map_or(Value::Nil, Value::from)),
                                ])
                            })
                            .collect();
                        jmap(vec![
                            ("id", Value::from(hex_lower(&ohash[..8]))),
                            ("text", Value::from(otext.as_str())),
                            ("votes", Value::from(votes as i64)),
                            ("percent", Value::from(pct)),
                            ("voters", Value::Array(voter_vals)),
                        ])
                    })
                    .collect();

                poll_val = jmap(vec![
                    ("question", Value::from(pdef.question.as_str())),
                    ("allow_multiple", Value::Boolean(pdef.allow_multiple)),
                    ("total_votes", Value::from(total_votes as i64)),
                    ("total_voters", Value::from(unique_voters.len() as i64)),
                    ("options", Value::Array(opt_values)),
                ]);
                if msg_content.trim().is_empty() {
                    msg_content = format!("[投票] {}", pdef.question);
                }

                run.record_values(
                    "ios.wa_poll",
                    vec![
                        cap(&chat_jid, 500).into(),
                        cap(&chat_title, 500).into(),
                        cap(&sender_display, 500).into(),
                        ts_or_nil(wa_ts(m.date)),
                        cap(&pdef.question, 1000).into(),
                        Value::from(if pdef.allow_multiple {
                            "多选"
                        } else {
                            "单选"
                        }),
                        Value::from(unique_voters.len() as i64),
                        Value::from(total_votes as i64),
                        Value::from(summary_parts.join("；")),
                        poll_val.clone(),
                    ],
                )
                .source(crate::util::node_source(f).note(format!("ZWAMESSAGE Z_PK={}", m.z_pk)))
                .emit()?;
                n += 1;
            }
        }

        let reactions_val = reactions_by_target
            .get(&idx)
            .filter(|list| !list.is_empty())
            .map(|list| {
                Value::Array(
                    list.iter()
                        .take(200)
                        .map(|rx| {
                            let (rname, _, _) = dir.resolve(&rx.jid, "");
                            jmap(vec![
                                ("emoji", Value::from(rx.emoji.as_str())),
                                ("sender", Value::from(rname)),
                                ("jid", Value::from(rx.jid.as_str())),
                                ("time", rx.time.as_deref().map_or(Value::Nil, Value::from)),
                            ])
                        })
                        .collect(),
                )
            })
            .unwrap_or(Value::Nil);

        let mut emit = run.record_values(
            "ios.wa_msg",
            vec![
                cap(&chat_jid, 500).into(),
                cap(&chat_title, 500).into(),
                cap(&sender_display, 500).into(),
                (if outgoing { "发出" } else { "收到" }).into(),
                msg_type_name(m.msg_type).into(),
                cap(&msg_content, 4000).into(),
                cap(&media_display, 1000).into(),
                (if media_path_str.is_empty() {
                    Value::Nil
                } else {
                    Value::from(media_path_str)
                }),
                ts_or_nil(wa_ts(m.date)),
                (if read_summary.is_empty() {
                    Value::Nil
                } else {
                    Value::from(read_summary)
                }),
                receipts_val,
                poll_val,
                reactions_val,
                reply_val,
            ],
        );
        let msg_src = crate::util::node_source(f).note(format!("ZWAMESSAGE Z_PK={}", m.z_pk));
        if let Some(mf) = media_fref {
            emit = emit
                .source(mf.source().note("WhatsApp 媒体附件"))
                .source(msg_src);
        } else {
            emit = emit.source(msg_src);
        }
        emit.emit()?;
        n += 1;
    }
    Ok(n)
}

/// ZWAMEDIAITEM → ios.app_geo（含有效经纬度的媒体条目）。
fn geos(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let Some(t_media) = find_table(conn, &["ZWAMEDIAITEM"]) else {
        return Ok(0);
    };
    let mc = cols(conn, &t_media);
    let Some(c_lat) = pick(&mc, &["ZLATITUDE", "LATITUDE"]) else {
        return Ok(0);
    };
    let Some(c_lng) = pick(&mc, &["ZLONGITUDE", "LONGITUDE"]) else {
        return Ok(0);
    };
    let c_vcard = pick(&mc, &["ZVCARDNAME"]);
    let c_title = pick(&mc, &["ZTITLE"]);
    let m_pk = pick(&mc, &["Z_PK"]).unwrap_or("Z_PK");

    // 时间取该媒体条目所属消息的发送时间（若消息表可关联）。
    let t_msg = find_table(conn, &["ZWAMESSAGE"]);
    let msg_cols = t_msg.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let m_media = pick(&msg_cols, &["ZMEDIAITEM"]);
    let m_date = pick(&msg_cols, &["ZMESSAGEDATE", "ZSENTDATE"]);
    let date_expr = match (&t_msg, m_media, m_date) {
        (Some(t), Some(mk), Some(d)) => {
            format!("(SELECT MAX(m2.[{d}]) FROM [{t}] m2 WHERE m2.[{mk}] = mi.[{m_pk}])")
        }
        _ => "NULL".to_string(),
    };

    let sql = format!(
        "SELECT mi.[{c_lat}], mi.[{c_lng}], {}, {}, {date_expr} FROM [{t_media}] mi \
         WHERE mi.[{c_lat}] IS NOT NULL AND mi.[{c_lng}] IS NOT NULL \
           AND mi.[{c_lat}] BETWEEN -90 AND 90 AND mi.[{c_lng}] BETWEEN -180 AND 180 \
           AND NOT (mi.[{c_lat}] = 0 AND mi.[{c_lng}] = 0) LIMIT {}",
        q("mi", c_vcard),
        q("mi", c_title),
        LIMIT + 1
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn(run, f, format!("WhatsApp 位置查询失败: {e}"));
            return Ok(0);
        }
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => {
            warn(run, f, format!("WhatsApp 位置读取失败: {e}"));
            return Ok(0);
        }
    };

    let mut n = 0usize;
    let mut seen = 0usize;
    while let Ok(Some(r)) = rows.next() {
        if n >= LIMIT {
            break;
        }
        if seen % 256 == 0 {
            run.check_cancelled()?;
        }
        seen += 1;
        let lat = get_f64(&r, 0);
        let lng = get_f64(&r, 1);
        let (Some(lat), Some(lng)) = (lat, lng) else {
            continue;
        };
        if !lat.is_finite() || !lng.is_finite() || lat.abs() > 90.0 || lng.abs() > 180.0 {
            continue;
        }
        if lat == 0.0 && lng == 0.0 {
            continue;
        }
        let vcard = get_text(&r, 2).unwrap_or_default();
        let title = get_text(&r, 3).unwrap_or_default();
        let name = if !vcard.trim().is_empty() {
            vcard
        } else {
            title
        };
        let date = get_f64(&r, 4);
        run.record_values(
            "ios.wa_geo",
            vec![
                String::new().into(),
                cap(&one_line(&name), 500).into(),
                Value::from(lat),
                Value::from(lng),
                ts_or_nil(wa_ts(date)),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// ZWAGROUPMEMBER 汇总：会话 → (有效成员数, 管理员名列表)。
fn member_index(conn: &Connection) -> HashMap<i64, (i64, Vec<String>)> {
    let mut map: HashMap<i64, (i64, Vec<String>)> = HashMap::new();
    let Some(t_member) = find_table(conn, &["ZWAGROUPMEMBER"]) else {
        return map;
    };
    let mc = cols(conn, &t_member);
    let Some(c_sess) = pick(&mc, &["ZCHATSESSION"]) else {
        return map;
    };
    let c_active = pick(&mc, &["ZISACTIVE"]);
    let c_admin = pick(&mc, &["ZISADMIN"]);
    let c_jid = pick(&mc, &["ZMEMBERJID"]);
    let c_name = pick(&mc, &["ZCONTACTNAME"]);

    // 成员数优先统计 ZISACTIVE != 0 的活跃成员；全为 0 时退回总行数。
    let sql = match c_active {
        Some(act) => format!(
            "SELECT [{c_sess}], COUNT(*), SUM(CASE WHEN [{act}] IS NOT NULL AND [{act}] <> 0 THEN 1 ELSE 0 END) \
             FROM [{t_member}] GROUP BY [{c_sess}]"
        ),
        None => format!("SELECT [{c_sess}], COUNT(*), NULL FROM [{t_member}] GROUP BY [{c_sess}]"),
    };
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let Some(sess) = get_i64(&r, 0) else { continue };
                let total = get_i64(&r, 1).unwrap_or(0);
                let active = get_i64(&r, 2).unwrap_or(0);
                let count = if active > 0 { active } else { total };
                map.entry(sess).or_insert((count, Vec::new()));
            }
        }
    }

    // 管理员名单：优先 JID，缺失时用联系人名称。
    if let Some(is_admin) = c_admin {
        let sql = format!(
            "SELECT [{c_sess}], {}, {} FROM [{t_member}] \
             WHERE [{is_admin}] IS NOT NULL AND [{is_admin}] <> 0 ORDER BY [{c_sess}]",
            q1(c_jid),
            q1(c_name)
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(r)) = rows.next() {
                    let Some(sess) = get_i64(&r, 0) else { continue };
                    let first = get_text(&r, 1).unwrap_or_default();
                    let second = get_text(&r, 2).unwrap_or_default();
                    let name = if !first.trim().is_empty() {
                        first
                    } else {
                        second
                    };
                    if name.trim().is_empty() {
                        continue;
                    }
                    let entry = map.entry(sess).or_insert((0, Vec::new()));
                    let name = cap(&one_line(&name), 200);
                    if entry.1.len() < 200 && !entry.1.contains(&name) {
                        entry.1.push(name);
                    }
                }
            }
        }
    }
    map
}

/// ZWACHATSESSION（+ ZWAGROUPINFO） → ios.app_chat。
fn chats(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let Some(t_sess) = find_table(conn, &["ZWACHATSESSION"]) else {
        return Ok(0);
    };
    let sc = cols(conn, &t_sess);
    let c_id = pick(&sc, &["ZCONTACTJID"]);
    let c_name = pick(&sc, &["ZPARTNERNAME"]);
    let c_type = pick(&sc, &["ZSESSIONTYPE"]);
    let c_arch = pick(&sc, &["ZARCHIVED"]);
    let c_grp = pick(&sc, &["ZGROUPINFO"]);
    let c_pk = pick(&sc, &["Z_PK"]);

    let t_grp = find_table(conn, &["ZWAGROUPINFO"]);
    let gc = t_grp.as_ref().map(|t| cols(conn, t)).unwrap_or_default();
    let g_pk = pick(&gc, &["Z_PK"]).unwrap_or("Z_PK");
    let g_pk_ok = pick(&gc, &["Z_PK"]).is_some();
    let g_creator = pick(&gc, &["ZCREATORJID"]);
    let g_created = pick(&gc, &["ZCREATIONDATE"]);
    let g_pic = pick(&gc, &["ZPICTUREPATH"]);
    // 优先会话侧外键，缺失时尝试群信息侧的会话外键。
    let grp_join = match (&t_grp, c_grp, c_pk) {
        (Some(t), Some(fk), _) if g_pk_ok => format!(" LEFT JOIN [{t}] g ON g.[{g_pk}] = s.[{fk}]"),
        (Some(t), None, Some(pk)) if g_pk_ok => match pick(&gc, &["ZCHATSESSION"]) {
            Some(gfk) => format!(" LEFT JOIN [{t}] g ON g.[{gfk}] = s.[{pk}]"),
            None => String::new(),
        },
        _ => String::new(),
    };
    let has_grp_join = !grp_join.is_empty();

    let (g_creator, g_created, g_pic) = if has_grp_join {
        (g_creator, g_created, g_pic)
    } else {
        (None, None, None)
    };

    // 无 ZCONTACTJID 时用 Z_PK 作为会话标识。
    let id_expr = match (c_id, c_pk) {
        (Some(c), _) => format!("s.[{c}]"),
        (None, Some(pk)) => format!("CAST(s.[{pk}] AS TEXT)"),
        _ => "NULL".to_string(),
    };

    let sql = format!(
        "SELECT {id_expr}, {}, {}, {}, {}, {}, {}, {} FROM [{t_sess}] s{grp_join} LIMIT {}",
        q("s", c_name),
        q("s", c_type),
        q("s", c_arch),
        q("g", g_creator),
        q("g", g_created),
        q("g", g_pic),
        q("s", c_pk),
        LIMIT + 1
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn(run, f, format!("WhatsApp 会话查询失败: {e}"));
            return Ok(0);
        }
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => {
            warn(run, f, format!("WhatsApp 会话读取失败: {e}"));
            return Ok(0);
        }
    };

    let members = member_index(conn);
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        if n >= LIMIT {
            break;
        }
        if n % 256 == 0 {
            run.check_cancelled()?;
        }
        let chat_id = get_text(&r, 0).unwrap_or_default();
        let name = get_text(&r, 1).unwrap_or_default();
        let ctype = get_i64(&r, 2);
        let archived = get_i64(&r, 3);
        let creator = get_text(&r, 4).unwrap_or_default();
        let created = get_f64(&r, 5);
        let pic = get_text(&r, 6).unwrap_or_default();
        let pk = get_i64(&r, 7);

        let (count, admins) = pk
            .and_then(|k| members.get(&k))
            .cloned()
            .unwrap_or((0, Vec::new()));
        let admin_line = admins.join(",");
        run.record_values(
            "ios.wa_chat",
            vec![
                cap(&chat_id, 500).into(),
                cap(&one_line(&name), 500).into(),
                chat_type_name(ctype, &chat_id).into(),
                Value::from(count),
                cap(&admin_line, 2000).into(),
                cap(&creator, 500).into(),
                ts_or_nil(apple(created.unwrap_or(0.0))),
                (if archived == Some(1) { "是" } else { "" }).into(),
                cap(&one_line(&pic), 1000).into(),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}

/// CallHistory.sqlite → ios.wa_call（ZWACDCALLEVENT / ZWAJOINABLECALLEVENT）。
/// 对方 JID 优先取主表列，缺失时回落到参与者子表。
pub fn calls(run: &Run, f: &FileRef, sides: &[FileRef]) -> lovelycarve_plugin::Result<usize> {
    let (_tmp, conn) = match crate::backupdb::open_db(f, sides) {
        Ok(v) => v,
        Err(e) => {
            warn(run, f, format!("WhatsApp 通话库打开失败: {e}"));
            return Err(lovelycarve_plugin::Error::new("DB_OPEN", e));
        }
    };
    let mut n = 0usize;
    for (table, jid_cols) in [
        ("ZWACDCALLEVENT", &["ZGROUPJIDSTRING", "ZCALLIDSTRING"][..]),
        (
            "ZWAJOINABLECALLEVENT",
            &["ZFROMJIDSTRING", "ZGROUPJIDSTRING"][..],
        ),
    ] {
        let Some(t) = find_table(&conn, &[table]) else {
            continue;
        };
        let c = cols(&conn, &t);
        let Some(c_date) = pick(&c, &["ZDATE", "ZFIRSTDATE"]) else {
            continue;
        };
        let c_dur = pick(&c, &["ZDURATION"]);
        let c_out = pick(&c, &["ZOUTCOME", "ZCURRENTOUTCOME"]);
        let c_video = pick(&c, &["ZVIDEO"]);
        let mut jid = None;
        for want in jid_cols {
            if let Some(x) = pick(&c, &[want]) {
                jid = Some(x.to_string());
                break;
            }
        }
        let mut sel = vec!["m.Z_PK".to_string(), format!("m.[{c_date}]")];
        for x in [c_dur, c_out, c_video, jid.as_deref()]
            .into_iter()
            .flatten()
        {
            sel.push(format!("m.[{x}]"));
        }
        let sql = format!(
            "SELECT {} FROM [{t}] m ORDER BY m.[{c_date}] LIMIT {}",
            sel.join(", "),
            LIMIT + 1
        );

        // 参与者子表：部分通话只在子表记录对方 JID。
        let part = format!("{t}PARTICIPANT");
        let mut parts: HashMap<i64, String> = HashMap::new();
        if find_table(&conn, &[&part]).is_some() {
            let pc = cols(&conn, &part);
            if let (Some(fk), Some(j)) = (
                pick(&pc, &["Z1PARTICIPANTS", "Z4PARTICIPANTS", "ZCALLEVENT"]),
                pick(&pc, &["ZJIDSTRING"]),
            ) {
                if let Ok(mut s) = conn.prepare(&format!("SELECT [{fk}], [{j}] FROM [{part}]")) {
                    if let Ok(mut rs) = s.query([]) {
                        while let Ok(Some(r)) = rs.next() {
                            if let (Some(k), Some(v)) = (get_i64(&r, 0), get_text(&r, 1)) {
                                parts.entry(k).or_insert(v);
                            }
                        }
                    }
                }
            }
        }

        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let cnt = stmt.column_count();
        let Ok(mut rows) = stmt.query([]) else {
            continue;
        };
        while let Ok(Some(r)) = rows.next() {
            if n >= LIMIT {
                break;
            }
            if n % 256 == 0 {
                run.check_cancelled()?;
            }
            let pk = get_i64(&r, 0).unwrap_or(0);
            let date = get_f64(&r, 1);
            let dur = if cnt > 2 { get_f64(&r, 2) } else { None };
            let out = if cnt > 3 { get_i64(&r, 3) } else { None };
            let video = if cnt > 4 { get_i64(&r, 4) } else { None };
            let j = if cnt > 5 { get_text(&r, 5) } else { None };
            let peer = j
                .filter(|s| !s.trim().is_empty())
                .or_else(|| parts.get(&pk).cloned())
                .unwrap_or_default();
            let kind = match out {
                Some(0) => "呼入",
                Some(2) => "未接",
                Some(3) => "呼出",
                _ => "通话",
            };
            run.record_values(
                "ios.wa_call",
                vec![
                    cap(&peer, 500).into(),
                    kind.into(),
                    Value::from(dur.unwrap_or(0.0)),
                    (if video == Some(1) { "是" } else { "" }).into(),
                    ts_or_nil(wa_ts(date)),
                ],
            )
            .source(crate::util::node_source(f))
            .emit()?;
            n += 1;
        }
    }
    Ok(n)
}

/// ZWACHATSESSION 的 JID → 展示名映射（供成员变更事件补会话名）。
fn session_names(conn: &Connection) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(t) = find_table(conn, &["ZWACHATSESSION"]) else {
        return map;
    };
    let c = cols(conn, &t);
    let (Some(jid), Some(name)) = (pick(&c, &["ZCONTACTJID"]), pick(&c, &["ZPARTNERNAME"])) else {
        return map;
    };
    let sql = format!("SELECT [{jid}], [{name}] FROM [{t}]");
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(r)) = rows.next() {
                let j = get_text(&r, 0).unwrap_or_default();
                let nm = get_text(&r, 1).unwrap_or_default();
                if !j.trim().is_empty() && !nm.trim().is_empty() {
                    map.entry(j).or_insert_with(|| one_line(&nm));
                }
            }
        }
    }
    map
}

/// ZWAGROUPMEMBERSCHANGE → ios.app_msg（成员加入/移除等事件）。
fn member_changes(run: &Run, f: &FileRef, conn: &Connection) -> lovelycarve_plugin::Result<usize> {
    let Some(t) = find_table(conn, &["ZWAGROUPMEMBERSCHANGE"]) else {
        return Ok(0);
    };
    let c = cols(conn, &t);
    let c_change = pick(&c, &["ZCHANGETYPE", "ZCHANGETYPEID"]);
    let c_date = pick(
        &c,
        &[
            "ZTIMESTAMP",
            "ZCHANGEDATE",
            "ZCHANGETIME",
            "ZCHANGEDTIME",
            "ZDATE",
        ],
    );
    let c_group = pick(&c, &["ZGROUPJID", "ZCHATSESSION", "ZGROUPID"]);
    let c_members = pick(&c, &["ZMEMBERJIDS", "ZMEMBERJID", "ZMEMBERS"]);

    let sql = format!(
        "SELECT {}, {}, {}, {} FROM [{t}] LIMIT {}",
        q1(c_change),
        q1(c_date),
        q1(c_group),
        q1(c_members),
        LIMIT + 1
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn(run, f, format!("WhatsApp 成员变更查询失败: {e}"));
            return Ok(0);
        }
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => {
            warn(run, f, format!("WhatsApp 成员变更读取失败: {e}"));
            return Ok(0);
        }
    };

    let names = session_names(conn);
    let mut n = 0usize;
    while let Ok(Some(r)) = rows.next() {
        if n >= LIMIT {
            break;
        }
        if n % 256 == 0 {
            run.check_cancelled()?;
        }
        let change = get_i64(&r, 0);
        let date = get_f64(&r, 1);
        let group = get_text(&r, 2).unwrap_or_default();
        let members = get_text(&r, 3).unwrap_or_default();
        let chat = names
            .get(&group)
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| group.clone());
        let member_s = one_line(&members);
        let content = format!(
            "成员变更: {} 类型={}",
            if member_s.is_empty() {
                "-"
            } else {
                member_s.as_str()
            },
            change.map(|v| v.to_string()).unwrap_or_default()
        );
        run.record_values(
            "ios.wa_msg",
            vec![
                cap(&group, 500).into(),
                cap(&chat, 500).into(),
                "".into(),
                "".into(),
                "成员变更".into(),
                cap(&content, 4000).into(),
                "".into(),
                ts_or_nil(wa_ts(date)),
            ],
        )
        .source(crate::util::node_source(f))
        .emit()?;
        n += 1;
    }
    Ok(n)
}
