"""生成 iOS 备份数据库 fixture（tests/fixtures/ios_backup/）。

按逆向口径（IOSBASEINFO_REVERSE.md）构造标准 schema 的最小数据库与 plist，
用于 Rust 解析器的单元/集成测试。生成后为静态文件入库。
"""
import os, sqlite3, plistlib, io, sys, random, datetime

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

ROOT = os.path.join(os.path.dirname(__file__), "fixtures", "ios_backup")
os.makedirs(ROOT, exist_ok=True)

APPLE = 978307200  # 2001-01-01 UTC


def ts(s):
    """ISO 字符串 -> Apple epoch 秒"""
    dt = datetime.datetime.fromisoformat(s)
    return dt.timestamp() - APPLE


# ---------- sms.db ----------
sms = os.path.join(ROOT, "sms.db")
os.path.exists(sms) and os.remove(sms)
con = sqlite3.connect(sms)
con.executescript("""
CREATE TABLE message (ROWID INTEGER PRIMARY KEY, guid TEXT, text TEXT, service TEXT,
 handle_id INTEGER, subject TEXT, date REAL, date_read REAL, date_delivered REAL,
 is_from_me INTEGER, is_read INTEGER, is_sent INTEGER, cache_has_attachments INTEGER);
CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, service TEXT, country TEXT);
CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid TEXT, style INTEGER, chat_identifier TEXT);
CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY, guid TEXT, filename TEXT, mime_type TEXT, total_bytes INTEGER);
CREATE TABLE message_attachment_join (message_id INTEGER, attachment_id INTEGER);
""")
con.execute("INSERT INTO handle VALUES (1, '+8613800138000', 'SMS', 'CN')")
con.execute("INSERT INTO handle VALUES (2, '+8613900139000', 'iMessage', 'CN')")
con.execute("INSERT INTO chat VALUES (1, 'chat1', 45, '+8613800138000')")
rows = [
    (1, "g1", "明天见", "SMS", 1, None, ts("2026-07-01 10:00:00"), None, None, 1, 1, 1, 0),
    (2, "g2", "好的", "SMS", 1, None, ts("2026-07-01 10:05:00"), ts("2026-07-01 10:04:00"), None, 0, 1, 0, 0),
    (3, "g3", "发票已发邮箱", "iMessage", 2, None, ts("2026-07-02 09:00:00"), None, None, 0, 1, 0, 1),
]
con.executemany("INSERT INTO message VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)", rows)
con.execute("INSERT INTO attachment VALUES (1, 'a1', 'Domains/Media/DCIM/101APPLE/IMG_0001.JPG', 'image/jpeg', 204800)")
con.execute("INSERT INTO message_attachment_join VALUES (3, 1)")
con.execute("INSERT INTO chat_message_join VALUES (1,1)")
con.execute("INSERT INTO chat_message_join VALUES (1,2)")
con.commit(); con.close()
print("sms.db ok")

# ---------- AddressBook.sqlitedb ----------
ab = os.path.join(ROOT, "AddressBook.sqlitedb")
os.path.exists(ab) and os.remove(ab)
con = sqlite3.connect(ab)
con.executescript("""
CREATE TABLE ABPerson (ROWID INTEGER PRIMARY KEY, First TEXT, Last TEXT, Middle TEXT,
 Organization TEXT, Note TEXT, Created REAL, ModificationDate REAL, ExternalIdentifier TEXT);
CREATE TABLE ABMultiValue (ROWID INTEGER PRIMARY KEY, record_id INTEGER, property INTEGER,
 value TEXT, label INTEGER, uid INTEGER);
CREATE TABLE ABMultiValueLabel (ROWID INTEGER PRIMARY KEY, property INTEGER, label TEXT);
CREATE TABLE ABPersonFullTextSearch_content (rowid INTEGER, c0First TEXT, c1Last TEXT, c16Phone TEXT);
""")
con.execute("INSERT INTO ABPerson VALUES (1, '三', '张', NULL, '某公司', '客户', 700000000.0, 710000000.0, 'x1')")
con.execute("INSERT INTO ABPerson VALUES (2, 'Min', 'Li', 'A', NULL, NULL, 705000000.0, 715000000.5, 'x2')")
# property: 3=电话, 4=email（ABMultiValue 惯例 property 3=phone 4=email；火眼按 label 值 1/2 判断，这里两种都造）
con.execute("INSERT INTO ABMultiValue VALUES (1, 1, 3, '+8613800138000', 1, 1)")
con.execute("INSERT INTO ABMultiValue VALUES (2, 1, 4, 'zhang@ex.com', 2, 2)")
con.execute("INSERT INTO ABMultiValue VALUES (3, 2, 3, '+8613900139000', 1, 3)")
con.execute("INSERT INTO ABMultiValueLabel VALUES (1, 3, '工作')")
con.execute("INSERT INTO ABPersonFullTextSearch_content VALUES (1, '三', '张', '+8613800138000')")
con.commit(); con.close()
print("AddressBook.sqlitedb ok")

