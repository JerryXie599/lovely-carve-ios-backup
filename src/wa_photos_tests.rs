use std::{collections::HashMap, fs, path::Path};

use lovelycarve_plugin::{
    testing::{Fixture, Harness},
    Value,
};
use rusqlite::Connection;

use crate::wa_proto::sha256_bytes;

fn proto_varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push(((n as u8) & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn proto_bytes(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    proto_varint(((field as u64) << 3) | 2, out);
    proto_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn proto_uint(field: u32, v: u64, out: &mut Vec<u8>) {
    proto_varint(((field as u64) << 3) | 0, out);
    proto_varint(v, out);
}

fn wal_checksum(big_end: bool, data: &[u8], init: (u32, u32)) -> (u32, u32) {
    let mut s0 = init.0;
    let mut s1 = init.1;
    let mut i = 0;
    while i + 8 <= data.len() {
        let w0: [u8; 4] = data[i..i + 4].try_into().unwrap();
        let w1: [u8; 4] = data[i + 4..i + 8].try_into().unwrap();
        let x0 = if big_end {
            u32::from_be_bytes(w0)
        } else {
            u32::from_le_bytes(w0)
        };
        let x1 = if big_end {
            u32::from_be_bytes(w1)
        } else {
            u32::from_le_bytes(w1)
        };
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
        i += 8;
    }
    (s0, s1)
}

#[test]
fn schemas_validate_including_wa_chat_layout_and_photos_wal() {
    let harness = Harness::new(crate::plugin);
    let hello = harness.hello();
    let wa_msg = hello
        .records
        .iter()
        .find(|s| s.record_type == "ios.wa_msg")
        .expect("ios.wa_msg schema");
    #[cfg(feature = "mod-chat")]
    {
        let chat = wa_msg
            .chat
            .as_ref()
            .expect("ChatLayout attached to ios.wa_msg");
        assert_eq!(chat.thread_id, "chat_id");
        assert_eq!(chat.content, "content");
        assert_eq!(chat.receipts.as_deref(), Some("receipts"));
        assert_eq!(chat.poll.as_deref(), Some("poll"));
    }
    #[cfg(not(feature = "mod-chat"))]
    assert!(wa_msg.chat.is_none());

    for id in [
        "ios.wa_poll",
        "ios.photos_tombstone",
        "ios.photos_wal_history",
        "ios.photos_wal_meta",
    ] {
        assert!(
            hello.records.iter().any(|s| s.record_type == id),
            "missing schema {id}"
        );
    }
}

#[test]
fn whatsapp_receipts_polls_lid_contacts_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chat_path = tmp.path().join("ChatStorage.sqlite");
    let contacts_path = tmp.path().join("ContactsV2.sqlite");
    let lid_path = tmp.path().join("LID.sqlite");

    // 1. ContactsV2.sqlite: maps phone JIDs to saved display names
    {
        let cconn = Connection::open(&contacts_path).expect("contacts");
        cconn
            .execute_batch(
                "CREATE TABLE ZWAADDRESSBOOKCONTACT (
                    Z_PK INTEGER PRIMARY KEY,
                    ZWHATSAPPID TEXT,
                    ZFULLNAME TEXT,
                    ZPHONENUMBER TEXT
                );",
            )
            .unwrap();
        cconn
            .execute(
                "INSERT INTO ZWAADDRESSBOOKCONTACT VALUES (1, '85211112222@s.whatsapp.net', '张三', '+852 1111 2222')",
                [],
            )
            .unwrap();
        cconn
            .execute(
                "INSERT INTO ZWAADDRESSBOOKCONTACT VALUES (2, '85233334444@s.whatsapp.net', '李四', '+852 3333 4444')",
                [],
            )
            .unwrap();
    }

    // 2. LID.sqlite: maps @lid to @s.whatsapp.net
    {
        let lconn = Connection::open(&lid_path).expect("lid");
        lconn
            .execute_batch(
                "CREATE TABLE ZWALIDMAPPING (Z_PK INTEGER PRIMARY KEY, ZLID TEXT, ZPHONENUMBER TEXT);",
            )
            .unwrap();
        lconn
            .execute(
                "INSERT INTO ZWALIDMAPPING VALUES (1, '999888@lid', '85211112222@s.whatsapp.net')",
                [],
            )
            .unwrap();
    }

    // 3. ChatStorage.sqlite: group session, protobuf receipts, bplist receipts, reactions, reply, and poll
    {
        let conn = Connection::open(&chat_path).expect("chat");
        conn.execute_batch(
            "CREATE TABLE ZWACHATSESSION (
                Z_PK INTEGER PRIMARY KEY, ZCONTACTJID TEXT, ZPARTNERNAME TEXT, ZSESSIONTYPE INTEGER
             );
             CREATE TABLE ZWAGROUPMEMBER (
                Z_PK INTEGER PRIMARY KEY, ZCHATSESSION INTEGER, ZMEMBERJID TEXT, ZCONTACTNAME TEXT, ZISACTIVE INTEGER
             );
             CREATE TABLE ZWAPROFILEPUSHNAME (
                Z_PK INTEGER PRIMARY KEY, ZJID TEXT, ZPUSHNAME TEXT
             );
             CREATE TABLE ZWAMEDIAITEM (
                Z_PK INTEGER PRIMARY KEY, ZMEDIALOCALPATH TEXT, ZTITLE TEXT, ZMETADATA BLOB
             );
             CREATE TABLE ZWAMESSAGEINFO (
                Z_PK INTEGER PRIMARY KEY, ZMESSAGE INTEGER, ZRECEIPTINFO BLOB
             );
             CREATE TABLE ZWAMESSAGE (
                Z_PK INTEGER PRIMARY KEY, ZSTANZAID TEXT, ZTEXT TEXT, ZMESSAGEDATE REAL,
                ZFROMJID TEXT, ZPUSHNAME TEXT, ZISFROMME INTEGER, ZMESSAGETYPE INTEGER,
                ZMESSAGESTATUS INTEGER, ZCHATSESSION INTEGER, ZMEDIAITEM INTEGER,
                ZMESSAGEINFO INTEGER, ZGROUPMEMBER INTEGER, ZPARENTMESSAGE INTEGER
             );",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO ZWACHATSESSION VALUES (1, '120363001@g.us', '项目调查群', 1)",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO ZWAGROUPMEMBER VALUES (10, 1, '999888@lid', NULL, 1);
             INSERT INTO ZWAGROUPMEMBER VALUES (11, 1, '85233334444@s.whatsapp.net', NULL, 1);
             INSERT INTO ZWAGROUPMEMBER VALUES (12, 1, '85255556666@s.whatsapp.net', '王五', 1);",
        )
        .unwrap();

        // Msg 1: Outgoing text with Protobuf receipts (field 2 = read, field 1 = delivered, field 7 = reaction)
        let mut read_entry = Vec::new();
        proto_bytes(1, b"999888@lid", &mut read_entry);
        proto_uint(2, 1_754_036_520, &mut read_entry); // 2025-08-01T16:22:00+08:00
        let mut deliv_entry = Vec::new();
        proto_bytes(1, b"85233334444@s.whatsapp.net", &mut deliv_entry);
        proto_uint(2, 1_754_036_460, &mut deliv_entry);
        let mut react_entry = Vec::new();
        proto_bytes(1, b"85233334444@s.whatsapp.net", &mut react_entry);
        proto_bytes(2, "👍".as_bytes(), &mut react_entry);
        proto_uint(3, 1_754_036_530, &mut react_entry);

        let mut pb_receipt = Vec::new();
        proto_bytes(1, &deliv_entry, &mut pb_receipt);
        proto_bytes(2, &read_entry, &mut pb_receipt);
        proto_bytes(7, &react_entry, &mut pb_receipt);

        conn.execute(
            "INSERT INTO ZWAMESSAGEINFO VALUES (101, 1, ?1)",
            [pb_receipt],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (1, 'stanza-1', '请确认明天的安排', 775700000.0, NULL, NULL, 1, 0, 2, 1, NULL, 101, NULL, NULL)",
            [],
        )
        .unwrap();

        // Msg 2: Quoted reply to Msg 1 with bplist00 played/read receipt
        let mut d_dict = plist::Dictionary::new();
        d_dict.insert(
            "85255556666@s.whatsapp.net".into(),
            plist::Value::Real(775700200.0),
        );
        let mut p_dict = plist::Dictionary::new();
        p_dict.insert("999888@lid".into(), plist::Value::Real(775700210.0));
        let mut root_plist = plist::Dictionary::new();
        root_plist.insert("d".into(), plist::Value::Dictionary(d_dict));
        root_plist.insert("p".into(), plist::Value::Dictionary(p_dict));
        let mut bplist_bytes = Vec::new();
        plist::Value::Dictionary(root_plist)
            .to_writer_binary(&mut bplist_bytes)
            .unwrap();

        conn.execute(
            "INSERT INTO ZWAMESSAGEINFO VALUES (102, 2, ?1)",
            [bplist_bytes],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (2, 'stanza-2', '收到语音回复', 775700100.0, '999888@lid', NULL, 0, 2, 2, 1, NULL, 102, 10, 1)",
            [],
        )
        .unwrap();

        // Msg 3: Poll (ZMESSAGETYPE=46) with protobuf metadata + Msg 4 vote (ZMESSAGETYPE=66) with sha256("火锅")
        let mut poll_meta = Vec::new();
        proto_bytes(1, "今晚聚餐去哪？".as_bytes(), &mut poll_meta);
        proto_uint(2, 1, &mut poll_meta); // single choice
        proto_bytes(3, "火锅".as_bytes(), &mut poll_meta);
        proto_bytes(3, "日料".as_bytes(), &mut poll_meta);
        conn.execute(
            "INSERT INTO ZWAMEDIAITEM VALUES (201, NULL, '今晚聚餐去哪？', ?1)",
            [poll_meta],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (3, 'stanza-poll', '今晚聚餐去哪？', 775700300.0, NULL, NULL, 1, 46, 2, 1, 201, NULL, NULL, NULL)",
            [],
        )
        .unwrap();

        let opt_hash = sha256_bytes("火锅");
        let mut vote_meta = Vec::new();
        proto_bytes(1, &opt_hash, &mut vote_meta);
        conn.execute(
            "INSERT INTO ZWAMEDIAITEM VALUES (202, NULL, NULL, ?1)",
            [vote_meta],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (4, 'stanza-vote', '', 775700400.0, '999888@lid', NULL, 0, 66, 0, 1, 202, NULL, 10, 3)",
            [],
        )
        .unwrap();
    }

    let harness = Harness::new(crate::plugin);
    let run = harness
        .run(
            "ios_whatsapp",
            Fixture::files(&[&chat_path, &contacts_path, &lid_path]),
        )
        .expect("run ios_whatsapp");

    let msgs = run.of_type("ios.wa_msg");
    assert_eq!(msgs.len(), 4);

    // Msg 1 checks: LID->ContactsV2 name resolution, read/delivered/unread, reaction
    let m1 = msgs
        .iter()
        .find(|m| m.text("content") == Some("请确认明天的安排"))
        .expect("msg 1");
    assert_eq!(m1.text("direction"), Some("发出"));
    let r1 = m1.get("receipts").expect("receipts json");
    let map1: HashMap<&str, &Value> = r1
        .as_map()
        .expect("receipts map")
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?, v)))
        .collect();
    let read_arr = map1["read"].as_array().expect("read array");
    assert_eq!(read_arr.len(), 1);
    let read_person = serde_json::to_string(&read_arr[0]).unwrap();
    assert!(
        read_person.contains("张三"),
        "expected 张三 in {read_person}"
    );
    assert!(
        read_person.contains("2025-08-01T16:22:00+08:00"),
        "missing timestamp in {read_person}"
    );

    let deliv_arr = map1["delivered"].as_array().expect("delivered array");
    assert_eq!(deliv_arr.len(), 1);
    assert!(serde_json::to_string(&deliv_arr[0])
        .unwrap()
        .contains("李四"));

    let unread_arr = map1["unread"].as_array().expect("unread array");
    assert_eq!(unread_arr.len(), 2); // 李四 + 王五
    let rx_json = serde_json::to_string(m1.get("reactions").expect("reactions")).unwrap();
    assert!(
        rx_json.contains("👍") && rx_json.contains("李四"),
        "{rx_json}"
    );

    // Msg 2 checks: incoming sender resolved via LID->ContactsV2, bplist played receipt, quoted reply
    let m2 = msgs
        .iter()
        .find(|m| m.text("content") == Some("收到语音回复"))
        .expect("msg 2");
    assert!(
        m2.text("sender").unwrap().contains("张三"),
        "got {:?}",
        m2.text("sender")
    );
    let reply_json = serde_json::to_string(m2.get("reply_to").expect("reply_to")).unwrap();
    assert!(reply_json.contains("请确认明天的安排"), "{reply_json}");
    let r2_json = serde_json::to_string(m2.get("receipts").expect("bplist receipts")).unwrap();
    assert!(r2_json.contains("\"played\""), "{r2_json}");

    // Msg 3 + ios.wa_poll checks: poll options decoded + SHA-256 vote matched + voter resolved
    let polls = run.of_type("ios.wa_poll");
    assert_eq!(polls.len(), 1);
    assert_eq!(polls[0].text("question"), Some("今晚聚餐去哪？"));
    assert_eq!(polls[0].int("total_voters"), Some(1));
    assert_eq!(polls[0].int("total_votes"), Some(1));
    assert!(polls[0]
        .text("results_summary")
        .unwrap()
        .contains("火锅: 1票"));
    let poll_json = serde_json::to_string(polls[0].get("poll").expect("poll json")).unwrap();
    assert!(
        poll_json.contains("张三"),
        "missing voter name in {poll_json}"
    );
}

