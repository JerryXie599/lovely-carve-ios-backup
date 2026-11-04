//! 备份数据库 fixture 测试（按分组分析器分别验证）。
//! `cargo test --test backup_db -- --nocapture`

use lovelycarve_plugin::testing::{Fixture, Harness};
use std::path::Path;

const FIXTURE: &str = "tests/fixtures/ios_backup";

fn run(analyzer: &str, inputs: &[&str]) -> lovelycarve_plugin::testing::TestRun {
    let harness = Harness::new(ios_backup::plugin);
    let fixture = Fixture::evidence(&[("root", Path::new(FIXTURE))]);
    harness
        .run_with_inputs(analyzer, fixture, inputs, Default::default(), Default::default())
        .expect("run")
}

#[test]
fn parses_backup_databases() {
    let root = Path::new(FIXTURE);
    assert!(root.is_dir(), "fixture 缺失，先运行 tests/gen_fixture.py");

    // ---- 通讯信息 ----
    let comm = run("ios_comm", &[
        "*/sms.db", "*/AddressBook.sqlitedb", "*/CallHistory.storedata",
        "*/Message.sqlite", "*/_message_zhihu_im.db", "*/BBIMDB233266909.db",
        "*/history_message.db", "*/PrivateMessage.db", "*/ChatStorage.sqlite",
    ]);
    let sms = comm.of_type("ios.sms");
    assert_eq!(sms.len(), 3, "短信条数");
    let first = sms.iter().find(|r| r.text("content").unwrap_or("").contains("明天见")).expect("『明天见』");
    assert_eq!(first.text("phone"), Some("+8613800138000"));
    assert_eq!(first.text("direction"), Some("发送"));
    assert!(first.text("time").unwrap_or("").starts_with("2026-07-01T10:00"));
    let with_att = sms.iter().find(|r| r.text("content").unwrap_or("").contains("发票")).expect("附件短信");
    assert!(with_att.text("attachment").unwrap_or("").contains("IMG_0001.JPG"));

    let calls = comm.of_type("ios.calllog");
    assert!(calls.len() >= 3, "通话应>=3，实得{}", calls.len());
    let miss = calls.iter().find(|r| r.text("kind") == Some("未接")).expect("未接来电");
    assert_eq!(miss.text("phone"), Some("+8613900139000"));

    let contacts = comm.of_type("ios.contact");
    assert_eq!(contacts.len(), 2);
    let zhang = contacts.iter().find(|r| r.text("name") == Some("张三")).expect("张三");
    assert!(zhang.text("phones").unwrap_or("").contains("+8613800138000"));
    assert!(zhang.text("emails").unwrap_or("").contains("zhang@ex.com"));

    let msgs = comm.of_type("ios.app_msg");
    assert!(msgs.iter().any(|r| r.text("app") == Some("小红书") && r.text("content").unwrap_or("").contains("这个链接不错")), "小红书");
    assert!(msgs.iter().any(|r| r.text("app") == Some("知乎") && r.text("content") == Some("知乎第一条消息")), "知乎");
    assert!(msgs.iter().any(|r| r.text("app") == Some("哔哩哔哩") && r.text("content") == Some("投个币")), "B站");
    assert!(msgs.iter().any(|r| r.text("app") == Some("WhatsApp聊天") && r.text("content").unwrap_or("").contains("WhatsApp测试消息")), "WhatsApp");
    assert!(msgs.iter().any(|r| r.text("app") == Some("KakaoTalk")), "Kakao");
    assert!(msgs.iter().any(|r| r.text("app") == Some("国际IM") && r.text("content") == Some("国际IM历史消息正文")), "history_message");

    // ---- 设备连接 ----
    let connect = run("ios_connect", &[
        "*/com.apple.wifi.plist", "*/com.apple.MobileBluetooth.ledevices.paired.db",
        "*/com.apple.MobileBluetooth.ledevices.other.db", "*/Accounts3.sqlite", "*/LoginDB.sqlite",
    ]);
    let wifis = connect.of_type("ios.wifi");
    assert_eq!(wifis.len(), 2);
    assert!(wifis.iter().any(|r| r.text("ssid") == Some("HomeWiFi") && r.text("auto_join") == Some("是")));
    let bts = connect.of_type("ios.bt_device");
    assert_eq!(bts.len(), 2);
    let tesla = bts.iter().find(|r| r.text("name").unwrap_or("").contains("Model3")).expect("特斯拉");
    assert_eq!(tesla.text("mac"), Some("AA:BB:CC:DD:EE:FF"));
    assert_eq!(tesla.text("state"), Some("已配对"));
    let accounts = connect.of_type("ios.account");
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().any(|r| r.text("username") == Some("test@icloud.com")));

    // ---- 应用信息 ----
    let users = connect.of_type("ios.app_user");
    assert!(users.iter().any(|r| r.text("app") == Some("Blued登录") && r.text("phone") == Some("+8613800138000")), "Blued用户");

    println!("backup_db 全部断言通过（按分组分析器验证）");
}
