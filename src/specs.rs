//! 记录结构定义（iOS 备份取证的通用字段命名）。

use lovelycarve_plugin::schema::{FieldKind as K, FieldSchema, PropertyLayout, RecordSchema};

pub struct Spec {
    pub id: &'static str,
    pub label: &'static str,
    pub fields: Vec<(&'static str, &'static str, K, bool)>, // (id, label, kind, key)
    pub properties: Option<(&'static str, Option<&'static str>, Option<&'static str>)>, // (value, label, secondary)
    pub category: &'static str,
}

fn s(id: &'static str, label: &'static str, fields: Vec<(&'static str, &'static str, K, bool)>) -> Spec {
    Spec { id, label, fields, properties: None, category: "baseinfo" }
}

fn sc(id: &'static str, label: &'static str, category: &'static str, fields: Vec<(&'static str, &'static str, K, bool)>) -> Spec {
    Spec { id, label, fields, properties: None, category }
}

/// 全部记录结构。时间字段 kind=Timestamp 且带偏移时自动投影时间线（timeline 角色由
/// LovelyCarve 按 timestamp 字段处理；无偏移的 local 值只排序展示）。
pub fn specs() -> Vec<Spec> {
    vec![
        Spec {
            id: "backup",
            label: "备份信息",
            fields: vec![
                ("device_name", "设备名称（DeviceName）", K::Text, false),
                ("display_name", "显示名称（DisplayName）", K::Text, false),
                ("product_name", "系统名称（ProductName）", K::Text, false),
                ("product_type", "系统类型（ProductType）", K::Text, false),
                ("model", "营销型号", K::Text, false),
                ("product_version", "系统版本（ProductVersion）", K::Text, false),
                ("build_version", "构建版本（BuildVersion）", K::Text, false),
                ("serial_number", "序列号（SerialNumber）", K::Text, true),
                ("udid", "UDID", K::Text, false),
                ("imei", "IMEI", K::Text, false),
                ("iccid", "ICCID", K::Text, false),
                ("phone_number", "手机号码（PhoneNumber）", K::Text, false),
                ("backup_time", "备份时间（UTC+8，BackupTime）", K::Timestamp, false),
                ("backup_time_utc", "备份时间（UTC 原值）", K::Timestamp, false),
                ("backup_time_raw", "备份时间原始值", K::Text, false),
                ("version", "备份格式版本", K::Text, false),
                ("system_domains_version", "系统域版本", K::Text, false),
                ("encrypt", "是否加密", K::Bool, false),
                ("was_passcode_set", "是否设置锁屏密码（WasPasscodeSet）", K::Bool, false),
                ("app_count", "备份内应用数", K::Int, false),
                ("path", "备份路径", K::Path, false),
            ],
            properties: None,
            category: "baseinfo",
        },
        Spec {
            id: "device_info",
            label: "设备属性",
            fields: vec![
                ("key", "键名（Key）", K::Text, true),
                ("name", "名称（Name）", K::Text, false),
                ("value", "内容（Value）", K::Text, false),
                ("source", "来源", K::Text, false),
            ],
            properties: None,
            category: "baseinfo",
        },
        Spec {
            id: "keybag",
            label: "备份密钥包（BackupKeyBag）",
            fields: vec![
                ("key", "参数", K::Text, true),
                ("value", "值", K::Text, false),
                ("note", "说明", K::Text, false),
            ],
            properties: Some(("value", Some("key"), Some("note"))),
            category: "baseinfo",
        },
        Spec {
            id: "app",
            label: "应用清单",
            fields: vec![
                ("bundle", "包名", K::Text, true),
                ("app_name", "应用名（AppName）", K::Text, false),
                ("version_name", "版本名称（VersionName）", K::Text, false),
                ("system_flag", "应用类型（SystemFlag）", K::Text, false),
                ("placeholder", "占位标识", K::Bool, false),
                ("domain_size", "备份数据体积", K::Size, false),
                ("domain", "备份域", K::Text, false),
                ("install_time", "首次安装请求时间（InstallTime）", K::Timestamp, false),
                ("source", "来源", K::Text, false),
            ],
            properties: None,
            category: "appinfo",
        },
        Spec {
            id: "install_event",
            label: "安装卸载事件",
            fields: vec![
                ("time", "时间（设备本地=UTC+8）", K::Timestamp, false),
                ("action", "动作", K::Text, false),
                ("bundle", "包名", K::Text, true),
                ("app_name", "应用显示名", K::Text, false),
                ("version", "版本", K::Text, false),
                ("detail", "事件明细", K::Text, false),
                ("requested_by", "发起进程", K::Text, false),
            ],
            properties: None,
            category: "appinfo",
        },
        Spec {
            id: "bluetooth_state",
            label: "本机蓝牙状态",
            fields: vec![
                ("item", "事项", K::Text, true),
                ("value", "值", K::Text, false),
                ("note", "说明", K::Text, false),
            ],
            properties: Some(("value", Some("item"), Some("note"))),
            category: "connect",
        },
        Spec {
            id: "battery",
            label: "电池信息",
            fields: vec![
                ("item", "事项", K::Text, true),
                ("value", "值", K::Text, false),
                ("note", "依据与来源", K::Text, false),
            ],
            properties: Some(("value", Some("item"), Some("note"))),
            category: "power",
        },
        Spec {
            id: "charge_full",
            label: "充至100%记录",
            fields: vec![
                ("session", "充电会话", K::Int, true),
                ("raw_ts", "充至100%原始时间戳", K::Text, false),
                ("reached", "充至100%（UTC+8）", K::Timestamp, false),
                ("fully_charged", "FullyCharged=1 首条（UTC+8）", K::Timestamp, false),
                ("last_seen", "最后一条100%记录（UTC+8）", K::Timestamp, false),
                ("samples", "100%采样数", K::Int, false),
                ("charge_start", "会话前充电起点（UTC+8）", K::Timestamp, false),
                ("detail", "口径说明", K::Text, false),
            ],
            properties: None,
            category: "power",
        },
        Spec {
            id: "unlock",
            label: "解锁锁定事件",
            fields: vec![
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("raw_ts", "原始时间戳", K::Text, false),
                ("action", "动作", K::Text, true),
                ("trigger_app", "触发/首个前台应用", K::Text, false),
                ("screen_chain", "解锁后屏幕状态链", K::Text, false),
                ("prev_locked_span", "此前锁定持续（秒，相邻观测）", K::Duration, false),
            ],
            properties: None,
            category: "power",
        },
        Spec {
            id: "lock_interval",
            label: "锁定状态区间",
            fields: vec![
                ("state", "状态", K::Text, true),
                ("start", "开始观测（UTC+8）", K::Timestamp, false),
                ("end", "结束观测（UTC+8）", K::Timestamp, false),
                ("seconds", "推定秒数", K::Duration, false),
                ("points", "同状态观测数", K::Int, false),
            ],
            properties: None,
            category: "power",
        },
        Spec {
            id: "screen_interval",
            label: "亮屏息屏区间",
            fields: vec![
                ("state", "状态", K::Text, true),
                ("start", "开始观测（UTC+8）", K::Timestamp, false),
                ("end", "结束观测（UTC+8）", K::Timestamp, false),
                ("seconds", "推定秒数", K::Duration, false),
                ("points", "同状态观测数", K::Int, false),
            ],
            properties: None,
            category: "power",
        },
        Spec {
            id: "app_usage",
            label: "应用前后台时长",
            fields: vec![
                ("bundle", "包名", K::Text, true),
                ("name", "应用名", K::Text, false),
                ("foreground", "前台秒", K::Duration, false),
                ("background", "后台秒", K::Duration, false),
                ("samples", "汇总记录数", K::Int, false),
                ("first", "最早（UTC+8）", K::Timestamp, false),
                ("last", "最晚（UTC+8）", K::Timestamp, false),
            ],
            properties: None,
            category: "appinfo",
        },
        Spec {
            id: "battery_daily",
            label: "电池每日采样",
            fields: vec![
                ("day", "日期（UTC+8）", K::Text, true),
                ("samples", "采样数", K::Int, false),
                ("level_min", "最低电量%", K::Float, false),
                ("level_max", "最高电量%", K::Float, false),
                ("temp_min", "最低温度°C", K::Float, false),
                ("temp_max", "最高温度°C", K::Float, false),
                ("cycle_latest", "最后采样循环数", K::Float, false),
            ],
            properties: None,
            category: "power",
        },
        Spec {
            id: "setting",
            label: "系统设置摘要",
            fields: vec![
                ("key", "键", K::Text, true),
                ("value", "值", K::Text, false),
                ("first", "首次观测（UTC+8）", K::Timestamp, false),
                ("last", "末次观测（UTC+8）", K::Timestamp, false),
                ("observations", "观测数", K::Int, false),
            ],
            properties: None,
            category: "baseinfo",
        },
        Spec {
            id: "finding",
            label: "取证发现",
            fields: vec![
                ("item", "事项", K::Text, true),
                ("finding", "结论", K::Text, false),
                ("meaning", "依据、来源与口径", K::Text, false),
            ],
            properties: Some(("finding", Some("item"), Some("meaning"))),
            category: "baseinfo",
        },

        sc(
            "sms",
            "短信",
            "comm",
            vec![
                ("phone", "号码", K::Text, true),
                ("content", "内容", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("direction", "方向", K::Text, false),
                ("is_read", "已读", K::Text, false),
                ("service", "服务", K::Text, false),
                ("attachment", "附件", K::Path, false),
            ],
        ),
        sc(
            "calllog",
            "通话记录",
            "comm",
            vec![
                ("phone", "号码", K::Text, true),
                ("name", "联系人", K::Text, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("kind", "类型", K::Text, false),
            ],
        ),
        sc(
            "contact",
            "通讯录",
            "comm",
            vec![
                ("name", "姓名", K::Text, true),
                ("phones", "电话", K::Text, false),
                ("emails", "邮箱", K::Text, false),
                ("org", "组织", K::Text, false),
                ("note", "备注", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("modified", "修改时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "wifi",
            "WiFi网络",
            "connect",
            vec![
                ("ssid", "SSID", K::Text, true),
                ("password", "密码", K::Text, false),
                ("security", "加密类型", K::Text, false),
                ("auto_join", "自动加入", K::Text, false),
                ("last_joined", "最后连接（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "bt_device",
            "蓝牙设备",
            "connect",
            vec![
                ("name", "名称", K::Text, true),
                ("mac", "MAC地址", K::Text, false),
                ("last_seen", "最后发现（UTC+8）", K::Timestamp, false),
                ("last_conn", "最后连接（UTC+8）", K::Timestamp, false),
                ("state", "配对状态", K::Text, false),
            ],
        ),
        sc(
            "account",
            "账户",
            "connect",
            vec![
                ("username", "用户名", K::Text, true),
                ("account_type", "账户类型", K::Text, false),
                ("description", "描述", K::Text, false),
                ("date", "时间（UTC+8）", K::Timestamp, false),
                ("bundle", "归属应用", K::Text, false),
            ],
        ),
        sc(
            "photo",
            "相册资产",
            "media",
            vec![
                ("filename", "文件名", K::Text, true),
                ("media_type", "媒体类型", K::Text, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("created", "拍摄时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "note",
            "笔记",
            "media",
            vec![
                ("title", "标题", K::Text, true),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("edited", "修改时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "log_event",
            "系统日志（Unified Log）",
            "log",
            vec![
                ("time", "时间（UTC）", K::Timestamp, false),
                ("process", "进程", K::Text, true),
                ("pid", "PID", K::Int, false),
                ("thread", "线程", K::Int, false),
                ("subsystem", "子系统", K::Text, false),
                ("category", "分类", K::Text, false),
                ("event_type", "事件类型", K::Text, false),
                ("log_type", "日志级别", K::Text, false),
                ("message", "消息", K::Text, false),
            ],
        ),
        sc(
            "push",
            "通知记录",
            "comm",
            vec![
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("app", "应用", K::Text, true),
                ("title", "标题", K::Text, false),
                ("subtitle", "副标题", K::Text, false),
                ("body", "内容", K::Text, false),
                ("bundle", "BundleID", K::Text, false),
            ],
        ),
        sc(
            "keychain_item",
            "钥匙串条目",
            "connect",
            vec![
                ("account", "账户", K::Text, true),
                ("service", "服务", K::Text, false),
                ("item_class", "条目类别", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("modified", "修改时间（UTC+8）", K::Timestamp, false),
                ("data_len", "数据长度", K::Int, false),
                ("note", "说明", K::Text, false),
            ],
        ),
        sc(
            "web_history",
            "浏览器历史",
            "appinfo",
            vec![
                ("time", "访问时间（UTC+8）", K::Timestamp, false),
                ("url", "网址", K::Text, true),
                ("title", "标题", K::Text, false),
                ("visit_count", "访问次数", K::Int, false),
                ("browser", "浏览器", K::Text, false),
            ],
        ),
        sc(
            "app_msg",
            "应用消息",
            "comm",
            vec![
                ("app", "应用", K::Text, false),
                ("chat", "会话/群", K::Text, false),
                ("sender", "发送者", K::Text, false),
                ("content", "内容", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("direction", "方向", K::Text, false),
            ],
        ),
        sc(
            "app_user",
            "应用账户",
            "appinfo",
            vec![
                ("app", "应用", K::Text, false),
                ("user_id", "用户ID", K::Text, true),
                ("nickname", "昵称", K::Text, false),
                ("phone", "手机号", K::Text, false),
                ("email", "邮箱", K::Text, false),
                ("gender", "性别", K::Text, false),
                ("extra", "其他", K::Text, false),
            ],
        ),
        sc(
            "app_search",
            "应用搜索历史",
            "appinfo",
            vec![
                ("app", "应用", K::Text, false),
                ("keyword", "关键词", K::Text, true),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_geo",
            "应用位置",
            "appinfo",
            vec![
                ("app", "应用", K::Text, false),
                ("name", "地点名称", K::Text, false),
                ("lng", "经度", K::Float, false),
                ("lat", "纬度", K::Float, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_bill",
            "应用订单",
            "appinfo",
            vec![
                ("app", "应用", K::Text, false),
                ("order_id", "订单号", K::Text, true),
                ("title", "商品/描述", K::Text, false),
                ("amount", "金额", K::Float, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_media",
            "应用媒体",
            "media",
            vec![
                ("app", "应用", K::Text, false),
                ("name", "名称", K::Text, true),
                ("path", "路径", K::Path, false),
                ("size", "大小", K::Size, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        s(
            "overview",
            "取证结论总览",
            vec![
                ("item", "事项", K::Text, true),
                ("finding", "结论", K::Text, false),
                ("meaning", "依据与限制", K::Text, false),
            ],
        ),
        // powerlog files 分析器输出（沿用 powerlog. 前缀，兼容既有阅读习惯）
        s(
            "powerlog_overview",
            "Powerlog取证结论总览",
            vec![
                ("item", "事项", K::Text, true),
                ("finding", "结论", K::Text, false),
                ("meaning", "依据与限制", K::Text, false),
            ],
        ),
    ]
}

pub fn schemas() -> Vec<RecordSchema> {
    specs()
        .into_iter()
        .map(|s| {
            let full = if s.id.starts_with("powerlog_") {
                format!("powerlog.{}", s.id.trim_start_matches("powerlog_"))
            } else {
                format!("ios.{}", s.id)
            };
            let mut r = RecordSchema::new(full, s.label);
            for (id, label, k, key) in s.fields {
                let f = FieldSchema::new(id, label, k);
                r = r.field(if key { f.key() } else { f });
            }
            r = r.category(s.category);
            if let Some((value, label, secondary)) = s.properties {
                let mut layout = PropertyLayout::new(value);
                if let Some(l) = label {
                    layout = layout.label(l);
                }
                if let Some(sec) = secondary {
                    layout = layout.secondary(sec);
                }
                r = r.properties(layout);
            }
            r
        })
        .collect()
}

pub fn type_id(spec_id: &str) -> String {
    if spec_id.starts_with("powerlog_") {
        format!("powerlog.{}", spec_id.trim_start_matches("powerlog_"))
    } else {
        format!("ios.{spec_id}")
    }
}
