//! iOS Photos 场景识别（ZSCENECLASSIFICATION.ZSCENEIDENTIFIER）真值字典。
//!
//! 来源：PFSceneTaxonomy / CSUSceneTaxonomy 的分类节点 ID（144 个视觉/顶层真值）+
//! SoundAnalysis 音频分类 ID。四个数字命名空间：
//! 1~2100 经典核心树；10000~30000 细粒度扩展树；
//! 2147481500+ 专用 CV 检测器；负数（0x80000000+ 溢出）为顶层环境宏分类。
//! 取值来源：iOS 取证手册卷二 §4.7 真值大字典（四库实测）。

/// 场景 ID → 名称。signed ID 为 SQLite 存储值（负数为 uint32 溢出）。
pub const SCENE_NAMES: &[(i64, &str)] = &[
    (-2147483643, "室内环境（Indoors）"),
    (-2147483641, "室外环境（Outdoor）"),
    (1, "机场跑道（Airfield）"),
    (7, "桥梁（Bridge）"),
    (8, "建筑物（Building）"),
    (15, "餐食菜肴（Chow）"),
    (23, "湖泊水体（Bayou）"),
    (32, "咖啡馆/茶餐厅（Cafe）"),
    (33, "城市林荫道（Avenue）"),
    (36, "摩天高楼（High Rise）"),
    (37, "体育场馆（Ballpark）"),
    (106, "航空器（Aircraft）"),
    (107, "客机（Aeroplane）"),
    (112, "坚果/杏仁（Almond）"),
    (122, "住宅公寓（Apartments）"),
    (130, "拱门建筑（Arch）"),
    (132, "大型场馆（Arena）"),
    (134, "艺术陈列（Art）"),
    (144, "汽车（Auto）"),
    (159, "球类（Ball）"),
    (229, "蓝天/晴空（Blue Sky）"),
    (247, "碗装器皿（Bowl）"),
    (312, "纸箱/包装盒（Carton）"),
    (334, "椅子/座椅（Chair）"),
    (340, "图表/统计表（Chart）"),
    (359, "筷子（Chopstick）"),
    (360, "圣诞装饰（Christmas Decoration）"),
    (362, "圣诞树（Christmas Tree）"),
    (382, "服装服饰（Apparel）"),
    (383, "多云/阴天（Cloudy）"),
    (407, "容器/储物箱（Container）"),
    (413, "线缆/电线（Cord）"),
    (439, "十字路口/斑马线（Crossing）"),
    (448, "水杯/茶杯（Cup）"),
    (473, "办公桌/书桌（Desk）"),
    (492, "文档/纸质材料（Document）"),
    (501, "门/门口（Door）"),
    (507, "饮品/饮料（Beverage）"),
    (546, "栅栏/围栏（Fence）"),
    (576, "鞋类（Footgear）"),
    (584, "边框/镜框（Frame）"),
    (590, "冰冷/雪景（Cold）"),
    (595, "家具/陈设（Furnishing）"),
    (652, "饺子/面点（Gyoza）"),
    (668, "帽子（Hat）"),
    (669, "头饰/头盔（Headdress）"),
    (712, "霜冻/冰晶（Frost）"),
    (726, "外套/大衣（Coat）"),
    (759, "键盘/小键盘（Keypad）"),
    (782, "灯具（Lamp）"),
    (783, "路灯/灯杆（Lamp Post）"),
    (784, "陆地/干地（Dry Land）"),
    (803, "光线/照明（Light）"),
    (804, "灯泡（Bulb）"),
    (821, "行李箱/包裹（Baggage）"),
    (825, "器械/设备（Apparatus）"),
    (839, "地图/导航（Map）"),
    (881, "人群/人类（Humans）"),
    (889, "赛车运动（Motor Sport）"),
    (906, "纳斯卡赛车（Nascar）"),
    (913, "坚果/果仁（Nut）"),
    (920, "海洋/大海（Ocean）"),
    (943, "绘画/颜料（Paint）"),
    (945, "棕榈树/椰树（Palm Tree）"),
    (961, "意面/面食（Pasta）"),
    (963, "赛道/球场（Course）"),
    (980, "凉亭/林荫棚（Arbour）"),
    (983, "电话/手机（Phone）"),
    (1006, "植物/花草（Flora）"),
    (1008, "盘子/餐盘（Plate）"),
    (1055, "手提包/钱包（Purse）"),
    (1088, "娱乐/休闲（Recreation）"),
    (1100, "米饭/主食（Rice）"),
    (1175, "海鲜/水产（Seafood）"),
    (1180, "座椅/席位（Seat）"),
    (1208, "灌木/树丛（Bush）"),
    (1209, "人行道/路面（Pavement）"),
    (1211, "标识/标牌（Sign）"),
    (1222, "天空/云彩（Skies）"),
    (1245, "足球（Football）"),
    (1246, "袜子（Sock）"),
    (1268, "勺子/调羹（Spoon）"),
    (1269, "体育运动（Sport）"),
    (1270, "运动器材（Athletic Equipment）"),
    (1280, "楼梯/台阶（Stair）"),
    (1284, "雕塑/雕像（Sculpture）"),
    (1303, "街道/马路（Street）"),
    (1304, "路牌/街标（Street Sign）"),
    (1345, "桌子/台面（Table）"),
    (1346, "餐具/器皿（Tableware）"),
    (1392, "工具/器具（Tool）"),
    (1414, "树木（Tree）"),
    (1441, "器皿/用具（Utensil）"),
    (1445, "蔬菜（Veg）"),
    (1447, "车辆/交通工具（Vehicle）"),
    (1469, "水/水面（H2O）"),
    (1499, "窗户/窗框（Window）"),
    (1520, "酸奶/乳制品（Yoghurt）"),
    (1622, "电脑/计算机（Computer）"),
    (1635, "小汽车（Car）"),
    (1667, "电脑键盘（Computer Keyboard）"),
    (1674, "消费电子设备（Consumer Electronics）"),
    (1688, "球类运动（Ball Games）"),
    (1715, "高速公路（Highway）"),
    (1736, "儿童/幼儿（Child）"),
    (1737, "观赏植物（Decorative Plant）"),
    (1739, "树叶/叶丛（Foliage）"),
    (1740, "树枝/枝丫（Branch）"),
    (1760, "密集人群（Crowd）"),
    (1761, "草地/草坪（Grass）"),
    (1762, "丘陵/山麓（Foothill）"),
    (1764, "夜空/星空（Night Sky）"),
    (1772, "城市全景/街市（Cityscape）"),
    (1773, "水体/江河湖海（Water Body）"),
    (10101, "航空旅行（Air Travel）"),
    (10112, "航空载具（Air Carrier）"),
    (10113, "民航客机（Airliner）"),
    (10454, "航空飞行（Aviation）"),
    (11770, "城市群（Cities）"),
    (11944, "通信设备/手机（Communication Device）"),
    (12016, "楼宇建筑（Building）"),
    (12416, "菜肴/装盘菜品（Dish）"),
    (12497, "商业中心/市中心（Downtown）"),
    (12674, "电子数码设备（Electronic Devices）"),
    (13235, "便携数码小工具（Gadget）"),
    (13991, "室内艺术品（Interior Art）"),
    (14398, "绿叶蔬菜（Greens）"),
    (14491, "标线/线条（Line）"),
    (14599, "午餐/简餐（Lunch）"),
    (14798, "正餐/餐食（Meal）"),
    (14871, "都市街区（Metro City）"),
    (14872, "大都会区（Greater Metropolitan）"),
    (15243, "面条/米粉（Noodle）"),
    (15425, "高架桥/立交桥（Flyover）"),
    (15716, "相框/镜框（Frame）"),
    (16133, "赛车道/赛马场（Race Track）"),
    (16493, "起降跑道（Landing Strip）"),
    (17006, "前沿科技（Advance Generation）"),
    (17019, "小吃/茶点（Snack）"),
    (17199, "体育场馆建筑（Athletic Building）"),
    (18062, "城市建成区（Urban Area）"),
    (18219, "墙壁/墙面（Wall）"),
    (18388, "宽体客机（Wide Body Aircraft）"),
    (18435, "机翼（Wing）"),
];

