//! 对真实检材目录跑 ios_backup 分析器的集成测试（testing feature 扮演宿主）。
//! `cargo test --test evidence -- --ignored --nocapture` 手动运行。

use lovelycarve_plugin::testing::{Fixture, Harness};
use std::path::Path;

const EVIDENCE: &str = r"E:\电子取证\精武杯\t9sV5pOBl1eLiM5ZPN8TEb";

#[test]
#[ignore]
fn analyzes_real_evidence() {
    let root = Path::new(EVIDENCE);
    assert!(root.is_dir(), "检材目录不存在：{EVIDENCE}");
    let harness = Harness::new(ios_backup::plugin);
    let fixture = Fixture::evidence(&[("root", root)]);
    // 宿主按清单 inputs 模式在视图内匹配；这里传种子模式让 harness 展开
    let inputs = [
        "*/Manifest.plist",
        "*/**/bluetooth_status.txt",
        "*/**/BDC_Once*.csv",
        "*/**/BatteryHealth.log",
        "*/**/mobile_installation.log*",
        "*/**/com.apple.MobileBackup.plist",
        "*/**/com.apple.powerd.bdc.plist",
        "*/**/SystemVersion.plist",
        "*/**/*.PLSQL",
    ];
    let run = harness
        .run_with_inputs("ios_backup", fixture, &inputs, Default::default(), Default::default())
        .expect("run");
    println!("summary: {:?}", run.summary);
    let counts = run.counts();
    let mut total = 0;
    for (t, n) in &counts {
        println!("  {t}: {n}");
        total += n;
    }
    println!("共 {total} 条记录");

    // 核心断言：做题时人工验证过的结论必须被插件自动产出
    let findings = run.of_type("ios.finding");
    let text_of = |item: &str| -> String {
        findings
            .iter()
            .filter(|r| r.text("item") == Some(item))
            .map(|r| r.text("finding").unwrap_or("").to_string())
            .next()
            .unwrap_or_default()
    };
    assert_eq!(text_of("设备序列号"), "FD2W71HVJCM0", "序列号");
    let bt = text_of("备份时间（UTC+8）");
    assert!(bt.starts_with("2026-09-23 15:02:50"), "备份时间 UTC+8: {bt}");
    assert_eq!(text_of("蓝牙 MAC 地址"), "C0:A5:3E:82:06:E0", "蓝牙MAC");
    assert_eq!(text_of("电池激活时间（UTC+8）"), "2022-01-23 15:02:27", "电池激活");
    let charge = text_of("最近一次充至 100%（UTC+8）");
    assert!(charge.contains("1783544086.85"), "充满原始时间戳：{charge}");
    let unlock = text_of("首次解锁屏幕（UTC+8）");
    assert!(unlock.contains("2026-07-09 02:15:16"), "首次解锁：{unlock}");
    assert!(unlock.contains("com.apple.mobilephone"), "解锁触发应用：{unlock}");
    let bat = text_of("首次安装请求：蝙蝠-安全聊天软件（com.zsim.bzychat）");
    assert!(bat.starts_with("2026-08-12 10:07:21"), "蝙蝠首次安装：{bat}");
}
