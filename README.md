# LovelyCarve iOS 备份与 Sysdiagnose 取证 1.0

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

## 时间口径

- Manifest `Date` 为 UTC：备份时间给出 UTC+8（进时间线）与 UTC 原值。
- powerlog `timestamp` 基准自动判别（Unix 秒 vs Apple epoch 2001 基准）：以库内中值对照 sysdiagnose 目录名锚点，命中 [2007, 锚点+2年] 即采用；无法判别时降级并告警。判别结果写入 ios.finding。
- 日志行首/CSV 时间为本地墙钟（时区未知）：按 local 存储不进时间线，结论文本按原样展示。

## 边界

- 备份口令不做爆破（KeyBag DPIC 可达千万次迭代）；加密状态与密钥包参数如实呈现。
- 备份文件树浏览（Manifest.db）由 LovelyCarve 宿主内置能力提供，本插件不重复实现。
- sms/通话/联系人/相册等业务库解析未包含（本类检材无对应数据文件）。
