//! 备份数据库 fixture 集成测试：验证 sms/calllog/contact/wifi/bt/account 解析。
//! `cargo test --test backup_db -- --nocapture`

use lovelycarve_plugin::testing::{Fixture, Harness};
use std::path::Path;

const FIXTURE: &str = "tests/fixtures/ios_backup";

#[test]
fn parses_backup_databases() {
    let root = Path::new(FIXTURE);
    assert!(root.is_dir(), "fixture 缺失，先运行 tests/gen_fixture.py");
    let harness = Harness::new(ios_backup::plugin);
    let inputs = [
        "*/sms.db",
        "*/AddressBook.sqlitedb",
        "*/CallHistory.storedata",
        "*/com.apple.wifi.plist",
        "*/com.apple.MobileBluetooth.ledevices.paired.db",
        "*/com.apple.MobileBluetooth.ledevices.other.db",
        "*/Accounts3.sqlite",
        "*/PrivateMessage.db",
        "*/_message_zhihu_im.db",
        "*/BBIMDB*",
        "*/favCoreDataDB.sqlite",
        "*/NTally.sqlite",
        "*/download.sqlite",
        "*/LoginDB.sqlite",
        "*/oftenArrived.db",
        "*/search_history.db",
        "*/imeituan_database.sqlite",
        "*/wallet-rn-v2.sqlite",
        "*/KSIMDBUserInfo.sqlite",
        "*/conversation.sqlite",
        "*/ChatStorage.sqlite",
        "*/amap.db",
        "*/calls.db",
    ];
    let run = harness
        .run_with_inputs("ios_backup", Fixture::evidence(&[("root", root)]), &inputs, Default::default(), Default::default())
        .expect("run");

    // 短信
    let sms = run.of_type("ios.sms");
    assert_eq!(sms.len(), 3, "短信条数");
    let first = sms.iter().find(|r| r.text("content").unwrap_or("").contains("明天见")).expect("含『明天见』");
    assert_eq!(first.text("phone"), Some("+8613800138000"));
    assert_eq!(first.text("direction"), Some("发送"));
    assert!(first.text("time").unwrap_or("").starts_with("2026-07-01T10:00"), "时间：{:?}", first.text("time"));
    let with_att = sms.iter().find(|r| r.text("content").unwrap_or("").contains("发票")).expect("附件短信");
    assert!(with_att.text("attachment").unwrap_or("").contains("IMG_0001.JPG"));

    // 通话
    let calls = run.of_type("ios.calllog");
    assert!(calls.len() >= 3, "通话应>=3，实得{}", calls.len());
    let miss = calls.iter().find(|r| r.text("kind") == Some("未接")).expect("未接来电");
    assert_eq!(miss.text("phone"), Some("+8613900139000"));

    // 通讯录
    let contacts = run.of_type("ios.contact");
    assert_eq!(contacts.len(), 2);
    let zhang = contacts.iter().find(|r| r.text("name") == Some("张三")).expect("张三");
    assert!(zhang.text("phones").unwrap_or("").contains("+8613800138000"));
    assert!(zhang.text("emails").unwrap_or("").contains("zhang@ex.com"));
    assert_eq!(zhang.text("org"), Some("某公司"));

    // WiFi
    let wifis = run.of_type("ios.wifi");
    assert_eq!(wifis.len(), 2, "WiFi 网络");
    let home = wifis.iter().find(|r| r.text("ssid") == Some("HomeWiFi")).expect("HomeWiFi");
    assert_eq!(home.text("auto_join"), Some("是"));

    // 蓝牙
    let bts = run.of_type("ios.bt_device");
    assert_eq!(bts.len(), 2);
    let tesla = bts.iter().find(|r| r.text("name").unwrap_or("").contains("Model3")).expect("特斯拉");
    assert_eq!(tesla.text("mac"), Some("AA:BB:CC:DD:EE:FF"));
    assert_eq!(tesla.text("state"), Some("已配对"));

    // 应用消息（小红书/知乎/B站）
    let app_msgs = run.of_type("ios.app_msg");
    assert!(app_msgs.len() >= 4, "应用消息应>=4条，实得{}", app_msgs.len());
    let xhs = app_msgs.iter().find(|r| r.text("app") == Some("小红书")).expect("小红书消息");
    assert!(xhs.text("content").unwrap_or("").contains("这个链接不错"), "小红书正文");
    assert_eq!(xhs.text("sender"), Some("u100"));
    let bili = app_msgs.iter().find(|r| r.text("app") == Some("哔哩哔哩")).expect("B站消息");
    assert_eq!(bili.text("content"), Some("投个币"));
    assert_eq!(bili.text("sender"), Some("uid-77"));

    // 账户
    let accounts = run.of_type("ios.account");
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().any(|r| r.text("username") == Some("test@icloud.com")));
    assert!(accounts.iter().any(|r| r.text("username") == Some("12345@qq.com")));

    // App geo/bill/media/user
    let geos = run.of_type("ios.app_geo");
    assert!(geos.iter().any(|r| r.text("app") == Some("百度地图收藏") && r.text("name") == Some("公司附近咖啡")), "geo");
    let bills = run.of_type("ios.app_bill");
    assert!(bills.iter().any(|r| r.text("app") == Some("鲨鱼记账") && (r.text("title") == Some("午餐"))), "bill");
    let medias = run.of_type("ios.app_media");
    assert!(medias.iter().any(|r| r.text("app") == Some("优酷下载") && r.text("name") == Some("纪录片EP1")), "media");
    let users = run.of_type("ios.app_user");
    assert!(users.iter().any(|r| r.text("app") == Some("Blued登录") && r.text("phone") == Some("+8613800138000")), "user");
    let amsg = run.of_type("ios.app_geo");
    assert!(amsg.iter().any(|r| r.text("app") == Some("高德常去地点") && r.text("name") == Some("公司大楼")), "geo oftenArrived");
    assert!(amsg.iter().any(|r| r.text("app") == Some("地图搜索历史") && r.text("name") == Some("北京南站")), "geo search_history JSON");
    let bills2 = run.of_type("ios.app_bill");
    assert!(bills2.iter().any(|r| r.text("app") == Some("美团订单") && (r.text("title").unwrap_or("").contains("外卖") || r.text("order_id") == Some("meituan-001"))), "美团订单");
    let users2 = run.of_type("ios.app_user");
    assert!(users2.iter().any(|r| r.text("app") == Some("加密货币钱包") && r.text("nickname") == Some("链上用户")), "钱包用户");
    let users3 = run.of_type("ios.app_user");
    assert!(users3.iter().any(|r| r.text("app") == Some("快手账户") && r.text("nickname") == Some("快手用户A")), "快手用户");
    let immsgs = run.of_type("ios.app_msg");
    assert!(immsgs.iter().any(|r| r.text("app") == Some("抖音/TIM会话") && r.text("content").unwrap_or("").contains("抖音IM消息正文")), "抖音IM");
    let wa = immsgs.iter().find(|r| r.text("app") == Some("WhatsApp聊天")).expect("WhatsApp消息");
    assert!(wa.text("content").unwrap_or("").contains("WhatsApp测试消息"), "WA正文");
    assert!(amsg.iter().any(|r| r.text("app") == Some("高德地图主库") && r.text("name") == Some("西单商场")), "amap glued");
    let wacalls = run.of_type("ios.calllog");
    assert!(wacalls.iter().any(|r| r.text("name") == Some("高德地图主库") || r.text("phone") == Some("+8613700137000")), "calls.db");
    println!("backup_db 全部断言通过（含 glued 系）");
}