/// 音频分类（ZCLASSIFICATIONTYPE=3，SoundAnalysis）——独立于视觉 ID 空间。
pub const SCENE_AUDIO_NAMES: &[(i64, &str)] = &[
    (2, "Babble（人群嘈杂交谈声）"),
    (6, "Speech（清晰人类说话语音）"),
];

/// 顶层环境宏分类（负数溢出段）：室内/室外。
pub const SCENE_ENV_NAMES: &[(i64, &str)] = &[
    (-2147483643, "Indoors（室内环境）"),
    (-2147483641, "Outdoor（室外环境）"),
];

/// 场景 ID → 名称（视觉/顶层/音频三空间合一）。
pub fn scene_name(id: i64) -> Option<&'static str> {
    SCENE_NAMES
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| *v)
        .or_else(|| {
            SCENE_AUDIO_NAMES
                .iter()
                .find(|(k, _)| *k == id)
                .map(|(_, v)| *v)
        })
        .or_else(|| {
            SCENE_ENV_NAMES
                .iter()
                .find(|(k, _)| *k == id)
                .map(|(_, v)| *v)
        })
}

/// 负数 ID 还原为 uint32 原值（CoreData NSInteger32 二补数回绕）。
pub fn unsigned_scene_id(id: i64) -> u32 {
    if id < 0 {
        (id as i64 + 4_294_967_296) as u32
    } else {
        id as u32
    }
}

