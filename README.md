# LovelyCarve iOS 备份与 Sysdiagnose 取证 2.1

插件 ID `org.lovelycarve.iosbackup`。对 iOS 备份控制文件与 sysdiagnose 日志包做一站式取证分析。

## 使用

1. 「设置 → 插件」安装本包；把检材目录（同时含 `Manifest.plist` 与 `sysdiagnose_*/`，即“手机检材”类目录）作为**文件夹检材**添加。
2. 选中检材 → 快速分析 → **iOS备份综合取证分析**（evidence 作用域，自动收集全部输入）。
3. 或在文件列表选中 `.PLSQL` 右键「插件分析任务」→ **Powerlog数据库深度归纳**。

## 记录类型

| 类型 | 内容 | 来源 |
| --- | --- | --- |
| ios.backup | 设备名称/型号/序列号/UDID/iOS版本/备份时间(UTC+8与原值)/加密/密码设置 | Manifest.plist |
| ios.device_info | Lockdown 全部属性 + SystemVersion + PreflightSizing 域体积 | Manifest/SystemVersion/MobileBackup.plist |
| ios.keybag | BackupKeyBag TLV 参数（SALT/ITER/DPIC/DPSL/类密钥统计） | Manifest.plist |
| ios.app | 应用清单（Manifest Applications + AppVersions 名称/版本/第三方标志） | Manifest/powerlog |
| ios.install_event | 安装请求(Placeholder/Customer)/成功/卸载/销毁容器 | mobile_installation.log* |
| ios.bluetooth_state | 本机蓝牙 MAC 等 | bluetooth_status.txt |
| ios.battery | 电池序列号/激活时间(BDC_Once)/健康度/循环/最新快照 | BDC CSV + powerlog |
| ios.charge_full | 充至100%会话（原始时间戳 + UTC+8 + FullyCharged 首条） | powerlog Battery 表 |
| ios.unlock | 解锁事件（SBLock 1→0）+ 解锁后首个前台应用（ScreenState 关联） | powerlog |
| ios.lock_interval / ios.screen_interval | 锁定/亮屏区间 | powerlog |
| ios.app_usage / ios.battery_daily | 前后台时长、每日电池采样 | powerlog |
| ios.finding | 取证发现（答题式结论：备份时间/序列号/蓝牙MAC/电池激活/充满时间戳/首次解锁+触发应用/各应用首次安装请求） | 全部 |
| powerlog.overview | files 分析器的总览 | powerlog |

## v1.5.0 新增通用数据源

面向 iOS 16/17 备份与文件级提取的通用解析能力（不针对单一 App）：

| 类型 | 内容 | 来源 |
| --- | --- | --- |
| ios.device_info（扩展） | IMEI/IMEI2/MEID/ICCID/号码/蓝牙地址/热点名 | iDevice_info.txt、CommCenter、热点与蓝牙 plist |
| ios.sim | 卡槽/ICCID/IMEI/号码/运营商 | com.apple.commcenter.plist |
| ios.wifi（扩展） | 首次/最后连接、BSSID、热点标记、明文密码 | known-networks / wifi-networks 备份 / KeyChain.json |
| ios.keychain_item（扩展） | 明文 Data | KeyChain.json、keychain-backup.plist |
| ios.photo（扩展） | iCloud、导入来源、原始文件名、GPS、方向、隐藏、分享 | Photos.sqlite + ZADDITIONALASSETATTRIBUTES |
| ios.media_exif | 相机型号、EXIF 时间、方向、GPS、SHA256 | DCIM / IM 媒体 / 文件App / 应用文档 |
| ios.web_bookmark / web_download / app_search | 书签、下载、搜索词 | Bookmarks.db / Downloads.plist / History.db |
| ios.app（扩展） | 展示版本、构建号、安装时间 | Manifest + Info.plist iTunesMetadata + Preferences |
| ios.wa_chat / wa_msg / wa_geo / wa_call | WhatsApp 独立分析器：会话/群/频道/社群、消息、位置、通话 | ChatStorage.sqlite、CallHistory.sqlite |
| ios.media_share | 照片分享/空投：方向、文件名、时间（对端身份仅 sharingd 导入可知） | Photos.sqlite 分享字段 + sharingd 导入标记 |
| ios.cloud_backup | iCloud 备份：最近备份时间/状态/错误、账户启用、引擎运行记录 | com.apple.MobileBackup.plist |
| ios.photo_album | 相簿名称/类型/成员数 | Photos.sqlite ZGENERICALBUM |
| ios.app_config | 「照片」「文件」/ iCloud Drive 容器的全部配置键值 | com.apple.mobileslideshow.plist、CloudDocs 容器 |
| ios.app（data_path 列） | 应用数据目录：App 容器 / 组容器 / HomeDomain 系统路径，做题指路 | 检材目录结构自动发现 |
| ios.app（store_name 列） | **存储名（App Store itemName）** + 显示名：包名对不上也能看到真实名字（如 com.uazoo.ssc → Discreet） | Info.plist itemName、Photos 导入记录 |
| ios.app（版本号重构） | 版本号优先取**应用配置文件**（WhatsApp SPClientVersion=2.25.14.79 这类客户端版本），App Store 短版本次之，构建号兜底 | 组/应用容器 Preferences |
| ios.icloud_photo / icloud_findmy / icloud_drive | iCloud 独立分析器：照片服务/同步/云库、查找（Find My）、云盘域；二进制值做魔数识别（bplist 直接解析提取可读内容） | CPL 状态 plist、com.apple.icloud.*、store.cloudphotodb、FileProvider Domains |
| ios.airdrop_transfer / airdrop_identity | AirDrop 独立分析器：收发方向、文件名、时间、对端装置（接收资产的发送方设备型号/时区/原文件名）、传输模式（AWDL 点对点直连）；本机 AirDrop 标识 | Photos.sqlite 分享/导入标记 + ZEXTENDEDATTRIBUTES、com.apple.sharingd.plist |
| ios.app_artifact（重构） | **Realm 结构化解析**：Group→表→列，按"表.列"输出全部行值（如 TransferInfoEntity.transferType/peerDeviceId/key/state/detailedState/startTime）；解析失败回退字符串表提取 | *.realm（SendAnywhere 等全部 App 通用） |
| ios.app_artifact / app_user | Realm 字符串、损坏 plist 原始标识、KV+JSON 用户 | *.realm、Preferences、Apollo 类缓存 |

## 时间口径

- Manifest `Date` 为 UTC：备份时间给出 UTC+8（进时间线）与 UTC 原值。
- powerlog `timestamp` 基准自动判别（Unix 秒 vs Apple epoch 2001 基准）：以库内中值对照 sysdiagnose 目录名锚点，命中 [2007, 锚点+2年] 即采用；无法判别时降级并告警。判别结果写入 ios.finding。
- 日志行首/CSV 时间为本地墙钟（时区未知）：按 local 存储不进时间线，结论文本按原样展示。

## 边界

- 备份口令不做爆破（KeyBag DPIC 可达千万次迭代）；加密状态与密钥包参数如实呈现。
- 备份文件树浏览（Manifest.db）由 LovelyCarve 宿主内置能力提供，本插件不重复实现。
- 整库加密的第三方私密数据库与需远程解密的云账单不在解析范围内；U 盘/磁盘镜像等非 iOS 备份检材不属于本插件。