#[test]
fn photos_wal_uncommitted_prior_salt_and_tombstones() {
    let live_tmp = tempfile::tempdir().expect("live");
    let live_db = live_tmp.path().join("Photos.sqlite");
    let snap_tmp = tempfile::tempdir().expect("snap");
    let db_path = snap_tmp.path().join("Photos.sqlite");
    let wal_path = snap_tmp.path().join("Photos.sqlite-wal");

    // Step A: Create WAL-mode Photos DB, keep writer connection open while snapshotting
    // DB + uncheckpointed -wal so SQLite does not auto-checkpoint & unlink -wal on close.
    {
        let conn = Connection::open(&live_db).expect("open photos");
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER PRIMARY KEY, Z_NAME TEXT, Z_MAX INTEGER);
             CREATE TABLE ZASSET (
                Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZFILENAME TEXT, ZDIRECTORY TEXT,
                ZDATECREATED REAL, ZADDEDDATE REAL, ZTRASHEDSTATE INTEGER, ZTRASHEDDATE REAL,
                ZTRASHEDREASON INTEGER, ZUUID TEXT, ZHIDDEN INTEGER, ZFAVORITE INTEGER,
                ZWIDTH INTEGER, ZHEIGHT INTEGER, ZDURATION REAL
             );
             CREATE TABLE ZADDITIONALASSETATTRIBUTES (
                Z_PK INTEGER PRIMARY KEY, ZASSET INTEGER, ZORIGINALFILENAME TEXT,
                ZORIGINALFILESIZE INTEGER, ZORIGINALSTABLEHASH TEXT,
                ZIMPORTEDBY INTEGER, ZIMPORTEDBYBUNDLEIDENTIFIER TEXT, ZIMPORTEDBYDISPLAYNAME TEXT
             );
             CREATE TABLE ZCLOUDMASTER (
                Z_PK INTEGER PRIMARY KEY, ZCLOUDMASTERGUID TEXT, ZORIGINALFILENAME TEXT
             );
             CREATE TABLE ATRANSACTIONSTRING (Z_PK INTEGER PRIMARY KEY, ZNAME TEXT);
             CREATE TABLE ATRANSACTION (Z_PK INTEGER PRIMARY KEY, ZTIMESTAMP REAL, ZPROCESSIDTS INTEGER, ZCONTEXTNAMETS INTEGER);
             CREATE TABLE ACHANGE (
                Z_PK INTEGER PRIMARY KEY, ZCHANGETYPE INTEGER, ZENTITY INTEGER, ZENTITYPK INTEGER,
                ZTRANSACTIONID INTEGER, ZTOMBSTONE0 TEXT, ZTOMBSTONE1 INTEGER, ZTOMBSTONE2 TEXT,
                ZTOMBSTONE3 REAL, ZTOMBSTONE4 INTEGER, ZTOMBSTONE5 TEXT
             );",
        )
        .unwrap();
        // Force initial schema checkpoint so schema is in base DB
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();

        // Commit 1: Insert PK=1 (created 806820794.0) and PK=2
        conn.execute_batch(
            "BEGIN;
             INSERT INTO Z_PRIMARYKEY VALUES (3, 'Asset', 4);
             INSERT INTO ZASSET VALUES (1, 3, 'IMG_FAST.PNG', 'DCIM/100APPLE', 806820794.0, 806820794.0, 0, NULL, 0, 'UUID-1', 0, 0, 1200, 900, 0.0);
             INSERT INTO ZASSET VALUES (2, 3, 'IMG_KEEP.JPG', 'DCIM/100APPLE', 806820900.0, 806820900.0, 0, NULL, 0, 'UUID-2', 0, 0, 1200, 900, 0.0);
             INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (1, 1, 'IMG_FAST.PNG', 318970, 'Aa1sazGmizbawcNL+g4HklK5aEqo', 8, 'com.apple.springboard', 'SpringBoard');
             INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (2, 2, 'IMG_KEEP.JPG', 2081467, 'AVsK+0MpZX3IkGcobRYQtckomRYl', 1, 'com.apple.camera', 'Camera');
             INSERT INTO ZCLOUDMASTER VALUES (1, 'Aa1sazGmizbawcNL+g4HklK5aEqo', 'IMG_FAST.PNG');
             INSERT INTO ZCLOUDMASTER VALUES (2, 'AVsK+0MpZX3IkGcobRYQtckomRYl', 'IMG_KEEP.JPG');
             COMMIT;",
        )
        .unwrap();

        // Commit 2: Trash PK=1 12.599 seconds later (user manually trashed: ZTRASHEDREASON=1)
        conn.execute_batch(
            "BEGIN;
             UPDATE ZASSET SET ZTRASHEDSTATE = 1, ZTRASHEDDATE = 806820806.599, ZTRASHEDREASON = 1 WHERE Z_PK = 1;
             COMMIT;",
        )
        .unwrap();

        // Commit 3: Hard delete PK=1 + record ACHANGE tombstone
        conn.execute_batch(
            "BEGIN;
             DELETE FROM ZASSET WHERE Z_PK = 1;
             DELETE FROM ZADDITIONALASSETATTRIBUTES WHERE Z_PK = 1;
             DELETE FROM ZCLOUDMASTER WHERE Z_PK = 1;
             INSERT INTO ATRANSACTIONSTRING VALUES (1, 'com.apple.mobileslideshow'), (2, 'DeleteExpired');
             INSERT INTO ATRANSACTION VALUES (243, 809856375.714, 1, 2);
             INSERT INTO ACHANGE VALUES (10, 2, 3, 1, 243, 'UUID-1#PrimarySync', 1, 'Deleting expired asset from trash', 806820806.599, 1, 'UUID-1');
             COMMIT;",
        )
        .unwrap();

        fs::copy(&live_db, &db_path).expect("copy snap db");
        fs::copy(live_tmp.path().join("Photos.sqlite-wal"), &wal_path).expect("copy snap wal");
    }

    // Build an extra synthetic WAL snapshot database for PK=3 (uncommitted frame) and PK=4 (prior-salt frame)
    let donor_dir = tempfile::tempdir().expect("donor");
    let donor_db = donor_dir.path().join("donor.sqlite");
    {
        let conn = Connection::open(&donor_db).expect("donor");
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER PRIMARY KEY, Z_NAME TEXT, Z_MAX INTEGER);
             CREATE TABLE ZASSET (
                Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZFILENAME TEXT, ZDIRECTORY TEXT,
                ZDATECREATED REAL, ZADDEDDATE REAL, ZTRASHEDSTATE INTEGER, ZTRASHEDDATE REAL,
                ZTRASHEDREASON INTEGER, ZUUID TEXT, ZHIDDEN INTEGER, ZFAVORITE INTEGER,
                ZWIDTH INTEGER, ZHEIGHT INTEGER, ZDURATION REAL
             );",
        )
        .unwrap();
        // Force enough dummy padding or match the same root page number as Photos.sqlite ZASSET root page!
        conn.execute(
            "INSERT INTO ZASSET VALUES (3, 3, 'IMG_UNCOMMITTED.JPG', 'DCIM/100APPLE', 806821000.0, 806821000.0, 1, 806821005.0, 1, 'UUID-3', 0, 0, 100, 100, 0.0)",
            [],
        )
        .unwrap();
    }

    // Read generated WAL + discover ZASSET rootpage from real Photos.sqlite
    let db_bytes = fs::read(&db_path).expect("read db");
    let mut wal_bytes = fs::read(&wal_path).expect("read wal");
    let hdr_insp = crate::sqlite_wal::inspect_wal_shm(&db_bytes, Some(&wal_bytes), None).unwrap();
    let asset_meta = crate::sqlite_wal::load_table_meta(&db_bytes, &hdr_insp, "ZASSET").unwrap();
    let asset_root = asset_meta.root_page;

    let donor_bytes = fs::read(&donor_db).expect("donor bytes");
    let mut leaf_page_bytes = vec![0u8; 4096];
    let donor_insp = crate::sqlite_wal::inspect_wal_shm(&donor_bytes, None, None).unwrap();
    let donor_meta =
        crate::sqlite_wal::load_table_meta(&donor_bytes, &donor_insp, "ZASSET").unwrap();
    let donor_pg_off = (donor_meta.root_page as usize - 1) * 4096;
    leaf_page_bytes.copy_from_slice(&donor_bytes[donor_pg_off..donor_pg_off + 4096]);

    // Walk existing WAL to find final rolling checksum & header salt
    let big_end = hdr_insp.wal.as_ref().unwrap().big_end_cksum;
    let hdr_salt = hdr_insp.wal.as_ref().unwrap().salt;
    let mut last_ck = wal_checksum(big_end, &wal_bytes[0..24], (0, 0));
    let stride = 24 + 4096;
    let mut off = 32;
    while off + stride <= wal_bytes.len() {
        let fh = &wal_bytes[off..off + 24];
        let pg = &wal_bytes[off + 24..off + stride];
        let c1 = wal_checksum(big_end, &fh[0..8], last_ck);
        last_ck = wal_checksum(big_end, pg, c1);
        off += stride;
    }

    // Append uncommitted trailing frame (current salt, db_size=0) on ZASSET root page
    {
        let mut fh = [0u8; 24];
        fh[0..4].copy_from_slice(&asset_root.to_be_bytes());
        fh[4..8].copy_from_slice(&0u32.to_be_bytes());
        fh[8..16].copy_from_slice(&hdr_salt);
        let c1 = wal_checksum(big_end, &fh[0..8], last_ck);
        let c2 = wal_checksum(big_end, &leaf_page_bytes, c1);
        fh[16..20].copy_from_slice(&c2.0.to_be_bytes());
        fh[20..24].copy_from_slice(&c2.1.to_be_bytes());
        wal_bytes.extend_from_slice(&fh);
        wal_bytes.extend_from_slice(&leaf_page_bytes);
    }

    // Append a prior-salt residual frame with a different filename (swap row to IMG_PRIORSALT.JPG)
    {
        let mut prior_page = leaf_page_bytes.clone();
        if let Some(pos) = prior_page
            .windows(b"IMG_UNCOMMITTED.JPG".len())
            .position(|w| w == b"IMG_UNCOMMITTED.JPG")
        {
            let rep = b"IMG_PRIORSALT00.JPG";
            prior_page[pos..pos + rep.len()].copy_from_slice(rep);
        }
        let mut fh = [0u8; 24];
        fh[0..4].copy_from_slice(&asset_root.to_be_bytes());
        fh[4..8].copy_from_slice(&1u32.to_be_bytes());
        fh[8..16].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        wal_bytes.extend_from_slice(&fh);
        wal_bytes.extend_from_slice(&prior_page);
    }
    fs::write(&wal_path, &wal_bytes).expect("rewrite wal");

    let run = Harness::new(crate::plugin)
        .run("ios_photos", Fixture::files(&[&db_path, &wal_path]))
        .expect("run ios_photos");

    // 1. Active asset check: filename must be IMG_KEEP.JPG (NOT DCIM/100APPLE!)
    let assets = run.of_type("ios.photos_asset");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].text("filename"), Some("IMG_KEEP.JPG"));

    // 2. WAL history checks: PK=1 recovered with trash_delay_secs ~ 12.599, plus uncommitted & prior-salt frames
    let wal_hist = run.of_type("ios.photos_wal_history");
    let pk1_trashed = wal_hist
        .iter()
        .find(|r| {
            r.int("asset_pk") == Some(1)
                && r.text("filename") == Some("IMG_FAST.PNG")
                && r.get("trashed_date").is_some()
        })
        .expect("recovered trashed WAL snapshot for PK=1");
    assert_eq!(pk1_trashed.text("original_filename"), Some("IMG_FAST.PNG"));
    assert_eq!(pk1_trashed.int("original_filesize"), Some(318970));
    assert_eq!(
        pk1_trashed.text("cloud_master_guid"),
        Some("Aa1sazGmizbawcNL+g4HklK5aEqo")
    );
    let dt = pk1_trashed
        .get("trash_delay_secs")
        .and_then(Value::as_f64)
        .expect("trash_delay_secs");
    assert!((dt - 12.599).abs() < 0.02, "expected dt ~ 12.599, got {dt}");
    assert!(
        pk1_trashed
            .text("trashed_reason")
            .unwrap_or("")
            .contains("用户主动移至「最近删除」"),
        "unexpected trashed_reason: {:?}",
        pk1_trashed.text("trashed_reason")
    );

    assert!(
        wal_hist
            .iter()
            .any(|r| r.text("filename") == Some("IMG_UNCOMMITTED.JPG")
                && r.text("wal_status") == Some("未提交 WAL 帧")),
        "missing uncommitted WAL frame recovery in {wal_hist:#?}"
    );
    assert!(
        wal_hist
            .iter()
            .any(|r| r.text("filename") == Some("IMG_PRIORSALT00.JPG")
                && r.text("wal_status") == Some("旧 Salt 残留帧")),
        "missing prior-salt WAL frame recovery in {wal_hist:#?}"
    );

    // 3. Tombstone check: PK=1 (not 243!), recovered filename, delay ~12.599, no 2001-01-01
    let tombs = run.of_type("ios.photos_tombstone");
    assert_eq!(tombs.len(), 1);
    assert_eq!(tombs[0].int("entity_pk"), Some(1));
    assert_eq!(tombs[0].text("recovered_filename"), Some("IMG_FAST.PNG"));
    assert_eq!(tombs[0].int("original_filesize"), Some(318970));
    assert_eq!(
        tombs[0].text("cloud_master_guid"),
        Some("Aa1sazGmizbawcNL+g4HklK5aEqo")
    );
    let tomb_dt = tombs[0]
        .get("trash_delay_secs")
        .and_then(Value::as_f64)
        .expect("tombstone trash_delay_secs");
    assert!((tomb_dt - 12.599).abs() < 0.02, "got {tomb_dt}");
    assert!(!tombs[0]
        .text("trashed_date")
        .unwrap_or("")
        .starts_with("2001-"));
    assert!(!tombs[0]
        .text("delete_time")
        .unwrap_or("")
        .starts_with("2001-"));

    // 4. WAL meta check
    assert!(run.count("ios.photos_wal_meta") >= 2);
}

