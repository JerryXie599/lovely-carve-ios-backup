//! tracev3 系统日志解析集成测试（真实 sysdiagnose logarchive）。
//! `cargo test --test logarchive -- --ignored --nocapture`

use lovelycarve_plugin::testing::{Fixture, Harness};
use std::path::Path;

const SD: &str = r"E:\电子取证\精武杯\t9sV5pOBl1eLiM5ZPN8TEb\sysdiagnose_2026.09.23_14-23-51+0800_iPhone-OS_iPhone_19D50";

#[test]
#[ignore]
fn parses_tracev3_from_real_sysdiagnose() {
    let root = Path::new(SD);
    assert!(root.is_dir(), "sysdiagnose 目录不存在");
    let harness = Harness::new(ios_backup::plugin);
    let inputs = [
        "*/system_logs.logarchive/Persist/00000000000010f1.tracev3",
        "*/system_logs.logarchive/timesync/*.timesync",
    ];
    let run = harness
        .run_with_inputs("log_archive", Fixture::evidence(&[("root", root)]), &inputs, Default::default(), Default::default())
        .expect("run");
    println!("summary: {:?}", run.summary);
    let events = run.of_type("ios.log_event");
    println!("ios.log_event 条数: {}", events.len());
    for e in events.iter().take(10) {
        println!(
            "  {} {} pid={} {} | {}",
            e.text("time").unwrap_or(""),
            e.text("process").unwrap_or(""),
            e.int("pid").unwrap_or(0),
            e.text("log_type").unwrap_or(""),
            e.text("message").unwrap_or("").chars().take(90).collect::<String>()
        );
    }
    assert!(!events.is_empty(), "应解析出至少一条日志（Error/Fault 级）");
    // 时间应落在 2026-07（sysdiagnose 采集期内，Persist 是最近的日志）
    let t = events[0].text("time").unwrap_or("");
    assert!(t.starts_with("2026-"), "时间戳异常：{t}");
}
