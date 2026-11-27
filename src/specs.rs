//! 记录结构定义（iOS 备份取证的通用字段命名）。

#[cfg(feature = "mod-chat")]
use lovelycarve_plugin::schema::ChatLayout;
use lovelycarve_plugin::schema::{FieldKind as K, FieldSchema, PropertyLayout, RecordSchema};

pub struct Spec {
    pub id: &'static str,
    pub label: &'static str,
    pub fields: Vec<(&'static str, &'static str, K, bool)>, // (id, label, kind, key)
    pub properties: Option<(&'static str, Option<&'static str>, Option<&'static str>)>, // (value, label, secondary)
    pub category: &'static str,
}

fn s(
    id: &'static str,
    label: &'static str,
    fields: Vec<(&'static str, &'static str, K, bool)>,
) -> Spec {
    Spec {
        id,
        label,
        fields,
        properties: None,
        category: "abasic",
    }
}

fn sc(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    fields: Vec<(&'static str, &'static str, K, bool)>,
) -> Spec {
    Spec {
        id,
        label,
        fields,
        properties: None,
        category,
    }
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
                (
                    "product_version",
                    "系统版本（ProductVersion）",
                    K::Text,
                    false,
                ),
                ("build_version", "构建版本（BuildVersion）", K::Text, false),
                ("serial_number", "序列号（SerialNumber）", K::Text, true),
                ("udid", "UDID", K::Text, false),
                ("imei", "IMEI", K::Text, false),
                ("iccid", "ICCID", K::Text, false),
                ("phone_number", "手机号码（PhoneNumber）", K::Text, false),
                (
                    "backup_time",
                    "备份时间（UTC+8，BackupTime）",
                    K::Timestamp,
                    false,
                ),
                (
                    "backup_time_utc",
                    "备份时间（UTC 原值）",
                    K::Timestamp,
                    false,
                ),
                ("backup_time_raw", "备份时间原始值", K::Text, false),
                ("version", "备份格式版本", K::Text, false),
                ("system_domains_version", "系统域版本", K::Text, false),
                ("encrypt", "是否加密", K::Bool, false),
                (
                    "was_passcode_set",
                    "是否设置锁屏密码（WasPasscodeSet）",
                    K::Bool,
                    false,
                ),
                ("app_count", "备份内应用数", K::Int, false),
                ("path", "备份路径", K::Path, false),
            ],
            properties: None,
            category: "abasic",
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
            category: "abasic",
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
            category: "abasic",
        },
        Spec {
            id: "app",
            label: "应用清单",
            fields: vec![
                ("bundle", "包名", K::Text, true),
                ("app_name", "显示名（桌面/导入记录）", K::Text, false),
                ("store_name", "存储名（App Store）", K::Text, false),
                ("version_name", "版本名称（VersionName）", K::Text, false),
                (
                    "bundle_version",
                    "构建号（CFBundleVersion）",
                    K::Text,
                    false,
                ),
                ("system_flag", "应用类型（SystemFlag）", K::Text, false),
                ("placeholder", "占位标识", K::Bool, false),
                ("domain_size", "备份数据体积", K::Size, false),
                ("domain", "备份域", K::Text, false),
                ("data_path", "数据目录（检材内路径）", K::Path, false),
                (
                    "install_time",
                    "首次安装请求时间（InstallTime）",
                    K::Timestamp,
                    false,
                ),
                ("source", "来源", K::Text, false),
            ],
            properties: None,
            category: "bappinfo",
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
            category: "bappinfo",
        },
        Spec {
            id: "data_usage",
            label: "网络流量与进程使用",
            fields: vec![
                ("app_name", "应用名称", K::Text, true),
                ("bundle", "应用包名", K::Text, false),
                ("process", "进程名称", K::Text, false),
                ("start_time", "开始时间", K::Timestamp, false),
                ("time", "记录时间", K::Timestamp, false),
                ("wifi_in", "WiFi下载流量", K::Size, false),
                ("wifi_out", "WiFi上传流量", K::Size, false),
                ("wwan_in", "数据下载流量", K::Size, false),
                ("wwan_out", "数据上传流量", K::Size, false),
                ("kind", "类别编号", K::Int, false),
            ],
            properties: None,
            category: "bappinfo",
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
            category: "dconnect",
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
            category: "epower",
        },
        Spec {
            id: "charge_full",
            label: "充至100%记录",
            fields: vec![
                ("session", "充电会话", K::Int, true),
                ("raw_ts", "充至100%原始时间戳", K::Text, false),
                ("reached", "充至100%（UTC+8）", K::Timestamp, false),
                (
                    "fully_charged",
                    "FullyCharged=1 首条（UTC+8）",
                    K::Timestamp,
                    false,
                ),
                (
                    "last_seen",
                    "最后一条100%记录（UTC+8）",
                    K::Timestamp,
                    false,
                ),
                ("samples", "100%采样数", K::Int, false),
                (
                    "charge_start",
                    "会话前充电起点（UTC+8）",
                    K::Timestamp,
                    false,
                ),
                ("detail", "口径说明", K::Text, false),
            ],
            properties: None,
            category: "epower",
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
                (
                    "prev_locked_span",
                    "此前锁定持续（秒，相邻观测）",
                    K::Duration,
                    false,
                ),
            ],
            properties: None,
            category: "epower",
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
            category: "epower",
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
            category: "epower",
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
            category: "bappinfo",
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
            category: "epower",
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
            category: "abasic",
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
            category: "abasic",
        },
        sc(
            "sms",
            "短信",
            "ccomm",
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
            "ccomm",
            vec![
                ("phone", "号码", K::Text, true),
                ("name", "联系人", K::Text, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("kind", "类型", K::Text, false),
                ("app", "来源应用", K::Text, false),
            ],
        ),
        sc(
            "contact",
            "通讯录",
            "ccomm",
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
            "dconnect",
            vec![
                ("ssid", "SSID", K::Text, true),
                ("password", "密码", K::Text, false),
                ("security", "加密类型", K::Text, false),
                ("auto_join", "自动加入", K::Text, false),
                ("last_joined", "最后连接（UTC+8）", K::Timestamp, false),
                ("first_joined", "首次连接（UTC+8）", K::Timestamp, false),
                ("bssid", "BSSID", K::Text, false),
                ("hotspot", "是否个人热点", K::Text, false),
                ("source", "来源", K::Text, false),
            ],
        ),
        sc(
            "bt_device",
            "蓝牙设备",
            "dconnect",
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
            "dconnect",
            vec![
                ("username", "用户名", K::Text, true),
                ("account_type", "账户类型", K::Text, false),
                ("description", "描述", K::Text, false),
                ("date", "时间（UTC+8）", K::Timestamp, false),
                ("bundle", "归属应用", K::Text, false),
            ],
        ),
        sc(
            "coreduet_attachment",
            "传输工件（分享附件）",
            "ginteraction",
            vec![
                ("interaction_pk", "交互记录号", K::Int, false),
                ("source_app", "来源应用", K::Text, false),
                ("target_app", "目标渠道", K::Text, false),
                ("uti", "文件类型（UTI）", K::Text, true),
                ("size", "字节数", K::Int, false),
                ("photo_uuid", "照片 UUID", K::Text, false),
                ("url", "内容 URL", K::Text, false),
                ("text", "内容文本", K::Text, false),
                ("person", "照片中人物", K::Text, false),
                ("scene", "场景描述", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("recipient", "接收方", K::Text, false),
                ("orphan", "孤儿标记", K::Text, false),
            ],
        ),
        sc(
            "coreduet_contact",
            "交互联系人（长期统计）",
            "ginteraction",
            vec![
                ("name", "名称", K::Text, true),
                ("handle", "账号标识", K::Text, false),
                ("custom_id", "平台原生 ID", K::Text, false),
                ("handle_type", "标识类型", K::Text, false),
                ("person_id", "通讯录关联", K::Text, false),
                ("person_link", "通讯录关联状态", K::Text, false),
                ("recv_count", "对方发来次数", K::Int, false),
                ("sent_count", "本机发出次数", K::Int, false),
                ("cc_count", "同收次数", K::Int, false),
                ("first_recv", "首次被联系（UTC+8）", K::Timestamp, false),
                ("last_recv", "末次被联系（UTC+8）", K::Timestamp, false),
                ("first_sent", "首次发出（UTC+8）", K::Timestamp, false),
                ("last_sent", "末次发出（UTC+8）", K::Timestamp, false),
                ("created", "记录建立（UTC+8）", K::Timestamp, false),
                ("kind", "类别", K::Text, false),
            ],
        ),
        sc(
            "photos_qr",
            "照片二维码与条码载荷",
            "fphotos",
            vec![
                ("filename", "文件名", K::Text, true),
                ("symbology", "码制类型", K::Text, false),
                ("payload", "解码内容/载荷", K::Text, false),
                ("confidence", "置信度", K::Float, false),
                ("created", "照片拍摄时间", K::Timestamp, false),
                ("source_app", "保存来源 App", K::Text, false),
                ("asset_path", "检材媒体路径", K::Path, false),
                ("directory", "目录", K::Text, false),
                ("location", "拍摄坐标", K::Text, false),
                ("uuid", "资产 UUID", K::Text, false),
                ("unsigned_id", "内部特征码", K::Int, false),
            ],
        ),
        sc(
            "photos_tombstone",
            "永久删除记录 (最近删除清空日志)",
            "fphotos",
            vec![
                ("entity", "实体类型", K::Text, true),
                ("entity_pk", "原记录主键", K::Int, false),
                ("recovered_filename", "WAL还原文件名", K::Text, false),
                ("original_filesize", "原始文件大小(字节)", K::Int, false),
                (
                    "cloud_master_guid",
                    "云端主唯一标识符 (ZCLOUDMASTERGUID)",
                    K::Text,
                    false,
                ),
                ("created", "原拍摄/生成时间（UTC+8）", K::Timestamp, false),
                ("trashed_date", "入回收站时间（UTC+8）", K::Timestamp, false),
                (
                    "trash_delay_secs",
                    "拍摄至入回收站耗时(秒)",
                    K::Float,
                    false,
                ),
                ("trashed_reason", "移入回收站原因", K::Text, false),
                ("delete_time", "彻底删除时间（UTC+8）", K::Timestamp, false),
                ("delete_reason", "永久删除原因", K::Text, false),
                ("cloud_delete_state", "云端/本地清除状态", K::Text, false),
                ("uuid", "原资产 UUID", K::Text, false),
                (
                    "cloud_guid",
                    "云端资产 GUID (ZCLOUDASSETGUID)",
                    K::Text,
                    false,
                ),
                ("imported_by", "保存来源应用", K::Text, false),
                ("process", "执行进程", K::Text, false),
                ("context", "事务上下文", K::Text, false),
            ],
        ),
        sc(
            "photos_wal_history",
            "WAL/SHM 历史版本与已删恢复",
            "fphotos",
            vec![
                ("asset_pk", "资产主键", K::Int, true),
                ("filename", "文件名 (ZFILENAME)", K::Text, false),
                ("original_filename", "原始文件名", K::Text, false),
                ("original_filesize", "原始文件大小(字节)", K::Int, false),
                (
                    "cloud_master_guid",
                    "云端主唯一标识符 (ZCLOUDMASTERGUID)",
                    K::Text,
                    false,
                ),
                ("record_state", "记录生命周期状态", K::Text, false),
                ("wal_status", "WAL 帧状态", K::Text, false),
                ("wal_frame", "WAL 帧号", K::Int, false),
                ("wal_commit", "WAL 提交序号", K::Int, false),
                ("page_no", "数据库页号", K::Int, false),
                ("created", "拍摄/生成时间（UTC+8）", K::Timestamp, false),
                (
                    "trashed_date",
                    "移入回收站时间（UTC+8）",
                    K::Timestamp,
                    false,
                ),
                (
                    "trash_delay_secs",
                    "拍摄至入回收站耗时(秒)",
                    K::Float,
                    false,
                ),
                ("trashed_state", "回收站状态", K::Text, false),
                ("trashed_reason", "移入回收站原因", K::Text, false),
                ("delete_time", "彻底删除时间（UTC+8）", K::Timestamp, false),
                ("delete_reason", "永久删除原因", K::Text, false),
                ("imported_by", "保存来源应用", K::Text, false),
                ("added", "加入相册（UTC+8）", K::Timestamp, false),
                ("directory", "存储目录", K::Text, false),
                ("uuid", "资产 UUID", K::Text, false),
                ("width", "宽度(px)", K::Int, false),
                ("height", "高度(px)", K::Int, false),
                ("duration", "时长(秒)", K::Float, false),
                ("favorite", "收藏", K::Text, false),
                ("hidden", "隐藏", K::Text, false),
            ],
        ),
        sc(
            "photos_wal_meta",
            "WAL/SHM 与主键空洞概览",
            "fphotos",
            vec![
                ("item", "检查项", K::Text, true),
                ("value", "关键结果", K::Text, false),
                ("status", "状态说明", K::Text, false),
                ("detail", "详细取证数据", K::Text, false),
            ],
        ),
        sc(
            "photos_share",
            "照片分享记录（渠道未知）",
            "fphotos",
            vec![
                ("direction", "方向", K::Text, true),
                ("file", "文件名", K::Text, true),
                ("time", "分享时间（UTC+8）", K::Timestamp, false),
                ("peer", "对端", K::Text, false),
                ("channel", "渠道", K::Text, false),
                ("detail", "说明", K::Text, false),
            ],
        ),
        sc(
            "coreduet_event",
            "交互事件",
            "ginteraction",
            vec![
                ("event_pk", "记录号", K::Int, true),
                ("source_app", "来源应用", K::Text, false),
                ("target_app", "目标渠道", K::Text, false),
                ("direction", "方向", K::Text, false),
                ("direction_code", "方向码", K::Int, false),
                ("mechanism", "通道", K::Text, false),
                ("mechanism_code", "通道码", K::Int, false),
                ("recipient_count", "接收方数（标称）", K::Int, false),
                ("group_name", "群名", K::Text, false),
                ("domain_id", "对端标识（JID/会话 ID）", K::Text, false),
                ("derived_intent", "派生意图标识", K::Text, false),
                ("account", "本机账户", K::Text, false),
                ("sender_name", "发送方名称", K::Text, false),
                ("sender_handle", "发送方标识", K::Text, false),
                ("recipients", "接收方", K::Text, false),
                ("attachment_count", "附件数", K::Int, false),
                ("attachments", "附件类型", K::Text, false),
                ("content_url", "内容 URL", K::Text, false),
                ("is_response", "是回复", K::Text, false),
                ("start", "开始（UTC+8）", K::Timestamp, false),
                ("end", "结束（UTC+8）", K::Timestamp, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("created", "写入时间（UTC+8）", K::Timestamp, false),
                ("updated", "更新时间（UTC+8）", K::Timestamp, false),
                ("uuid", "事件 UUID", K::Text, false),
                ("notes", "说明", K::Text, false),
            ],
        ),
        sc(
            "coreduet_store",
            "交互库元信息",
            "ginteraction",
            vec![
                ("item", "条目", K::Text, true),
                ("value", "数值", K::Int, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("detail", "详情", K::Text, false),
            ],
        ),
        sc(
            "coreduet_keyword",
            "交互关键词",
            "ginteraction",
            vec![
                ("keyword", "关键词", K::Text, true),
                ("source_app", "来源应用", K::Text, false),
                ("event_pk", "关联记录号", K::Int, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "photos_asset",
            "相册照片与视频",
            "fphotos",
            vec![
                // === 主表默认展示的 14 个核心取证列 ===
                ("filename", "现存文件名", K::Text, true),
                ("original_filename", "原始文件名", K::Text, false),
                ("media_type", "媒体类型", K::Text, false),
                ("subtype", "子类型", K::Text, false),
                ("created", "拍摄时间", K::Timestamp, false),
                ("added", "入库时间", K::Timestamp, false),
                ("source", "来源渠道", K::Text, false),
                ("source_app", "保存来源 App", K::Text, false),
                ("location_flag", "定位异常告警", K::Text, false),
                ("reverse_geo", "反向地理编码地址", K::Text, false),
                ("location", "GPS 坐标", K::Text, false),
                ("hidden", "已隐藏", K::Text, false),
                ("deleted", "最近删除状态", K::Text, false),
                ("asset_path", "检材媒体路径", K::Path, false),
                // === 默认 .hidden()（在右侧详情栏和 CSV 导出中完整展示） ===
                ("directory", "目录", K::Text, false),
                ("modified", "修改时间", K::Timestamp, false),
                ("exif_time", "EXIF 原始时间", K::Text, false),
                ("tz_name", "拍摄时区", K::Text, false),
                ("tz_offset", "时区偏移", K::Text, false),
                ("exif_lat", "EXIF 纬度", K::Float, false),
                ("exif_lon", "EXIF 经度", K::Float, false),
                ("make", "相机制造商", K::Text, false),
                ("model", "相机型号", K::Text, false),
                ("lens", "镜头型号", K::Text, false),
                ("focal_length", "焦距(mm)", K::Float, false),
                ("aperture", "光圈(f/)", K::Float, false),
                ("shutter", "快门速度", K::Text, false),
                ("iso", "ISO 感光度", K::Int, false),
                ("flash", "闪光灯", K::Text, false),
                ("hdr", "HDR 类型", K::Text, false),
                ("width", "宽度(px)", K::Int, false),
                ("height", "高度(px)", K::Int, false),
                ("orientation", "方向", K::Int, false),
                ("duration", "时长(秒)", K::Float, false),
                ("filesize", "文件大小", K::Size, false),
                ("original_filesize", "原始文件大小(字节)", K::Int, false),
                ("uti", "类型标识(UTI)", K::Text, false),
                ("favorite", "已收藏", K::Text, false),
                ("trashed_date", "移入回收站时间", K::Timestamp, false),
                ("trashed_reason", "移入回收站原因", K::Text, false),
                (
                    "trash_delay_secs",
                    "拍摄至入回收站耗时(秒)",
                    K::Float,
                    false,
                ),
                ("adjusted", "已修图/裁剪", K::Text, false),
                ("burst_uuid", "连拍组 UUID", K::Text, false),
                ("live_photo_uuid", "实况照片组ID", K::Text, false),
                ("cloud_status", "iCloud 同步状态", K::Text, false),
                (
                    "cloud_guid",
                    "iCloud 资产 GUID (ZCLOUDASSETGUID)",
                    K::Text,
                    false,
                ),
                (
                    "cloud_master_guid",
                    "云端主唯一标识符 (ZCLOUDMASTERGUID)",
                    K::Text,
                    false,
                ),
                ("shared_by", "共享者", K::Text, false),
                ("view_count", "浏览次数", K::Int, false),
                ("play_count", "播放次数", K::Int, false),
                ("share_count", "分享次数", K::Int, false),
                ("uuid", "资产 UUID", K::Text, false),
            ],
        ),
        sc(
            "photos_album",
            "相册",
            "fphotos",
            vec![
                ("title", "相册名", K::Text, true),
                ("album_kind", "相册种类", K::Text, false),
                ("asset_count", "资产数", K::Int, false),
                ("photo_count", "照片数", K::Int, false),
                ("video_count", "视频数", K::Int, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("deleted", "删除状态", K::Text, false),
                ("owner", "云端所有者", K::Text, false),
                ("imported_by", "导入方", K::Text, false),
                ("uuid", "UUID", K::Text, false),
                ("kind_code", "相册类型码", K::Int, false),
            ],
        ),
        sc(
            "photos_album_asset",
            "相册成员",
            "fphotos",
            vec![
                ("album", "相册名", K::Text, true),
                ("filename", "文件名", K::Text, true),
            ],
        ),
        sc(
            "photos_moment",
            "回忆时刻",
            "fphotos",
            vec![
                ("title", "标题", K::Text, true),
                ("subtitle", "副标题", K::Text, false),
                ("start", "开始（UTC+8）", K::Timestamp, false),
                ("end", "结束（UTC+8）", K::Timestamp, false),
                ("asset_count", "资产数", K::Int, false),
                ("photo_count", "照片数", K::Int, false),
                ("places", "地点", K::Text, false),
                ("deleted", "删除状态", K::Text, false),
            ],
        ),
        sc(
            "photos_person",
            "人物聚类",
            "fphotos",
            vec![
                ("name", "人物", K::Text, true),
                ("face_count", "人脸数", K::Int, false),
                ("status", "状态", K::Text, false),
                ("identifier", "人物标识", K::Text, false),
            ],
        ),
        sc(
            "photos_scene",
            "相册场景与实况语音识别",
            "fphotos",
            vec![
                ("scene_name", "识别标签", K::Text, true),
                ("channel_name", "分类通道", K::Text, false),
                ("asset_count", "命中照片数", K::Int, false),
                ("max_confidence", "最高置信度", K::Float, false),
                ("sample_files", "命中文件名", K::Text, false),
                ("start_sec", "起始时间(秒)", K::Float, false),
                ("duration", "持续时长(秒)", K::Float, false),
                ("asset_path", "检材媒体路径", K::Path, false),
                ("scene_id", "场景编号(SceneID)", K::Int, false),
                ("channel", "通道编号", K::Int, false),
                ("bbox", "检测框(归一化)", K::Text, false),
            ],
        ),
        sc(
            "photos_cloud",
            "iCloud 云端资产",
            "fphotos",
            vec![
                ("guid", "云 GUID", K::Text, true),
                ("state", "本地状态", K::Text, false),
                ("imported_by", "导入方", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("import_date", "导入时间（UTC+8）", K::Timestamp, false),
                ("placeholder", "占位状态", K::Int, false),
                ("codec", "编码格式", K::Text, false),
            ],
        ),
        sc(
            "photos_analysis",
            "资产分析状态",
            "fphotos",
            vec![
                ("filename", "文件名", K::Text, true),
                ("scene_count", "场景标签数", K::Int, false),
                ("analysis_state", "分析状态", K::Int, false),
                ("worker_type", "分析任务类型", K::Int, false),
                ("worker_flags", "分析任务标记", K::Int, false),
            ],
        ),
        sc(
            "photo",
            "相册资产",
            "fmedia",
            vec![
                ("filename", "文件名", K::Text, true),
                ("media_type", "媒体类型", K::Text, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("created", "拍摄时间（UTC+8）", K::Timestamp, false),
                ("added", "加入相册（UTC+8）", K::Timestamp, false),
                ("original_filename", "原始文件名", K::Text, false),
                ("source_app", "导入/拍摄应用", K::Text, false),
                ("cloud", "iCloud 存储", K::Text, false),
                ("gps_lat", "纬度", K::Float, false),
                ("gps_lng", "经度", K::Float, false),
                ("orientation", "方向", K::Int, false),
                ("hidden", "隐藏", K::Text, false),
                ("kind_subtype", "子类型", K::Int, false),
                ("share_count", "分享次数", K::Int, false),
                ("last_shared", "最后分享（UTC+8）", K::Timestamp, false),
                ("directory", "存储目录", K::Text, false),
            ],
        ),
        sc(
            "note",
            "笔记",
            "fmedia",
            vec![
                ("title", "标题", K::Text, true),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("edited", "修改时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "log_event",
            "系统日志（Unified Log）",
            "glog",
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
            "ccomm",
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
            "dconnect",
            vec![
                ("account", "账户", K::Text, true),
                ("service", "服务", K::Text, false),
                ("item_class", "条目类别", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("modified", "修改时间（UTC+8）", K::Timestamp, false),
                ("data_len", "数据长度", K::Int, false),
                ("secret", "明文数据", K::Text, false),
                ("note", "说明", K::Text, false),
            ],
        ),
        sc(
            "web_history",
            "浏览器历史",
            "bappinfo",
            vec![
                ("time", "访问时间（UTC+8）", K::Timestamp, false),
                ("url", "网址", K::Text, true),
                ("title", "标题", K::Text, false),
                ("visit_count", "访问次数", K::Int, false),
                ("browser", "浏览器", K::Text, false),
            ],
        ),
        sc(
            "web_bookmark",
            "浏览器书签",
            "bappinfo",
            vec![
                ("browser", "浏览器", K::Text, false),
                ("title", "标题", K::Text, false),
                ("url", "网址", K::Text, true),
                ("folder", "所属文件夹", K::Text, false),
                ("added", "添加时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "web_download",
            "浏览器下载",
            "bappinfo",
            vec![
                ("browser", "浏览器", K::Text, false),
                ("url", "来源网址", K::Text, true),
                ("file", "文件名", K::Text, false),
                ("size", "大小", K::Size, false),
                ("time", "下载时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_msg",
            "应用消息",
            "ccomm",
            vec![
                ("app", "应用", K::Text, false),
                ("chat", "会话/群", K::Text, false),
                ("sender", "发送者", K::Text, false),
                ("content", "内容", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("direction", "方向", K::Text, false),
                ("msg_type", "消息类型", K::Text, false),
                ("fmedia", "媒体/附件", K::Text, false),
            ],
        ),
        sc(
            "app_chat",
            "应用会话/群组",
            "ccomm",
            vec![
                ("app", "应用", K::Text, false),
                ("chat_id", "会话ID", K::Text, true),
                ("name", "名称", K::Text, false),
                ("chat_type", "类型", K::Text, false),
                ("member_count", "成员数", K::Int, false),
                ("creator", "创建者", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("archived", "已封存", K::Text, false),
                ("extra", "其他", K::Text, false),
            ],
        ),
        sc(
            "photo_album",
            "相簿",
            "fmedia",
            vec![
                ("name", "相簿名称", K::Text, true),
                ("kind", "类型", K::Text, false),
                ("asset_count", "资产数", K::Int, false),
                ("note", "说明", K::Text, false),
            ],
        ),
        sc(
            "app_config",
            "应用配置键值",
            "bappinfo",
            vec![
                ("app", "应用/组件", K::Text, true),
                ("key", "键", K::Text, false),
                ("value", "值", K::Text, false),
                ("source", "来源文件", K::Text, false),
            ],
        ),
        sc(
            "icloud_photo",
            "iCloud 照片（服务/同步/云库）",
            "jicloud",
            vec![
                ("service", "服务", K::Text, true),
                ("key", "键", K::Text, false),
                ("value", "值", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("source", "来源文件", K::Text, false),
            ],
        ),
        sc(
            "icloud_findmy",
            "iCloud 查找（Find My）",
            "jicloud",
            vec![
                ("service", "服务", K::Text, true),
                ("key", "键", K::Text, false),
                ("value", "值", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("source", "来源文件", K::Text, false),
            ],
        ),
        sc(
            "icloud_drive",
            "iCloud Drive（云盘）",
            "jicloud",
            vec![
                ("service", "服务", K::Text, true),
                ("key", "键", K::Text, false),
                ("value", "值", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("source", "来源文件", K::Text, false),
            ],
        ),
        sc(
            "airdrop_transfer",
            "AirDrop 传输记录",
            "kairdrop",
            vec![
                ("direction", "方向", K::Text, false),
                ("file", "文件名", K::Text, true),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("peer", "对端装置", K::Text, false),
                ("mode", "传输模式", K::Text, false),
                ("detail", "说明", K::Text, false),
                ("media_path", "检材文件路径", K::Path, false),
            ],
        ),
        Spec {
            id: "airdrop_identity",
            label: "AirDrop 本机标识",
            fields: vec![
                ("key", "项", K::Text, true),
                ("value", "值", K::Text, false),
                ("note", "说明", K::Text, false),
            ],
            properties: Some(("value", Some("key"), Some("note"))),
            category: "kairdrop",
        },
        sc(
            "coreduet_interaction",
            "跨应用交互残留记录",
            "kairdrop",
            vec![
                ("start_time", "交互开始时间", K::Timestamp, false),
                ("end_time", "交互结束时间", K::Timestamp, false),
                ("app_name", "应用名称", K::Text, false),
                ("bundle_id", "应用 BundleID", K::Text, false),
                ("direction", "交互方向", K::Text, false),
                ("mechanism", "交互机制", K::Text, false),
                ("sender", "发送方账号/昵称", K::Text, false),
                ("recipients", "接收方账号/昵称", K::Text, false),
                ("group_name", "群组/会话名称", K::Text, false),
                ("domain_id", "会话域标识(Domain/ChatID)", K::Text, false),
                ("attachments", "关联附件摘要", K::Text, false),
                ("content_url", "关联链接", K::Text, false),
                ("account", "本机账户", K::Text, false),
                ("target_bundle_id", "目标扩展 BundleID", K::Text, false),
                ("pk", "记录主键(Z_PK)", K::Int, true),
            ],
        ),
        sc(
            "cloud_backup",
            "iCloud 备份",
            "jicloud",
            vec![
                ("item", "事项", K::Text, true),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("state", "状态", K::Text, false),
                ("detail", "说明", K::Text, false),
            ],
        ),
        sc(
            "wa_chat",
            "WhatsApp 会话/群组",
            "iwhatsapp",
            vec![
                ("chat_id", "会话ID（JID）", K::Text, true),
                ("name", "名称", K::Text, false),
                ("chat_type", "类型", K::Text, false),
                ("member_count", "成员数", K::Int, false),
                ("admins", "管理员", K::Text, false),
                ("creator", "创建者", K::Text, false),
                ("created", "创建时间（UTC+8）", K::Timestamp, false),
                ("archived", "已封存", K::Text, false),
                ("extra", "其他", K::Text, false),
            ],
        ),
        sc(
            "wa_msg",
            "WhatsApp 消息",
            "iwhatsapp",
            vec![
                ("chat_id", "会话ID（JID）", K::Text, true),
                ("chat_name", "会话名称", K::Text, false),
                ("sender", "发送者", K::Text, false),
                ("direction", "方向", K::Text, false),
                ("msg_type", "消息类型", K::Text, false),
                ("content", "内容", K::Text, false),
                ("fmedia", "媒体/附件", K::Text, false),
                ("media_path", "检材媒体路径", K::Path, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
                ("read_summary", "已读/未读状态", K::Text, false),
                ("receipts", "回执明细", K::Json, false),
                ("poll", "投票结果", K::Json, false),
                ("reactions", "消息回应", K::Json, false),
                ("reply_to", "引用回复", K::Json, false),
            ],
        ),
        sc(
            "wa_poll",
            "WhatsApp 投票",
            "iwhatsapp",
            vec![
                ("chat_id", "会话ID（JID）", K::Text, false),
                ("chat_name", "会话名称", K::Text, false),
                ("creator", "发起人", K::Text, false),
                ("time", "发起时间（UTC+8）", K::Timestamp, false),
                ("question", "投票问题", K::Text, true),
                ("allow_multiple", "单/多选", K::Text, false),
                ("total_voters", "参与人数", K::Int, false),
                ("total_votes", "总票数", K::Int, false),
                ("results_summary", "得票汇总", K::Text, false),
                ("poll", "投票明细", K::Json, false),
            ],
        ),
        sc(
            "wa_geo",
            "WhatsApp 位置",
            "iwhatsapp",
            vec![
                ("chat_name", "会话名称", K::Text, false),
                ("name", "地点名称", K::Text, false),
                ("lat", "纬度", K::Float, false),
                ("lng", "经度", K::Float, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "wa_call",
            "WhatsApp 通话",
            "iwhatsapp",
            vec![
                ("peer", "对方 JID/号码", K::Text, true),
                ("direction", "类型", K::Text, false),
                ("duration", "时长（秒）", K::Duration, false),
                ("video", "视频通话", K::Text, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_user",
            "应用账户",
            "bappinfo",
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
            "bappinfo",
            vec![
                ("app", "应用", K::Text, false),
                ("keyword", "关键词", K::Text, true),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "app_geo",
            "应用位置",
            "bappinfo",
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
            "bappinfo",
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
            "fmedia",
            vec![
                ("app", "应用", K::Text, false),
                ("name", "名称", K::Text, true),
                ("path", "路径", K::Path, false),
                ("size", "大小", K::Size, false),
                ("time", "时间（UTC+8）", K::Timestamp, false),
            ],
        ),
        sc(
            "media_exif",
            "媒体文件元数据",
            "fmedia",
            vec![
                ("path", "路径", K::Path, true),
                ("name", "文件名", K::Text, false),
                ("app", "来源应用/目录", K::Text, false),
                ("size", "大小", K::Size, false),
                ("sha256", "SHA256", K::Text, false),
                ("make", "相机厂商", K::Text, false),
                ("model", "相机型号", K::Text, false),
                ("software", "软件", K::Text, false),
                ("taken", "拍摄时间（EXIF）", K::Text, false),
                ("orientation", "方向", K::Int, false),
                ("gps_lat", "纬度", K::Float, false),
                ("gps_lng", "经度", K::Float, false),
            ],
        ),
        sc(
            "sim",
            "SIM 卡",
            "dconnect",
            vec![
                ("slot", "卡槽/标识", K::Text, true),
                ("iccid", "ICCID", K::Text, false),
                ("imei", "IMEI", K::Text, false),
                ("phone", "号码", K::Text, false),
                ("label", "标签", K::Text, false),
                ("extra", "其他", K::Text, false),
            ],
        ),
        sc(
            "app_artifact",
            "应用数据片段",
            "bappinfo",
            vec![
                ("app", "应用/文件", K::Text, false),
                ("kind", "类型", K::Text, false),
                ("value", "值", K::Text, true),
                ("path", "来源路径", K::Path, false),
                ("note", "说明", K::Text, false),
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
            let mut r = RecordSchema::new(&full, s.label);
            if let Some(tpl) = title_template(&full) {
                r = r.title(tpl);
            }
            for (id, label, k, key) in s.fields {
                let mut f = FieldSchema::new(id, label, k);
                if is_hidden_field(&full, id) {
                    f = f.hidden();
                }
                if matches!(k, K::Timestamp) {
                    if let Some(role) = timeline_role(&full, id) {
                        f = f.timeline(role);
                    }
                }
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
            #[cfg(feature = "mod-chat")]
            if let Some(chat) = chat_layout(&full) {
                r = r.chat(chat);
            }
            r
        })
        .collect()
}

fn is_hidden_field(type_id: &str, field_id: &str) -> bool {
    match type_id {
        "ios.photos_asset" => matches!(
            field_id,
            "directory"
                | "modified"
                | "exif_time"
                | "tz_name"
                | "tz_offset"
                | "exif_lat"
                | "exif_lon"
                | "make"
                | "model"
                | "lens"
                | "focal_length"
                | "aperture"
                | "shutter"
                | "iso"
                | "flash"
                | "hdr"
                | "width"
                | "height"
                | "orientation"
                | "duration"
                | "filesize"
                | "original_filesize"
                | "uti"
                | "favorite"
                | "trashed_date"
                | "trashed_reason"
                | "trash_delay_secs"
                | "adjusted"
                | "burst_uuid"
                | "live_photo_uuid"
                | "cloud_status"
                | "cloud_guid"
                | "cloud_master_guid"
                | "shared_by"
                | "view_count"
                | "play_count"
                | "share_count"
                | "uuid"
        ),
        "ios.photos_qr" => matches!(field_id, "directory" | "location" | "uuid" | "unsigned_id"),
        "ios.photos_scene" => matches!(field_id, "scene_id" | "channel" | "bbox"),
        "ios.photos_wal_history" => matches!(
            field_id,
            "added"
                | "directory"
                | "uuid"
                | "width"
                | "height"
                | "duration"
                | "favorite"
                | "hidden"
        ),
        "ios.wa_msg" => matches!(
            field_id,
            "media_path" | "receipts" | "poll" | "reactions" | "reply_to"
        ),
        "ios.wa_poll" => matches!(field_id, "poll"),
        "ios.airdrop_transfer" => matches!(
            field_id,
            "hash" | "real_file" | "source_table" | "bundle_id" | "chat_id"
        ),
        "ios.coreduet_interaction" => matches!(
            field_id,
            "end_time" | "bundle_id" | "account" | "target_bundle_id" | "pk"
        ),
        _ => false,
    }
}

fn timeline_role(type_id: &str, field_id: &str) -> Option<&'static str> {
    match (type_id, field_id) {
        ("ios.photos_asset", "created") => Some("photo_created"),
        ("ios.photos_asset", "added") => Some("photo_added"),
        ("ios.photos_asset", "trashed_date") => Some("photo_trashed"),
        ("ios.photos_tombstone", "trashed_date") => Some("photo_trashed"),
        ("ios.photos_tombstone", "delete_time") => Some("photo_expunged"),
        ("ios.photos_wal_history", "created") => Some("photo_created"),
        ("ios.photos_wal_history", "trashed_date") => Some("photo_trashed"),
        ("ios.photos_wal_history", "delete_time") => Some("photo_expunged"),
        ("ios.photos_qr", "created") => Some("qr_photo_created"),
        ("ios.airdrop_transfer", "time") => Some("airdrop_transfer"),
        ("ios.coreduet_interaction", "start_time") => Some("coreduet_interaction"),
        ("ios.sms", "time") => Some("sms_time"),
        ("ios.calllog", "time") => Some("call_time"),
        ("ios.safari_history", "visit_time") => Some("safari_visit"),
        ("ios.safari_download", "time") => Some("safari_download"),
        ("ios.unlock", "time") => Some("screen_unlock"),
        ("ios.app_usage", "start") => Some("app_foreground"),
        ("ios.app_install", "time") => Some("app_install"),
        ("ios.location", "time") => Some("location_fix"),
        ("ios.wifi_known", "last_joined") => Some("wifi_joined"),
        ("ios.bt_device", "last_conn") => Some("bt_connected"),
        ("ios.data_usage", "time") => Some("data_usage"),
        ("ios.data_usage", "start_time") => Some("process_start"),
        ("ios.wa_msg", "time") => Some("wa_msg"),
        ("ios.wa_poll", "time") => Some("wa_poll"),
        _ => None,
    }
}

fn title_template(type_id: &str) -> Option<&'static str> {
    match type_id {
        "ios.photos_asset" => Some("{filename} ({media_type})"),
        "ios.photos_tombstone" => Some("永久删除：{entity} #{entity_pk}"),
        "ios.photos_wal_history" => Some("Z_PK #{asset_pk} · {filename} ({record_state})"),
        "ios.photos_wal_meta" => Some("{item}：{value}"),
        "ios.photos_qr" => Some("[{symbology}] {payload}"),
        "ios.photos_scene" => Some("{scene_name} ({channel_name})"),
        "ios.photos_album" => Some("{title} ({asset_count} 项)"),
        "ios.photos_person" => Some("{name} ({face_count} 张人脸)"),
        "ios.airdrop_transfer" => Some("{direction} · {file}"),
        "ios.coreduet_interaction" => Some("[{direction}] {app_name} · {sender}"),
        "ios.data_usage" => Some("{app_name} ({bundle}) 流量使用"),
        "ios.finding" => Some("{item}：{finding}"),
        "ios.wa_msg" => Some("{sender}：{content}"),
        "ios.wa_poll" => Some("{question}（{total_votes} 票）"),
        _ => None,
    }
}

#[cfg(feature = "mod-chat")]
fn chat_layout(type_id: &str) -> Option<ChatLayout> {
    match type_id {
        "ios.wa_msg" => Some(
            ChatLayout::new("chat_id", "content")
                .thread_title("chat_name")
                .sender("sender")
                .direction("direction")
                .outgoing_values(vec!["发出".to_owned(), "Outgoing".to_owned()])
                .time("time")
                .msg_type("msg_type")
                .media("media_path")
                .status("read_summary")
                .receipts("receipts")
                .poll("poll")
                .reactions("reactions")
                .reply_to("reply_to"),
        ),
        _ => None,
    }
}

pub fn type_id(spec_id: &str) -> String {
    if spec_id.starts_with("powerlog_") {
        format!("powerlog.{}", spec_id.trim_start_matches("powerlog_"))
    } else {
        format!("ios.{spec_id}")
    }
}