#[test]
#[ignore]
fn real_fixture_photos_recovers_deleted_fast_trashed_asset() {
    let dir = Path::new(r"E:\电子取证\26美亚个人赛\ios\图片");
    let db = dir.join("Photos.sqlite");
    let wal = dir.join("Photos.sqlite-wal");
    let shm = dir.join("Photos.sqlite-shm");
    assert!(db.is_file(), "missing real fixture DB at {}", db.display());

    let run = Harness::new(crate::plugin)
        .run("ios_photos", Fixture::files(&[&db, &wal, &shm]))
        .expect("run ios_photos on real fixture");

    let assets = run.of_type("ios.photos_asset");
    assert_eq!(assets.len(), 19, "expected 19 active assets");
    for a in &assets {
        let name = a.text("filename").expect("filename");
        assert!(!name.contains('/'), "filename leaked directory: {name}");
    }

    let wal_hist = run.of_type("ios.photos_wal_history");
    let pk1 = wal_hist
        .iter()
        .find(|r| {
            r.int("asset_pk") == Some(1)
                && r.text("filename") == Some("IMG_0001.PNG")
                && r.get("trashed_date").is_some()
        })
        .expect("expected PK=1 IMG_0001.PNG recovered from WAL");
    let dt = pk1
        .get("trash_delay_secs")
        .and_then(Value::as_f64)
        .expect("trash_delay_secs");
    println!(
        "REAL FIXTURE WAL PK=1: filename={:?} orig={:?} created={:?} trashed={:?} dt={dt:.3}s reason={:?} state={:?} frame={:?}",
        pk1.text("filename"),
        pk1.text("original_filename"),
        pk1.text("created"),
        pk1.text("trashed_date"),
        pk1.text("trashed_reason"),
        pk1.text("record_state"),
        pk1.int("wal_frame"),
    );
    assert!(
        (12.5..=12.7).contains(&dt),
        "expected ~12.599s delay for IMG_0001.PNG, got {dt}"
    );

    let tombs = run.of_type("ios.photos_tombstone");
    assert_eq!(tombs.len(), 1);
    assert_eq!(tombs[0].int("entity_pk"), Some(1));
    assert_eq!(tombs[0].text("recovered_filename"), Some("IMG_0001.PNG"));
    assert_eq!(tombs[0].int("original_filesize"), Some(318970));
    assert_eq!(
        tombs[0].text("cloud_master_guid"),
        Some("Aa1sazGmizbawcNL+g4HklK5aEqo")
    );
    assert_eq!(pk1.int("original_filesize"), Some(318970));
    assert_eq!(
        pk1.text("cloud_master_guid"),
        Some("Aa1sazGmizbawcNL+g4HklK5aEqo")
    );
    let t_dt = tombs[0]
        .get("trash_delay_secs")
        .and_then(Value::as_f64)
        .expect("tombstone dt");
    assert!((12.5..=12.7).contains(&t_dt), "got {t_dt}");
    assert!(!tombs[0]
        .text("trashed_date")
        .unwrap_or("")
        .starts_with("2001-"));
    println!(
        "REAL FIXTURE TOMBSTONE: entity={:?} pk={:?} file={:?} size={:?} cloud_master_guid={:?} created={:?} trashed={:?} dt={t_dt:.3}s delete_time={:?} del_reason={:?}",
        tombs[0].text("entity"),
        tombs[0].int("entity_pk"),
        tombs[0].text("recovered_filename"),
        tombs[0].int("original_filesize"),
        tombs[0].text("cloud_master_guid"),
        tombs[0].text("created"),
        tombs[0].text("trashed_date"),
        tombs[0].text("delete_time"),
        tombs[0].text("delete_reason"),
    );
}