// ─────────────────────────── 枚举字典（取证手册卷二 §3） ───────────────────────────

/// 取枚举标签。
fn label(table: &[(i64, &'static str)], value: i64) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == value).map(|(_, v)| *v)
}

/// ZASSET.ZKIND —— 主媒体类型。
pub const ASSET_KIND: &[(i64, &str)] = &[(0, "照片"), (1, "视频"), (2, "音频"), (3, "未知/其他")];

/// ZASSET.ZKINDSUBTYPE —— 媒体子类型（截屏/实况/录屏等高价值模式）。
pub const ASSET_KIND_SUBTYPE: &[(i64, &str)] = &[
    (0, "普通照片/普通视频"),
    (1, "全景照片"),
    (2, "实况照片"),
    (10, "系统截屏"),
    (100, "慢动作"),
    (101, "延时摄影"),
    (102, "屏幕录制"),
    (103, "电影效果"),
    (104, "空间视频"),
];

/// ZASSET.ZSAVEDASSETTYPE —— 资产入库通道（来源溯源第一层）。
pub const ASSET_SAVED_TYPE: &[(i64, &str)] = &[
    (0, "默认/旧版胶卷"),
    (1, "Photo Stream / iTunes 同步"),
    (2, "Photo Stream / iTunes 同步"),
    (3, "本机相机拍摄/本地入库"),
    (4, "iCloud 照片图库同步下载"),
    (5, "遗留第三方应用转存"),
    (6, "iCloud 共享相册"),
    (7, "PhotoBooth 生成"),
    (8, "后台同步/外部应用导入"),
    (12, "Syndication「与你共享」"),
];

/// ZADDITIONALASSETATTRIBUTES.ZIMPORTEDBY —— 资产来源鉴定核心枚举。
pub const IMPORTED_BY: &[(i64, &str)] = &[
    (0, "未知/系统内部（含截屏）"),
    (1, "本机后置摄像头拍摄"),
    (2, "本机前置摄像头自拍"),
    (3, "Safari 浏览器保存"),
    (4, "PhotoBooth"),
    (5, "读卡器/外部设备导入"),
    (6, "iCloud/系统内部传递"),
    (7, "系统第一方应用保存（含 AirDrop 接收）"),
    (8, "第三方 App 保存到相册"),
    (9, "Syndication「与你共享」投影"),
    (10, "macOS/Finder/迁移工具"),
    (11, "macOS/Finder/迁移工具"),
    (12, "macOS/Finder/迁移工具"),
    (13, "macOS/Finder/迁移工具"),
    (14, "macOS/Finder/迁移工具"),
    (15, "macOS/Finder/迁移工具"),
];

/// ZASSET.ZVISIBILITYSTATE —— 隐藏状态。
pub const VISIBILITY_STATE: &[(i64, &str)] = &[(0, "正常可见"), (2, "已装入「已隐藏」加密相册")];

/// ZASSET.ZTRASHEDSTATE —— 最近删除回收站状态。
pub const TRASHED_STATE: &[(i64, &str)] = &[
    (0, "正常 (未删除)"),
    (1, "在「最近删除」回收站"),
    (2, "已从回收站永久移除"),
];

/// ZASSET.ZTRASHEDREASON / ACHANGE.ZTOMBSTONE4 —— 删除原因。
pub const TRASHED_REASON: &[(i64, &str)] = &[
    (0, "无 / 正常未删除"),
    (1, "用户主动移至「最近删除」"),
    (2, "iCloud / 同步级联移除"),
    (3, "系统策略 / 过期清理"),
    (4, "连拍筛选 / 重复合并清理"),
];

/// ACHANGE.ZTOMBSTONE1 —— 云端/彻底删除标记状态。
pub const CLOUD_DELETE_STATE: &[(i64, &str)] = &[
    (0, "正常未删除"),
    (1, "已本地永久清除 / 清空回收站"),
    (2, "云端同步标记删除"),
    (3, "待云端确认清除"),
];

/// ACHANGE.ZCHANGETYPE —— 持久历史变更操作类型。
pub const ACHANGE_TYPE: &[(i64, &str)] = &[
    (0, "新增 (Insert)"),
    (1, "更新 (Update)"),
    (2, "硬删除 (Delete)"),
];

/// ZASSET.ZCLOUDLOCALSTATE —— iCloud 同步状态。
pub const CLOUD_LOCAL_STATE: &[(i64, &str)] = &[
    (0, "未同步（纯本地）"),
    (1, "等待上传"),
    (2, "上传中"),
    (3, "已同步至 iCloud"),
    (4, "已同步至 iCloud"),
];

/// ZASSET.ZHDRTYPE —— HDR 类型。
pub const HDR_TYPE: &[(i64, &str)] = &[
    (0, "非 HDR"),
    (3, "多帧合成 HDR"),
    (4, "Dolby Vision / HLG"),
    (5, "Smart HDR"),
    (6, "Gain Map HDR"),
    (10, "Ultra HDR"),
];

/// ZASSET.ZDEPTHTYPE —— 景深（人像模式）。
pub const DEPTH_TYPE: &[(i64, &str)] = &[(0, "无景深数据"), (1, "人像模式（含深度层）")];

/// ZASSET.ZPLAYBACKSTYLE —— 播放样式。
pub const PLAYBACK_STYLE: &[(i64, &str)] = &[
    (0, "不支持"),
    (1, "静态图"),
    (2, "GIF 动图"),
    (3, "实况照片"),
    (4, "常规视频"),
    (5, "循环播放视频"),
];

/// ZASSET.ZPLAYBACKVARIATION —— 实况特效。
pub const PLAYBACK_VARIATION: &[(i64, &str)] = &[
    (0, "原始实况"),
    (1, "循环（Loop）"),
    (2, "来回（Bounce）"),
    (3, "长曝光（Long Exposure）"),
];

/// ZASSET.ZBUNDLESCOPE —— 图库作用域。
pub const BUNDLE_SCOPE: &[(i64, &str)] = &[
    (0, "主个人图库"),
    (1, "iCloud 共享相册"),
    (2, "系统内部预览"),
    (3, "Syndication「与你共享」"),
    (4, "iCloud 共享图库"),
    (5, "iCloud 共享图库"),
];

/// ZGENERICALBUM.ZKIND —— 相册类型。
pub const ALBUM_KIND: &[(i64, &str)] = &[
    (2, "机主自建相册"),
    (1500, "Photo Stream 相册"),
    (1505, "iCloud 共享相册"),
    (1506, "导入会话批次"),
    (1507, "用户相册根"),
    (1508, "用户相册根"),
    (1510, "「与你共享」会话虚拟相册"),
    (1552, "待处理/同步中相册"),
    (3998, "相册文件夹"),
    (3999, "相册文件夹"),
    (4000, "相册文件夹"),
    (1600, "智能相册：全部照片"),
    (1602, "智能相册：个人收藏"),
    (1605, "智能相册：视频"),
    (1606, "智能相册：自拍"),
    (1609, "智能相册：截屏"),
    (1611, "智能相册：全景"),
    (1612, "智能相册：已隐藏"),
    (1613, "智能相册：最近删除"),
    (1614, "智能相册：连拍"),
    (1615, "智能相册：慢动作"),
    (1617, "智能相册：实况照片"),
    (1618, "智能相册：动画"),
    (1619, "智能相册：长曝光"),
    (1620, "智能相册：无法上传"),
    (1621, "智能相册：RAW"),
    (1622, "智能相册：电影效果"),
    (1623, "智能相册：ProRes"),
    (1628, "智能相册：重复项目/空间视频"),
];

/// ZPERSON/ZDETECTEDFACE.ZDETECTIONTYPE —— 检测物种（iOS 17 起宠物独立建档）。
pub const DETECTION_TYPE: &[(i64, &str)] = &[(1, "人类"), (2, "动物"), (3, "狗"), (4, "猫")];

/// ZPERSON.ZVERIFIEDTYPE —— 人物身份确认状态。
pub const VERIFIED_TYPE: &[(i64, &str)] = &[
    (0, "未确认（算法自动聚类）"),
    (1, "机主已确认（命名/合并/关联通讯录）"),
    (2, "iCloud 图谱确认"),
    (-1, "被用户标记「不是此人」"),
    (-2, "被用户标记「不是此人」"),
];

/// ZPERSON.ZTYPE —— 人物聚类类型。
pub const PERSON_TYPE: &[(i64, &str)] = &[
    (0, "普通聚类人物"),
    (1, "机主置顶的重要人物"),
    (-1, "被机主隐藏的人物"),
];

/// ZDETECTEDFACE.ZGENDERTYPE —— 性别。
pub const GENDER_TYPE: &[(i64, &str)] = &[(0, "未知"), (1, "男性"), (2, "女性")];

/// ZDETECTEDFACE.ZAGETYPE —— 年龄段。
pub const AGE_TYPE: &[(i64, &str)] = &[
    (0, "未知"),
    (1, "婴儿"),
    (2, "儿童"),
    (3, "青少年/青年"),
    (4, "成年人"),
    (5, "老年人"),
];

/// ZDETECTEDFACE.ZEYESSTATE —— 睁闭眼。
pub const EYES_STATE: &[(i64, &str)] = &[(0, "未知"), (1, "闭眼"), (2, "睁眼")];

/// ZDETECTEDFACE.ZGLASSESTYPE —— 眼镜状态。
pub const GLASSES_TYPE: &[(i64, &str)] = &[
    (0, "未知"),
    (1, "佩戴普通眼镜"),
    (2, "佩戴太阳镜（特征遮挡）"),
    (3, "未戴眼镜"),
];

/// ZINTERNALRESOURCE.ZRESOURCETYPE —— 衍生资源角色。
pub const RESOURCE_TYPE: &[(i64, &str)] = &[
    (0, "照片（主图/衍生预览）"),
    (1, "视频（主文件/转码流）"),
    (2, "音频"),
    (3, "实况照片伴生视频"),
    (4, "缩略图/旧版衍生"),
    (5, "编辑调整参数（Adjustments.plist）"),
    (6, "编辑后全尺寸渲染图"),
    (7, "原始调整备份"),
    (8, "基准渲染层"),
    (9, "编辑后全尺寸渲染视频"),
    (10, "超广角画框外超采原图"),
    (11, "超采伴生视频"),
    (13, "元数据/XMP/AAE 侧车"),
    (14, "元数据/XMP/AAE 侧车"),
];

/// ZINTERNALRESOURCE.ZLOCALAVAILABILITY —— 本地在位。
pub const LOCAL_AVAILABILITY: &[(i64, &str)] = &[
    (1, "本地磁盘存在（可直接提取）"),
    (-1, "本地无此文件（仅云端）"),
    (-32768, "本地不可用/损坏占位"),
];

/// ZINTERNALRESOURCE.ZREMOTEAVAILABILITY —— 云端在位。
pub const REMOTE_AVAILABILITY: &[(i64, &str)] = &[(0, "未上传 iCloud"), (1, "已备份于 iCloud")];

/// 各类枚举的统一查询入口。
pub fn enum_label(table: &[(i64, &'static str)], value: i64) -> String {
    label(table, value).unwrap_or("").to_string()
}

/// 相册类型代码转中文标签。
pub fn album_kind_label(kind: i64) -> String {
    match kind {
        2 => "机主自建相册 (2)".to_string(),
        1500 => "Photo Stream 相册 (1500)".to_string(),
        1505 => "iCloud 共享相册 (1505)".to_string(),
        1506 => "导入会话批次 (1506)".to_string(),
        1507 | 1508 => "用户相册根 (1507/1508)".to_string(),
        1510 => "「与你共享」虚拟相册 (1510)".to_string(),
        1552 => "待处理/同步中相册 (1552)".to_string(),
        1600 => "智能相册：全部照片 (1600)".to_string(),
        1602 => "智能相册：个人收藏 (1602)".to_string(),
        1605 => "智能相册：视频 (1605)".to_string(),
        1606 => "智能相册：自拍 (1606)".to_string(),
        1609 => "智能相册：截屏 (1609)".to_string(),
        1611 => "智能相册：全景 (1611)".to_string(),
        1612 => "智能相册：已隐藏 (1612)".to_string(),
        1613 => "智能相册：最近删除 (1613)".to_string(),
        1614 => "智能相册：连拍 (1614)".to_string(),
        1615 => "智能相册：慢动作 (1615)".to_string(),
        1617 => "智能相册：实况照片 (1617)".to_string(),
        1618 => "智能相册：动画 (1618)".to_string(),
        1619 => "智能相册：长曝光 (1619)".to_string(),
        1620 => "智能相册：无法上传 (1620)".to_string(),
        1621 => "智能相册：RAW (1621)".to_string(),
        1622 => "智能相册：电影效果 (1622)".to_string(),
        1623 => "智能相册：ProRes (1623)".to_string(),
        1628 => "智能相册：空间视频/重复项 (1628)".to_string(),
        3998..=4000 => format!("相册文件夹 ({kind})"),
        _ => {
            if let Some(l) = label(ALBUM_KIND, kind) {
                format!("{l} ({kind})")
            } else {
                format!("系统相册 ({kind})")
            }
        }
    }
}

/// 人物确认状态（ZVERIFIEDTYPE）标签。
pub fn verified_type_label(verified: i64) -> &'static str {
    match verified {
        1 => "用户手动确认",
        2 => "iCloud 图谱确认",
        -1 | -2 => "标记非此人",
        _ => "算法自动聚类",
    }
}

/// 人脸检测物种（ZDETECTIONTYPE）标签。
pub fn detection_type_label(detection: i64) -> &'static str {
    match detection {
        1 => "人物",
        2 => "动物",
        3 => "宠物狗",
        4 => "宠物猫",
        _ => "生物/物体",
    }
}