# ---------- CallHistory.storedata ----------
ch = os.path.join(ROOT, "CallHistory.storedata")
os.path.exists(ch) and os.remove(ch)
con = sqlite3.connect(ch)
con.executescript("""
CREATE TABLE ZCALLRECORD (Z_PK INTEGER PRIMARY KEY, ZADDRESS TEXT, ZDATE REAL,
 ZDURATION REAL, ZORIGINATED INTEGER, ZANSWERED INTEGER, ZCALLTYPE INTEGER, ZISO_COUNTRY_CODE TEXT);
CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
""")
con.executemany("INSERT INTO ZCALLRECORD VALUES (?,?,?,?,?,?,?,?)", [
    (1, '+8613800138000', ts("2026-07-03 08:00:00"), 120.0, 1, 1, 1, 'CN'),
    (2, '+8613900139000', ts("2026-07-03 09:00:00"), 0.0, 0, 0, 0, 'CN'),
    (3, '+8613700137000', ts("2026-07-04 11:30:00"), 45.5, 1, 1, 2, 'CN'),
])
con.execute("INSERT INTO Z_PRIMARYKEY VALUES (1, 'CallRecord')")
con.commit(); con.close()
print("CallHistory.storedata ok")

# ---------- ledevices ----------
for name, table in [("com.apple.MobileBluetooth.ledevices.paired.db", "PairedDevices"),
                    ("com.apple.MobileBluetooth.ledevices.other.db", "OtherDevices")]:
    p = os.path.join(ROOT, name)
    os.path.exists(p) and os.remove(p)
    con = sqlite3.connect(p)
    if table == "PairedDevices":
        con.executescript("""
        CREATE TABLE PairedDevices (Uuid TEXT, Name TEXT, NameOrigin INTEGER,
         Address TEXT, ResolvedAddress TEXT, LastSeenTime REAL, LastConnectionTime REAL);
        """)
        con.execute("INSERT INTO PairedDevices VALUES ('u1','特斯拉-Model3',1,'AA:BB:CC:DD:EE:FF',NULL,740000000.0,745000000.0)")
    else:
        con.executescript("""
        CREATE TABLE OtherDevices (Uuid TEXT, Name TEXT, NameOrigin INTEGER, Address TEXT, LastSeenTime REAL);
        """)
        con.execute("INSERT INTO OtherDevices VALUES ('u2','iPhone of Bob',1,'11:22:33:44:55:66',730000000.0)")
    con.commit(); con.close()
print("ledevices ok")

# ---------- Accounts3.sqlite ----------
ac = os.path.join(ROOT, "Accounts3.sqlite")
os.path.exists(ac) and os.remove(ac)
con = sqlite3.connect(ac)
con.executescript("""
CREATE TABLE ZACCOUNT (Z_PK INTEGER PRIMARY KEY, ZACCOUNTTYPEDESCRIPTION TEXT, ZUSERNAME TEXT,
 ZDATE REAL, ZACCOUNTDESCRIPTION TEXT, ZIDENTIFIER TEXT, ZOWNINGBUNDLEID TEXT);
""")
con.execute("INSERT INTO ZACCOUNT VALUES (1,'iCloud','test@icloud.com',690000000.0,'iCloud账户','id1','com.apple')") 
con.execute("INSERT INTO ZACCOUNT VALUES (2,'QQ邮箱','12345@qq.com',695000000.0,'','id2','com.apple.mobilemail')")
con.commit(); con.close()
print("Accounts3.sqlite ok")

# ---------- com.apple.wifi.plist（binary plist） ----------
wifi = {
    "ListOfPreferredNetworks": {
        "HomeWiFi": {
            "SSIDString": "HomeWiFi",
            "AutoJoin": True,
            "LastJoined": datetime.datetime(2026, 7, 5, 12, 0, 0),
            "SecurityMode": 3,
        },
        "Office_5G": {
            "SSIDString": "Office_5G",
            "AutoJoin": False,
            "LastJoined": datetime.datetime(2026, 6, 1, 9, 30, 0),
        },
    }
}
with open(os.path.join(ROOT, "com.apple.wifi.plist"), "wb") as f:
    plistlib.dump(wifi, f, fmt=plistlib.FMT_BINARY)
print("wifi.plist ok")
print("\nfixture 完成 ->", ROOT)