#[test]
#[ignore]
fn real_fixture_whatsapp_bcd_receipts_media_and_names() {
    let dir = Path::new(r"E:\电子取证\26美亚个人赛\ios\tele");
    let chat_db = dir.join("ChatStorage.sqlite");
    assert!(
        chat_db.is_file(),
        "missing ChatStorage.sqlite at {}",
        chat_db.display()
    );
    let mut files = vec![chat_db.clone()];
    for name in [
        "ChatStorage.sqlite-wal",
        "ChatStorage.sqlite-shm",
        "ContactsV2.sqlite",
        "ContactsV2.sqlite-wal",
        "LID.sqlite",
        "LID.sqlite-wal",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    let refs: Vec<&Path> = files.iter().map(std::path::PathBuf::as_path).collect();
    let run = Harness::new(crate::plugin)
        .run("ios_whatsapp", Fixture::files(&refs))
        .expect("run ios_whatsapp on real fixture");

    let msgs = run.of_type("ios.wa_msg");
    assert!(
        msgs.len() > 350,
        "expected >350 wa_msg records, got {}",
        msgs.len()
    );

    // 1. No sender should contain raw base64 protobuf strings like CKj9xdMGIAB...=
    for m in &msgs {
        let sender = m.text("sender").unwrap_or("");
        assert!(
            !sender.contains("GIAB") && !sender.contains("GIAA") && !sender.contains("=="),
            "base64 gibberish leaked into sender: {sender}"
        );
    }

    // 2. Outgoing message with BCD JIDs + relative offsets ("website also prepared" / "Tronic.apk")
    let tronic_apk = msgs
        .iter()
        .find(|m| m.text("content") == Some("Tronic.apk"))
        .expect("Tronic.apk message");
    assert_eq!(tronic_apk.text("read_summary"), Some("已读 2/2"));
    let r_apk = serde_json::to_string(tronic_apk.get("receipts").expect("receipts")).unwrap();
    println!("REAL WA Tronic.apk receipts: {r_apk}");
    assert!(r_apk.contains("Chan Lap"), "missing Chan Lap in {r_apk}");
    assert!(
        r_apk.contains("Cheung Mei"),
        "missing Cheung Mei in {r_apk}"
    );
    assert!(
        r_apk.contains("2026-08-04T11:53:33+08:00"),
        "missing Chan Lap read time in {r_apk}"
    );
    assert!(
        r_apk.contains("2026-08-04T12:12:26+08:00"),
        "missing Cheung Mei read time in {r_apk}"
    );

    // 3. Companion-device outgoing message in Tronic Working Group ("Wow!" / "Yes! Focus on this project.")
    let ok_msg = msgs
        .iter()
        .find(|m| m.text("content") == Some("Wow!"))
        .expect("Wow! outgoing message in Tronic Working Group");
    assert_eq!(ok_msg.text("read_summary"), Some("已读 3/3"));
    let r_ok = serde_json::to_string(ok_msg.get("receipts").expect("receipts")).unwrap();
    println!("REAL WA companion-device outgoing receipts: {r_ok}");
    assert!(
        r_ok.contains("Chan Lap") && r_ok.contains("Cheung Mei"),
        "{r_ok}"
    );

    // 4. Incoming message in Tronic Working Group ("Bosses, this message is forward by agents!...")
    let incoming_msg = msgs
        .iter()
        .find(|m| {
            m.text("content")
                .unwrap_or("")
                .starts_with("Bosses, this message is forward by agents!")
        })
        .expect("Bosses incoming message");
    assert_eq!(
        incoming_msg.text("read_summary"),
        Some("本机已读"),
        "incoming message must NOT show 未读 3"
    );

    // 5. Image messages in Tronic Working Group populate media_path and fmedia from ZXMPPTHUMBPATH (.thumb)
    let tronic_imgs: Vec<_> = msgs
        .iter()
        .filter(|m| {
            m.text("chat_name") == Some("Tronic Working Group")
                && m.text("msg_type") == Some("图片")
        })
        .collect();
    assert!(
        !tronic_imgs.is_empty(),
        "expected image messages in Tronic Working Group"
    );
    for img in &tronic_imgs {
        let mp = img.text("media_path").unwrap_or("");
        assert!(
            mp.ends_with(".thumb") || mp.ends_with(".jpg"),
            "expected media_path on Tronic Working Group image, got {mp:?}"
        );
    }
    println!(
        "REAL WA Tronic Working Group images count={} sample media_path={:?}",
        tronic_imgs.len(),
        tronic_imgs[0].text("media_path")
    );
}

#[test]
fn whatsapp_messaging_infra_receipt_device_and_historical_group_size() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let chat_path = tmp.path().join("ChatStorage.sqlite");
    let contacts_path = tmp.path().join("ContactsV2.sqlite");
    let bus_dir = tmp.path().join("BackedUpStorageDB");
    let mi_dir = tmp.path().join("MessagingInfraDB_v2");
    fs::create_dir_all(&bus_dir).unwrap();
    fs::create_dir_all(&mi_dir).unwrap();
    let bus_path = bus_dir.join("BackedUpStorageDatabase.sqlite");
    let mi_path = mi_dir.join("MessagingInfraDatabase.sqlite");

    // 1. ContactsV2: names for user1 and user2 (user3 intentionally omitted -> resolved via BackedUpStorageDatabase)
    {
        let cconn = Connection::open(&contacts_path).unwrap();
        cconn
            .execute_batch(
                "CREATE TABLE ZWAADDRESSBOOKCONTACT (
                    Z_PK INTEGER PRIMARY KEY, ZWHATSAPPID TEXT, ZLID TEXT, ZFULLNAME TEXT, ZPHONENUMBER TEXT
                );
                INSERT INTO ZWAADDRESSBOOKCONTACT VALUES (1, '85210001001@s.whatsapp.net', '1001@lid', 'Alice', '+852 1000 1001');
                INSERT INTO ZWAADDRESSBOOKCONTACT VALUES (2, '85210001002@s.whatsapp.net', '1002@lid', 'Bob', '+852 1000 1002');",
            )
            .unwrap();
    }

    // 2. BackedUpStorageDatabase: push_name for user3
    {
        let bconn = Connection::open(&bus_path).unwrap();
        bconn
            .execute_batch(
                "CREATE TABLE profile_push_name (pk INTEGER PRIMARY KEY, jid TEXT NOT NULL UNIQUE, push_name TEXT NOT NULL);
                 INSERT INTO profile_push_name VALUES (1, '1003@lid', 'Charlie');",
            )
            .unwrap();
    }

    // 3. ChatStorage: 3 active group members (user3 joined later), Msg A (ZFILTEREDRECIPIENTCOUNT=2), Msg B (ZFILTEREDRECIPIENTCOUNT=3)
    {
        let mut self_push_proto = Vec::new();
        proto_bytes(3, b"9999@lid", &mut self_push_proto);
        let self_push_b64 = {
            const B64: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in self_push_proto.chunks(3) {
                let b0 = chunk[0] as usize;
                let b1 = *chunk.get(1).unwrap_or(&0) as usize;
                let b2 = *chunk.get(2).unwrap_or(&0) as usize;
                let n = (b0 << 16) | (b1 << 8) | b2;
                out.push(B64[(n >> 18) & 63] as char);
                out.push(B64[(n >> 12) & 63] as char);
                out.push(if chunk.len() > 1 {
                    B64[(n >> 6) & 63] as char
                } else {
                    '='
                });
                out.push(if chunk.len() > 2 {
                    B64[n & 63] as char
                } else {
                    '='
                });
            }
            out
        };

        let conn = Connection::open(&chat_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZWACHATSESSION (Z_PK INTEGER PRIMARY KEY, ZCONTACTJID TEXT, ZPARTNERNAME TEXT, ZSESSIONTYPE INTEGER);
             CREATE TABLE ZWAGROUPMEMBER (Z_PK INTEGER PRIMARY KEY, ZCHATSESSION INTEGER, ZMEMBERJID TEXT, ZCONTACTNAME TEXT, ZISACTIVE INTEGER);
             CREATE TABLE ZWAMESSAGEINFO (Z_PK INTEGER PRIMARY KEY, ZMESSAGE INTEGER, ZRECEIPTINFO BLOB);
             CREATE TABLE ZWAMESSAGE (
                Z_PK INTEGER PRIMARY KEY, ZSTANZAID TEXT, ZTEXT TEXT, ZMESSAGEDATE REAL,
                ZFROMJID TEXT, ZPUSHNAME TEXT, ZISFROMME INTEGER, ZMESSAGETYPE INTEGER,
                ZMESSAGESTATUS INTEGER, ZCHATSESSION INTEGER, ZMEDIAITEM INTEGER,
                ZMESSAGEINFO INTEGER, ZGROUPMEMBER INTEGER, ZPARENTMESSAGE INTEGER,
                ZFILTEREDRECIPIENTCOUNT INTEGER
             );
             INSERT INTO ZWACHATSESSION VALUES (1, '120363999@g.us', 'Historical Roster Group', 1);
             INSERT INTO ZWAGROUPMEMBER VALUES (1, 1, '1001@lid', NULL, 1);
             INSERT INTO ZWAGROUPMEMBER VALUES (2, 1, '1002@lid', NULL, 1);
             INSERT INTO ZWAGROUPMEMBER VALUES (3, 1, '1003@lid', NULL, 1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (1, '3A_MSG_A', 'Early msg for 2', 806894508.0, NULL, ?1, 1, 0, 8, 1, NULL, NULL, NULL, NULL, 2)",
            [&self_push_b64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZWAMESSAGE VALUES (2, '3A_MSG_B', 'Later msg for 3', 807515553.0, NULL, ?1, 1, 0, 2, 1, NULL, NULL, NULL, NULL, 3)",
            [&self_push_b64],
        )
        .unwrap();
    }

    // 4. MessagingInfraDatabase: receipt_device rows
    {
        let mconn = Connection::open(&mi_path).unwrap();
        mconn
            .execute_batch(
                "CREATE TABLE receipt_device (
                    _id INTEGER PRIMARY KEY AUTOINCREMENT,
                    stanza_id TEXT NOT NULL,
                    chat_jid TEXT NOT NULL,
                    user_jid TEXT NOT NULL,
                    device_id INTEGER NOT NULL,
                    send_timestamp INTEGER,
                    delivered_timestamp INTEGER,
                    read_timestamp INTEGER,
                    played_timestamp INTEGER
                );
                -- Msg A: Alice dev=0 read; Bob dev=0 delivered only, dev=3 read; self dev=4 must be excluded
                INSERT INTO receipt_device (stanza_id, chat_jid, user_jid, device_id, send_timestamp, delivered_timestamp, read_timestamp, played_timestamp) VALUES
                    ('3A_MSG_A', '120363999@g.us', '1001@lid', 0, 1785201708, 1785201709, 1785201715, NULL),
                    ('3A_MSG_A', '120363999@g.us', '1002@lid', 0, 1785201708, 1785201709, NULL, NULL),
                    ('3A_MSG_A', '120363999@g.us', '1002@lid', 3, 1785201708, 1785201708, 1785201726, NULL),
                    ('3A_MSG_A', '120363999@g.us', '9999@lid', 4, 1785201708, 1785201900, NULL, NULL);
                -- Msg B: Alice read; Bob delivered only; Charlie pending NULL/NULL
                INSERT INTO receipt_device (stanza_id, chat_jid, user_jid, device_id, send_timestamp, delivered_timestamp, read_timestamp, played_timestamp) VALUES
                    ('3A_MSG_B', '120363999@g.us', '1001@lid', 0, 1785822753, 1785822755, 1785822757, NULL),
                    ('3A_MSG_B', '120363999@g.us', '1002@lid', 0, 1785822753, 1785822755, NULL, NULL),
                    ('3A_MSG_B', '120363999@g.us', '1003@lid', 0, 1785822753, NULL, NULL, NULL);",
            )
            .unwrap();
    }

    let run = Harness::new(crate::plugin)
        .run(
            "ios_whatsapp",
            Fixture::files(&[&chat_path, &contacts_path, &bus_path, &mi_path]),
        )
        .expect("run ios_whatsapp");
    let msgs = run.of_type("ios.wa_msg");
    assert_eq!(msgs.len(), 2);

    // Msg A: historical 2/2, Charlie NOT falsely marked unread, self 9999@lid excluded, Bob uses dev=3 read timestamp
    let ma = msgs
        .iter()
        .find(|m| m.text("content") == Some("Early msg for 2"))
        .expect("Msg A");
    assert_eq!(ma.text("read_summary"), Some("已读 2/2"));
    let ra = serde_json::to_string(ma.get("receipts").expect("Msg A receipts")).unwrap();
    assert!(
        ra.contains("2026-07-28T09:21:55+08:00"),
        "missing Alice read ts: {ra}"
    );
    assert!(
        ra.contains("2026-07-28T09:22:06+08:00"),
        "missing Bob dev=3 read ts: {ra}"
    );
    assert!(
        !ra.contains("Charlie") && !ra.contains("1003@lid"),
        "future member leaked into Msg A: {ra}"
    );
    assert!(!ra.contains("9999@lid"), "self LID leaked into Msg A: {ra}");
    let ra_map: HashMap<&str, &Value> = ma
        .get("receipts")
        .unwrap()
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?, v)))
        .collect();
    assert_eq!(ra_map["unread_count"].as_i64(), Some(0));

    // Msg B: 1/3 read, 1 delivered-only (Bob), 2 unread (Bob + Charlie resolved via BackedUpStorageDatabase)
    let mb = msgs
        .iter()
        .find(|m| m.text("content") == Some("Later msg for 3"))
        .expect("Msg B");
    assert_eq!(mb.text("read_summary"), Some("已读 1/3"));
    let rb_map: HashMap<&str, &Value> = mb
        .get("receipts")
        .unwrap()
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?, v)))
        .collect();
    assert_eq!(rb_map["read_count"].as_i64(), Some(1));
    assert_eq!(rb_map["delivered_count"].as_i64(), Some(1));
    assert_eq!(rb_map["unread_count"].as_i64(), Some(2));
    let rb = serde_json::to_string(mb.get("receipts").unwrap()).unwrap();
    assert!(
        rb.contains("Charlie"),
        "missing BackedUpStorage push name Charlie: {rb}"
    );
    assert!(
        rb.contains("2026-08-04T13:52:37+08:00"),
        "missing Alice read ts in Msg B: {rb}"
    );
}

