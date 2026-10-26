//! 离线 CLI：不经 LovelyCarve 宿主，用 SDK testing Harness 直接对本地目录跑分析。
//! 用法：ios-backup-cli <检材目录> [ios_backup|powerlog_files]

fn main() {
    #[cfg(feature = "testing")]
    {
        let args: Vec<String> = std::env::args().collect();
        let dir = args.get(1).expect("用法: ios-backup-cli <检材目录> [analyzer]");
        let analyzer = args.get(2).map(|s| s.as_str()).unwrap_or("ios_backup");
        let root = std::path::Path::new(dir);
        let harness = lovelycarve_plugin::testing::Harness::new(lovelycarve_ios_backup::plugin);
        let fixture = lovelycarve_plugin::testing::Fixture::evidence(&[("root", root)]);
        let inputs: Vec<String> = std::fs::read_dir(root)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        // evidence scope：宿主按清单 inputs 模式匹配；CLI 直接传根下文件名做种子，
        // harness 会按模式在视图里展开（见 sdk testing.rs prepare）。
        let input_refs: Vec<&str> = inputs.iter().map(|s| s.as_str()).collect();
        let run = harness
            .run_with_inputs(analyzer, fixture, &input_refs, Default::default(), Default::default())
            .expect("run");
        let counts = run.counts();
        let mut total = 0usize;
        println!("== {} 记录统计 ==", analyzer);
        for (t, n) in &counts {
            println!("  {t}: {n}");
            total += n;
        }
        println!("共 {total} 条；summary: {:?}", run.summary);
    }
    #[cfg(not(feature = "testing"))]
    {
        println!("需要 --features lovelycarve-plugin/testing 构建（CLI 仅开发用）");
    }
}