#[test]
#[ignore]
fn real_fixture_cm01_whatsapp_messaging_infra_receipt_timestamps() {
    let dir = Path::new(
        r"E:\电子取证\26美亚团体赛\hermes-workspace\CM-01.zip\work\ext\var\mobile\Applications\group.net.whatsapp.WhatsApp.shared",
    );
    let chat_db = dir.join("ChatStorage.sqlite");
    assert!(
        chat_db.is_file(),
        "missing CM-01 ChatStorage.sqlite at {}",
        chat_db.display()
    );

    let mut files = vec![chat_db];
    for rel in [
        "ChatStorage.sqlite-wal",
        "ChatStorage.sqlite-shm",
        "ContactsV2.sqlite",
        "ContactsV2.sqlite-wal",
        "LID.sqlite",
        "LID.sqlite-wal",
        r"MessagingInfraDB_v2\MessagingInfraDatabase.sqlite",
        r"MessagingInfraDB_v2\MessagingInfraDatabase.sqlite-wal",
        r"BackedUpStorageDB\BackedUpStorageDatabase.sqlite",
        r"BackedUpStorageDB\BackedUpStorageDatabase.sqlite-wal",
        r"ExtChatDB\ExtChatDatabase.sqlite",
        r"ExtChatDB\ExtChatDatabase.sqlite-wal",
    ] {
        let p = dir.join(rel);
        if p.is_file() {
            files.push(p);
        }
    }
    let refs: Vec<&Path> = files.iter().map(std::path::PathBuf::as_path).collect();
    let run = Harness::new(crate::plugin)
        .run("ios_whatsapp", Fixture::files(&refs))
        .expect("run ios_whatsapp on CM-01 real fixture");

    let msgs = run.of_type("ios.wa_msg");
    assert!(
        msgs.len() > 200,
        "expected >200 wa_msg records, got {}",
        msgs.len()
    );

    // 1. msg=58 ("Hi, guys! I just opened a group for our working!"): 2/2 read (Ching Fung joined 6 days later)
    let m58 = msgs
        .iter()
        .find(|m| m.text("content") == Some("Hi, guys! I just opened a group for our working!"))
        .expect("CM-01 msg=58");
    assert_eq!(m58.text("read_summary"), Some("已读 2/2"));
    let r58 = serde_json::to_string(m58.get("receipts").expect("m58 receipts")).unwrap();
    println!("CM-01 msg=58 receipts: {r58}");
    assert!(
        r58.contains("2026-07-28T09:21:49+08:00"),
        "missing Boss read ts: {r58}"
    );
    assert!(
        r58.contains("2026-07-28T09:22:06+08:00"),
        "missing Lik Lee read ts: {r58}"
    );
    assert!(
        !r58.contains("Ching Fung") && !r58.contains("92346846855171@lid"),
        "future member in m58: {r58}"
    );

    // 2. msg=524 ("Two clients invested!..." in Tronic Working Group, outgoing): 3/3 read with exact timestamps, self excluded
    let m524 = msgs
        .iter()
        .find(|m| {
            m.text("chat_name") == Some("Tronic Working Group")
                && m.text("direction") == Some("发出")
                && m.text("content")
                    .unwrap_or("")
                    .starts_with("Two clients invested!")
        })
        .expect("CM-01 msg=524");
    assert_eq!(m524.text("read_summary"), Some("已读 3/3"));
    let r524 = serde_json::to_string(m524.get("receipts").expect("m524 receipts")).unwrap();
    println!("CM-01 msg=524 receipts: {r524}");
    assert!(
        r524.contains("2026-08-04T13:52:37+08:00"),
        "missing Boss read ts: {r524}"
    );
    assert!(
        r524.contains("2026-08-04T13:54:27+08:00"),
        "missing Lik Lee read ts: {r524}"
    );
    assert!(
        r524.contains("2026-08-04T14:32:37+08:00"),
        "missing Ching Fung read ts: {r524}"
    );
    assert!(
        r524.contains("2026-08-04T13:52:35+08:00"),
        "missing delivered ts: {r524}"
    );
    assert!(
        !r524.contains("192252030808131"),
        "self LID leaked into m524: {r524}"
    );

    // 3. msg=525 ("Bosses, this message is forward by agents!..." in Tronic Working Group, outgoing): 3/3 read, exact timestamps, self excluded
    let m525 = msgs
        .iter()
        .find(|m| {
            m.text("chat_name") == Some("Tronic Working Group")
                && m.text("direction") == Some("发出")
                && m.text("content")
                    .unwrap_or("")
                    .starts_with("Bosses, this message is forward by agents!")
        })
        .expect("CM-01 msg=525");
    assert_eq!(m525.text("read_summary"), Some("已读 3/3"));
    let r525 = serde_json::to_string(m525.get("receipts").expect("m525 receipts")).unwrap();
    println!("CM-01 msg=525 receipts: {r525}");
    assert!(
        r525.contains("2026-08-04T13:54:09+08:00"),
        "missing Boss read ts: {r525}"
    );
    assert!(
        r525.contains("2026-08-04T13:54:27+08:00"),
        "missing Lik Lee read ts: {r525}"
    );
    assert!(
        r525.contains("2026-08-04T14:32:37+08:00"),
        "missing Ching Fung read ts: {r525}"
    );
    assert!(
        !r525.contains("192252030808131"),
        "self LID leaked into m525: {r525}"
    );

    // 4. msg=54 ("Hi, guys."): 1/2 read (Boss read at 09:16:57, 104450081423401@lid pending unread)
    let m54 = msgs
        .iter()
        .find(|m| m.text("content") == Some("Hi, guys."))
        .expect("CM-01 msg=54");
    assert_eq!(m54.text("read_summary"), Some("已读 1/2"));
    let r54 = serde_json::to_string(m54.get("receipts").expect("m54 receipts")).unwrap();
    println!("CM-01 msg=54 receipts: {r54}");
    assert!(
        r54.contains("2026-07-28T09:16:57+08:00"),
        "missing Boss read ts in m54: {r54}"
    );
    assert!(
        r54.contains("104450081423401@lid"),
        "missing unread member in m54: {r54}"
    );
}

#[test]
fn logical_dump_user_and_bare_containers_resolve_with_slash_and_notes_sqlite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let user_root = tmp.path().join("logical_user");
    let app_uuid = "11111111-2222-3333-4444-555555555555";
    let meta_dir = user_root
        .join("User")
        .join("Containers")
        .join("Data")
        .join("Application")
        .join(app_uuid);
    fs::create_dir_all(&meta_dir).unwrap();
    fs::write(
        meta_dir.join(".com.apple.mobile_container_manager.metadata.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>MCMMetadataIdentifier</key><string>com.example.userdump</string></dict></plist>"#,
    )
    .unwrap();
    let notes_dir = user_root.join("User").join("Library").join("Notes");
    fs::create_dir_all(&notes_dir).unwrap();
    let notes_db = notes_dir.join("notes.sqlite");
    {
        let conn = Connection::open(&notes_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZNOTEBODY (Z_PK INTEGER PRIMARY KEY, ZCONTENT TEXT);
             CREATE TABLE ZNOTE (
                 Z_PK INTEGER PRIMARY KEY,
                 ZTITLE TEXT,
                 ZSUMMARY TEXT,
                 ZCREATIONDATE REAL,
                 ZMODIFICATIONDATE REAL,
                 ZDELETEDFLAG INTEGER,
                 ZBODY INTEGER
             );
             INSERT INTO ZNOTEBODY VALUES (10, 'Meeting body detail from User dump');
             INSERT INTO ZNOTE VALUES (1, 'Secret Plan', 'Plan summary', 700000000.0, 700000100.0, 0, 10);
             INSERT INTO ZNOTE VALUES (2, '', 'Deleted snippet', 700000200.0, NULL, 1, NULL);",
        )
        .unwrap();
    }

    // Bare root (dump started inside `User/` directly)
    let bare_root = tmp.path().join("logical_bare");
    let bare_uuid = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    let bare_meta = bare_root
        .join("Containers")
        .join("Data")
        .join("Application")
        .join(bare_uuid);
    fs::create_dir_all(&bare_meta).unwrap();
    fs::write(
        bare_meta.join(".com.apple.mobile_container_manager.metadata.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>MCMMetadataIdentifier</key><string>com.example.baredump</string></dict></plist>"#,
    )
    .unwrap();
    fs::create_dir_all(bare_root.join("Library").join("Notes")).unwrap();
    fs::copy(
        &notes_db,
        bare_root.join("Library").join("Notes").join("notes.sqlite"),
    )
    .unwrap();

    // Verify `app_container_index` resolves both `User/...` and bare `Containers/...` with `/` before `.com.apple...`.
    fn user_probe_plugin() -> lovelycarve_plugin::Plugin {
        crate::plugin().analyzer("container_probe", |run: &lovelycarve_plugin::Run| {
            let idx = crate::app_container_index(run);
            for (bundle, path) in idx {
                run.record_values(
                    "ios.app_config",
                    vec![
                        "container_probe".into(),
                        bundle.into(),
                        path.into(),
                        "probe".into(),
                    ],
                )
                .emit()?;
            }
            Ok(())
        })
    }
    let probe_harness = Harness::new(user_probe_plugin);
    let user_probe = probe_harness
        .run(
            "container_probe",
            Fixture::evidence(&[("part:0", &user_root)]),
        )
        .unwrap();
    let user_kvs = user_probe.of_type("ios.app_config");
    assert!(
        user_kvs.iter().any(|r| {
            r.text("key") == Some("com.example.userdump")
                && r.text("value")
                    == Some(&format!(
                        "part:0/User/Containers/Data/Application/{app_uuid}"
                    ))
        }),
        "missing User container: {user_kvs:?}"
    );
    assert!(
        user_kvs.iter().any(|r| {
            r.text("key") == Some("com.apple.Notes")
                && r.text("value") == Some("part:0/User/Library/Notes")
        }),
        "missing User/Library/Notes system path: {user_kvs:?}"
    );

    let bare_probe = probe_harness
        .run(
            "container_probe",
            Fixture::evidence(&[("part:0", &bare_root)]),
        )
        .unwrap();
    let bare_kvs = bare_probe.of_type("ios.app_config");
    assert!(
        bare_kvs.iter().any(|r| {
            r.text("key") == Some("com.example.baredump")
                && r.text("value")
                    == Some(&format!("part:0/Containers/Data/Application/{bare_uuid}"))
        }),
        "missing bare container: {bare_kvs:?}"
    );

    // Verify `ios_media` extracts `notes.sqlite` (`ZNOTE` + `ZNOTEBODY`).
    let media_run = Harness::new(crate::plugin)
        .run_with_inputs(
            "ios_media",
            Fixture::evidence(&[("part:0", &user_root)]),
            &["**/notes.sqlite"],
            Default::default(),
            Default::default(),
        )
        .unwrap();
    let notes = media_run.of_type("ios.note");
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].text("title"), Some("Secret Plan"));
    assert!(
        notes[0].sources.iter().any(|s| s
            .note
            .as_deref()
            .unwrap_or("")
            .contains("Meeting body detail")),
        "missing joined ZNOTEBODY detail: {:?}",
        notes[0].sources
    );
    assert_eq!(notes[1].text("title"), Some("[已删除] Deleted snippet"));
}
