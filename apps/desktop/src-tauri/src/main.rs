//! Muster 点将台桌面壳(P1-07/08/09 纵切):
//! 频道消息 → E2 路由决策(徽章数据)→ A2 流式模型调用 → A9 审计落库。
//!
//! 形态说明:B1 Runner / Worktree / Diff 落地后,`send_message` 将改为驱动
//! 完整任务;当前是"真实决策 + 真实流式 + 真实审计"的最小闭环,不含工具执行,
//! 因此审计只写 route.decide / model.call 两类**如实发生**的事件
//! (run.start 的 ReplayRefs 要求仓库快照等硬引用,聊天形态尚无,不伪造)。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::Serialize;
use tauri::{Emitter, State};

use muster_audit::{
    actor_first_seen, capsules, day_throughput, distinct_runs, downgrades_zh, drill_report,
    forgeable, pending_approval_list, pending_approvals, recent_events, recent_events_of, roster,
    Actor, AuditStore, ContentHash, EgressBytes, EventBody, NewEvent, Scope,
};
use muster_provider::{ChatMessage, ChatRequest, Locality, ProviderRegistry, StreamEvent};
use muster_route::{
    LabelOrigin, LabelSource, OrgPolicy, RoutePlan, RouteRequest, Router, SessionRatchet, Sensitivity,
};
use muster_runner::{run_task, RunnerConfig, RunnerError, RunnerEvent, TaskSpec};

const POLICY_VERSION: &str = "policy-v1";
const AGENT_BADGE: &str = "A-007";
/// 聊天模式的提示词。
///
/// **必须写清"我现在没有工具"以及"怎样才有"**:此模式确实读不到也改不了文件,
/// 若只说"我无法访问文件系统",用户会以为是产品缺陷——实际上换个按钮就能做到。
/// 让 Agent 自己指路,比在 UI 上堆说明有效得多。
const SYSTEM_PROMPT: &str = r#"你是 Muster 点将台的协作 Agent(工牌 A-007)。用中文回答,简洁、直接。

当前是**对话模式**:你没有任何文件或命令工具,读不到仓库内容,也改不了代码。
当用户要求你查看、修改文件或执行命令时,不要只说自己无法访问,而要明确告诉他:
「这需要任务模式——点输入框右侧的『▶ 任务』按钮重发这条需求,我就能在隔离分支上
真正读写代码,完成后产出 diff 交你审批。」然后可以顺带给出你的思路或建议方案。"#;

// ---------------------------------------------------------------- 演示编制

#[derive(Clone, Serialize)]
struct ChannelInfo {
    id: String,
    /// 频道显示名(v4:# 前缀由前端加)。
    name: String,
    team_id: String,
    team: String,
    level: Sensitivity,
    /// 徽章悬浮:密级从哪来(与 label_sources 保持一致)。
    level_note: String,
    desc: String,
    /// 个人空间伪频道:不出现在团队树,只服务「我的工作台」。
    personal: bool,
}

fn ch(
    id: &str,
    name: &str,
    team_id: &str,
    team: &str,
    level: Sensitivity,
    level_note: &str,
    desc: &str,
) -> ChannelInfo {
    ChannelInfo {
        id: id.into(),
        name: name.into(),
        team_id: team_id.into(),
        team: team.into(),
        level,
        level_note: level_note.into(),
        desc: desc.into(),
        personal: false,
    }
}

/// v4 编制:三团队六频道 + 个人空间。密级与 label_sources 一一对应。
fn demo_channels() -> Vec<ChannelInfo> {
    let mut v = vec![
        ch("general", "general", "platform", "平台组", Sensitivity::Open,
            "未贴标签,默认 open(产品决策:未标注可走云端)", "公开讨论,允许云端模型"),
        ch("platform", "platform", "platform", "平台组", Sensitivity::Internal,
            "频道标签 internal(恰在 cloud_max 上限,仍可云端)", "平台组主频道"),
        ch("code-review", "code-review", "platform", "平台组", Sensitivity::Internal,
            "频道标签 internal", "评审与合入讨论"),
        ch("payments", "payments", "pay", "支付组", Sensitivity::Internal,
            "频道标签 internal", "支付业务频道"),
        ch("release-train", "release-train", "pay", "支付组", Sensitivity::Internal,
            "频道标签 internal", "发布列车"),
        ch("sec-ops", "sec-ops", "sec", "安全组", Sensitivity::Restricted,
            "频道标签 restricted——硬编码不变量:永不上云;本地不可用即拒绝", "安全运营(仅本地)"),
    ];
    v.push(ChannelInfo {
        id: "personal".into(),
        name: "私有会话".into(),
        team_id: "personal".into(),
        team: "个人".into(),
        level: Sensitivity::Open,
        level_note: "个人空间默认 open;引用 restricted 资源会被会话棘轮抬升(E3)".into(),
        desc: "与小七的私有会话,默认不进团队".into(),
        personal: true,
    });
    v
}

/// 频道的密级标签来源。
///
/// **按已解析频道的 level 生成,不按 id 硬编码。** 曾经这里是按 demo 频道 id
/// 打表的,于是服务端来的频道(id 不在表里)落进兜底分支、拿到空标签——
/// 一个 restricted 的服务端频道会被当成 open 来路由。**那是密级泄漏**,
/// 而且外部完全看不出来:界面上密级徽章显示得好好的,路由却当它不存在。
///
/// `Open` 不产生标签是**有意的**:E1 里"未标注即 open"是默认值而非断言,
/// 加一条 open 标签会让 `deciders`(为什么是这个密级)里多出一条无信息的来源。
fn label_sources(channel: &ChannelInfo) -> Vec<LabelSource> {
    match channel.level {
        Sensitivity::Open => vec![],
        level => vec![LabelSource::new(
            LabelOrigin::Channel,
            level,
            format!("channel:{}", channel.id),
        )],
    }
}

// ---------------------------------------------------------------- 状态

// ---------------------------------------------------------------- C1 会话持久化
//
// 桌面本地状态库,与审计库**刻意分离**:审计是证据层(只存哈希,append-only,
// 防篡改);这里是会话正文的 run 存储侧,带自己的密级语义,可清除。

#[derive(Serialize, Clone)]
struct StoredMsg {
    channel_id: String,
    /// 所属线程。老库回填成 `main:<channel_id>`
    #[serde(default)]
    thread_id: String,
    role: String,
    text: String,
    run_id: Option<String>,
    status: String,
    ts_ms: i64,
}

/// 主线程的 id。老库回填用的也是这个式子——**只此一处**,
/// 散着写就会有一天两边算出不同的字符串。
#[derive(Serialize)]
struct ForkResult {
    thread_id: String,
    forked_from: String,
    /// 继承了多少条
    inherited: usize,
    /// 被切掉的那条提问,原样交回输入框等你改。
    /// **这才是这个功能好用的地方**——不是"复制一份对话"。
    reopened_prompt: Option<String>,
}

#[derive(Serialize, Clone)]
struct ThreadInfo {
    id: String,
    title: String,
    forked_from: Option<String>,
    inherited_count: usize,
    persistence: String,
    created_ms: i64,
}

/// 够用的唯一 id。不引 uuid crate:这个值只在本机 state.db 里做主键,
/// 时间戳 + 计数器足够,而多一个依赖要多验一次供应链。
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", now_ms(), SEQ.fetch_add(1, Ordering::Relaxed))
}

fn main_thread(channel_id: &str) -> String {
    format!("main:{channel_id}")
}

impl fork::ForkItem for StoredMsg {
    fn is_user(&self) -> bool {
        self.role == "user"
    }
}

struct StateStore {
    conn: rusqlite::Connection,
}

impl StateStore {
    fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL,
                role TEXT NOT NULL,
                text TEXT NOT NULL,
                run_id TEXT,
                status TEXT NOT NULL,
                ts_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_msg_chan ON messages(channel_id, id);

            -- 会话线程(照抄 codex 的 thread)。每个频道默认有一条主线程,
            -- 分叉出来的是**新线程,新 id**——父线程一个字节都不动。
            CREATE TABLE IF NOT EXISTS thread(
                id              TEXT PRIMARY KEY,
                channel_id      TEXT NOT NULL,
                title           TEXT NOT NULL,
                -- 从哪条线程分出来的。NULL = 主线程
                forked_from     TEXT,
                -- 从父线程继承了多少条(仅 referenced 用)
                inherited_count INTEGER NOT NULL DEFAULT 0,
                -- 'copied' | 'referenced',见 fork::Persistence
                persistence     TEXT NOT NULL DEFAULT 'copied',
                created_ms      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_thread_chan ON thread(channel_id, created_ms);

            -- E3 会话棘轮的持久化。**跨重启保留**是语义的一部分,不是优化:
            -- 底线只升不降,重启就归零的话,关一次应用就等于降密。
            CREATE TABLE IF NOT EXISTS session_ratchet(
                session_id TEXT PRIMARY KEY,
                state      TEXT NOT NULL,  -- SessionRatchet 的 JSON
                updated_ms INTEGER NOT NULL
            );",
        )?;
        // 老库没有 thread_id 列。**加列 + 回填,不是重建表**——
        // 重建会丢掉 rowid 顺序,而消息的先后全靠它
        let has = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='thread_id'")?
            .exists([])?;
        if !has {
            conn.execute_batch(
                "ALTER TABLE messages ADD COLUMN thread_id TEXT;
                 UPDATE messages SET thread_id = 'main:' || channel_id WHERE thread_id IS NULL;
                 CREATE INDEX IF NOT EXISTS idx_msg_thread ON messages(thread_id, id);",
            )?;
        }
        Ok(Self { conn })
    }

    /// 状态库是便利层,写失败降级为日志,不阻断任务(与审计的 fail-closed 相反,刻意)。
    fn insert(&self, channel_id: &str, role: &str, text: &str, run_id: Option<&str>, status: &str) {
        self.insert_in(&main_thread(channel_id), channel_id, role, text, run_id, status)
    }

    /// 写进指定线程。分叉出来的线程走这条。
    fn insert_in(
        &self,
        thread_id: &str,
        channel_id: &str,
        role: &str,
        text: &str,
        run_id: Option<&str>,
        status: &str,
    ) {
        if let Err(e) = self.conn.execute(
            "INSERT INTO messages(channel_id, thread_id, role, text, run_id, status, ts_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![channel_id, thread_id, role, text, run_id, status, now_ms() as i64],
        ) {
            eprintln!("state 持久化失败(忽略):{e}");
        }
    }

    /// 某条线程的完整历史,**含从父线程继承的部分**。
    ///
    /// `referenced` 模式下前缀不在自己表里,要顺着 `forked_from` 往上拼。
    /// 递归有上限:分叉链坏掉(父被清、或有环)时不能把栈吃穿——
    /// 状态库是便利层,它坏了不该拖垮应用。
    fn thread_history(&self, thread_id: &str) -> rusqlite::Result<Vec<StoredMsg>> {
        self.thread_history_depth(thread_id, 0)
    }

    fn thread_history_depth(&self, thread_id: &str, depth: u32) -> rusqlite::Result<Vec<StoredMsg>> {
        const MAX_DEPTH: u32 = 32;
        let mut out = Vec::new();
        if depth < MAX_DEPTH {
            let parent: Option<(String, i64, String)> = self
                .conn
                .query_row(
                    "SELECT forked_from, inherited_count, persistence FROM thread WHERE id = ?1",
                    rusqlite::params![thread_id],
                    |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?, r.get(2)?)),
                )
                .ok()
                .filter(|(f, _, p)| !f.is_empty() && p == "referenced");
            if let Some((from, n, _)) = parent {
                let mut inherited = self.thread_history_depth(&from, depth + 1)?;
                inherited.truncate(n as usize);
                out = inherited;
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, COALESCE(thread_id,''), role, text, run_id, status, ts_ms
             FROM messages WHERE thread_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![thread_id], |r| {
            Ok(StoredMsg {
                channel_id: r.get(0)?,
                thread_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                run_id: r.get(4)?,
                status: r.get(5)?,
                ts_ms: r.get(6)?,
            })
        })?;
        for m in rows {
            out.push(m?);
        }
        Ok(out)
    }

    /// 分叉。返回 (新线程 id, 继承条数, 被切掉那条提问的正文)。
    ///
    /// 照抄 codex:新线程**新 id**,父线程一个字节都不动;切点只能落在
    /// 用户消息边界上;越界时的三种行为见 [`fork::truncate_before_nth_user_message`]。
    ///
    /// 被切掉的那条提问要**原样带回给调用方**——codex 那边 Esc Esc 之后
    /// 提问会重新出现在输入框里等你改,那才是这个功能好用的地方,
    /// 而不是"复制一份对话"。
    /// 建一条线程行。跨频道 fork 与同频道 fork 共用它——
    /// 两份 INSERT 迟早会漂,而漂的表现是"某种 fork 出来的线程读不出继承关系"。
    #[allow(clippy::too_many_arguments)]
    fn create_thread(
        &self,
        id: &str,
        channel_id: &str,
        title: &str,
        forked_from: Option<&str>,
        inherited: usize,
        persistence: fork::Persistence,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO thread(id, channel_id, title, forked_from, inherited_count, persistence, created_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                id, channel_id, title, forked_from, inherited as i64,
                persistence.as_str(), now_ms() as i64
            ],
        )?;
        Ok(())
    }

    fn fork_thread(
        &self,
        src_thread: &str,
        channel_id: &str,
        nth_user_message: usize,
        persistence: fork::Persistence,
    ) -> rusqlite::Result<(String, usize, Option<String>)> {
        let history = self.thread_history(src_thread)?;
        let keep = fork::truncate_before_nth_user_message(&history, nth_user_message);
        let reopened = history.get(keep).filter(|m| m.role == "user").map(|m| m.text.clone());

        let new_id = format!("fork:{}", uuid_like());
        let title = format!("分叉自 {} 第 {} 问", src_thread, nth_user_message + 1);
        self.create_thread(&new_id, channel_id, &title, Some(src_thread), keep, persistence)?;

        // Copied:把前缀抄进新线程。Referenced:什么都不抄,读的时候拼。
        if persistence == fork::Persistence::Copied {
            for m in history.iter().take(keep) {
                self.conn.execute(
                    "INSERT INTO messages(channel_id, thread_id, role, text, run_id, status, ts_ms)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        &m.channel_id, &new_id, &m.role, &m.text, &m.run_id, &m.status, m.ts_ms
                    ],
                )?;
            }
        }
        Ok((new_id, keep, reopened))
    }

    fn threads_of(&self, channel_id: &str) -> rusqlite::Result<Vec<ThreadInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, forked_from, inherited_count, persistence, created_ms
             FROM thread WHERE channel_id = ?1 ORDER BY created_ms ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![channel_id], |r| {
            Ok(ThreadInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                forked_from: r.get(2)?,
                inherited_count: r.get::<_, i64>(3)? as usize,
                persistence: r.get(4)?,
                created_ms: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// 读会话棘轮。**读不出来一律当新会话**——但这里有个取舍要说清楚:
    ///
    /// 解析失败时回到 Open,等于一次静默降密。之所以仍这么做,是因为
    /// 另一条路更糟:解析失败就拒绝一切路由,那么一个损坏的字节会让人
    /// 彻底用不了这个频道,而他根本不知道为什么。**取舍写在这里,不藏着。**
    ///
    /// 真正的防线是:抬升的那一刻已经进了审计链(`session.lock.raise`),
    /// 链是 append-only 的,棘轮这张表坏了不影响它。
    fn load_ratchet(&self, session_id: &str) -> SessionRatchet {
        self.conn
            .query_row(
                "SELECT state FROM session_ratchet WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// 存棘轮。**写失败要报错,不能像别的状态那样降级为日志**——
    /// 抬升没存住,下一轮就回到低密级,那正是"只升不降"要防的事。
    fn save_ratchet(&self, session_id: &str, r: &SessionRatchet) -> Result<(), String> {
        let j = serde_json::to_string(r).map_err(|e| format!("棘轮序列化失败:{e}"))?;
        self.conn
            .execute(
                "INSERT INTO session_ratchet(session_id, state, updated_ms) VALUES(?1,?2,?3)
                 ON CONFLICT(session_id) DO UPDATE SET state=?2, updated_ms=?3",
                rusqlite::params![session_id, j, now_ms() as i64],
            )
            .map(|_| ())
            .map_err(|e| format!("棘轮写入失败:{e}"))
    }

    fn bulk(&self, limit: u32) -> rusqlite::Result<Vec<StoredMsg>> {
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, COALESCE(thread_id,''), role, text, run_id, status, ts_ms
             FROM messages ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |r| {
            Ok(StoredMsg {
                channel_id: r.get(0)?,
                thread_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                run_id: r.get(4)?,
                status: r.get(5)?,
                ts_ms: r.get(6)?,
            })
        })?;
        let mut out: Vec<StoredMsg> = rows.collect::<Result<_, _>>()?;
        out.reverse(); // 倒查取最近 N 条,再转回时间正序
        Ok(out)
    }

    fn stats(&self) -> rusqlite::Result<(u64, u64, Option<u64>)> {
        self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(text)),0), MIN(ts_ms) FROM messages",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                ))
            },
        )
    }

    /// 按条件取出正文。`None` 条件 = 不限。
    fn select(&self, sel: &Selector) -> rusqlite::Result<Vec<StoredMsg>> {
        let (where_sql, arg): (&str, Option<String>) = match sel {
            Selector::All => ("1=1", None),
            Selector::Channel(c) => ("channel_id = ?1", Some(c.clone())),
            Selector::Run(r) => ("run_id = ?1", Some(r.clone())),
            Selector::OlderThan(ms) => ("ts_ms < ?1", Some(ms.to_string())),
        };
        let sql = format!(
            "SELECT channel_id, COALESCE(thread_id,''), role, text, run_id, status, ts_ms
             FROM messages WHERE {where_sql} ORDER BY id ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(StoredMsg {
                channel_id: r.get(0)?,
                thread_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                run_id: r.get(4)?,
                status: r.get(5)?,
                ts_ms: r.get(6)?,
            })
        };
        let rows: Vec<StoredMsg> = match (&arg, sel) {
            (None, _) => stmt.query_map([], map)?.collect::<Result<_, _>>()?,
            (Some(_), Selector::OlderThan(ms)) => {
                stmt.query_map(rusqlite::params![*ms as i64], map)?.collect::<Result<_, _>>()?
            }
            (Some(a), _) => stmt.query_map(rusqlite::params![a], map)?.collect::<Result<_, _>>()?,
        };
        Ok(rows)
    }

    /// 删除并 VACUUM。**不 VACUUM 等于没删**:SQLite 只是把页标记为空闲,
    /// 正文仍留在文件里,`strings` 一读就出来——对一个以"删得掉"为卖点的
    /// 功能来说,那是假删。
    fn purge(&self, sel: &Selector) -> rusqlite::Result<u64> {
        let n = match sel {
            Selector::All => self.conn.execute("DELETE FROM messages", [])?,
            Selector::Channel(c) => {
                self.conn.execute("DELETE FROM messages WHERE channel_id = ?1", rusqlite::params![c])?
            }
            Selector::Run(r) => {
                self.conn.execute("DELETE FROM messages WHERE run_id = ?1", rusqlite::params![r])?
            }
            Selector::OlderThan(ms) => self
                .conn
                .execute("DELETE FROM messages WHERE ts_ms < ?1", rusqlite::params![*ms as i64])?,
        };
        if n > 0 {
            self.conn.execute_batch("VACUUM;")?;
        }
        Ok(n as u64)
    }
}

/// 正文的选取口径。同一套口径同时服务导出与删除——**能导出什么就能删什么**,
/// 免得出现"看得见却删不掉"的角落。
#[derive(Debug, Clone)]
enum Selector {
    All,
    Channel(String),
    Run(String),
    OlderThan(u64),
}

impl Selector {
    /// 进审计的口径串(不含正文)。
    fn tag(&self, keep_days: Option<u32>) -> String {
        match self {
            Selector::All => "all".into(),
            Selector::Channel(c) => format!("channel:{c}"),
            Selector::Run(r) => format!("run:{r}"),
            Selector::OlderThan(_) => match keep_days {
                Some(d) => format!("retention:{d}d"),
                None => "older_than".into(),
            },
        }
    }

    fn parse(kind: &str, value: Option<String>) -> Result<Self, String> {
        match kind {
            "all" => Ok(Selector::All),
            "channel" => value.map(Selector::Channel).ok_or("缺少频道 id".into()),
            "run" => value.map(Selector::Run).ok_or("缺少 run id".into()),
            "older_than_days" => {
                let d: u64 = value.ok_or("缺少天数")?.parse().map_err(|_| "天数不是数字")?;
                Ok(Selector::OlderThan(now_ms().saturating_sub(d * 86_400_000)))
            }
            other => Err(format!("未知口径 {other}")),
        }
    }
}

/// 正文保留期(天)。**默认不开**:静默销毁用户既有的对话历史是不可接受的,
/// 尤其在升级之后——保留期必须是明确选择的结果,不是升级的副作用。
/// 一旦设了,每次启动自动执行一次,并写 `transcript.purge` 留痕。
fn retention_days() -> Option<u32> {
    std::env::var("MUSTER_TRANSCRIPT_KEEP_DAYS").ok()?.parse().ok().filter(|d| *d > 0)
}

// ---------------------------------------------------------------- P2 当前身份

/// 构建当前会话的主体。
///
/// **单机模式下部署者即组织所有者**——这是产品语义,不是偷懒:一台机器上的
/// 个人本地版没有第二个人来授权,把自己降权毫无意义。
///
/// 但角色可用 `MUSTER_ROLE` 覆盖(owner/admin/group_admin/publisher/approver/
/// member/guest),`MUSTER_ROLE_SCOPE` 指定作用域(缺省全组织)。这不是玩具开关:
/// 权限层若永远返回 allow 就等于没接,**必须能被真实验证**——切成 guest 试一次
/// 就知道拦不拦。演示时也用它展示"不同角色看到的不同结果"。
///
/// 接 OIDC 后此函数改为从 id_token 的 iss+sub 解析(§4.1:不能只用邮箱做主键),
/// 判定逻辑与调用点一行不改。
fn current_principal() -> muster_identity::Principal {
    use muster_identity::{Principal, Role, RoleBinding, Scope};
    let role = match std::env::var("MUSTER_ROLE").unwrap_or_default().as_str() {
        "admin" => Role::OrgAdmin,
        "group_admin" => Role::GroupAdmin,
        "publisher" => Role::Publisher,
        "approver" => Role::Approver,
        "member" => Role::Member,
        "guest" => Role::Guest,
        _ => Role::OrgOwner,
    };
    let scope = match std::env::var("MUSTER_ROLE_SCOPE").ok().filter(|s| !s.is_empty()) {
        Some(g) => Scope::Group(g),
        None => Scope::Org,
    };
    let id = std::env::var("MUSTER_USER").unwrap_or_else(|_| "owner".into());
    Principal::human(id.clone(), id, vec![RoleBinding { role, scope }])
}

/// UI 顶栏的真实身份(替代此前写死的 "Alice · 平台组 · 组长")。
#[derive(Serialize)]
struct WhoAmI {
    id: String,
    display_name: String,
    kind: String,
    role: String,
    role_zh: String,
    scope: String,
    /// 当前身份能做哪些事(UI 据此禁用按钮,而不是点了才报错)。
    can: std::collections::BTreeMap<String, bool>,
}

#[tauri::command]
fn whoami() -> WhoAmI {
    use muster_identity::{Action, Scope};
    let p = current_principal();
    let b = &p.bindings[0];
    let (dir, proh) = (directory(), prohibitions());
    let mut can = std::collections::BTreeMap::new();
    for (key, action, target) in [
        ("create_task", Action::CreateTask, Scope::Org),
        ("approve_merge", Action::ApproveMerge, Scope::Org),
        ("forge_capsule", Action::ForgeCapsule, Scope::Org),
        ("adopt_capsule", Action::AdoptCapsule, Scope::Org),
        ("toggle_drill", Action::ToggleDrill, Scope::Org),
        ("change_policy", Action::ChangePolicy, Scope::Org),
        ("view_audit", Action::ViewAudit, Scope::Org),
    ] {
        can.insert(
            key.to_string(),
            muster_identity::can(&p, &action, &target, &proh, &dir).allowed(),
        );
    }
    WhoAmI {
        id: p.id.clone(),
        display_name: p.display_name.clone(),
        kind: format!("{:?}", p.kind).to_lowercase(),
        role: format!("{:?}", b.role).to_lowercase(),
        role_zh: b.role.zh().to_string(),
        scope: match &b.scope {
            Scope::Org => "全组织".into(),
            Scope::Group(g) => g.clone(),
            Scope::Channel(c) => format!("#{c}"),
        },
        can,
    }
}

/// 频道归属目录:组级授权能否覆盖组内频道,取决于这份事实。
fn directory() -> muster_identity::Directory {
    demo_channels().iter().filter(|c| !c.personal).fold(
        muster_identity::Directory::default(),
        |d, c| d.with_channel(c.id.clone(), c.team.clone()),
    )
}

/// 组织级禁止策略(§4.4 最高优先级)。单机版暂无来源,留空但保留位置——
/// 有了它,"冻结全组织写操作"这类应急开关才有地方落。
fn prohibitions() -> muster_identity::OrgProhibitions {
    muster_identity::OrgProhibitions::default()
}

/// 统一的权限闸门:拒绝时返回**分层理由**,直接可作为 UI 文案。
fn require(
    action: muster_identity::Action,
    target: muster_identity::Scope,
) -> Result<muster_identity::Principal, String> {
    let p = current_principal();
    let d = muster_identity::can(&p, &action, &target, &prohibitions(), &directory());
    if d.allowed() {
        Ok(p)
    } else {
        Err(d.reason_zh())
    }
}

/// 进行中的演习(E6):id + 起始时刻,结束时用 drill_report 聚合窗口。
struct DrillState {
    id: String,
    from_ms: u64,
}

struct Backend {
    router: Arc<Router>,
    /// 组织策略。重建路由器时要沿用它——provider 目录换的是"有哪些通道",
    /// 不是"什么密级能上云"
    policy: OrgPolicy,
    audit: Arc<Mutex<AuditStore>>,
    state: Arc<Mutex<StateStore>>,
    run_seq: Arc<AtomicU64>,
    drill: Arc<Mutex<Option<DrillState>>>,
    /// 已连接的服务端(C1)。`None` = 单机模式,**一切行为与从前完全一致**——
    /// 单机才是这个产品的入口形态,不能因为加了联网模式就让它退化。
    remote: Arc<Mutex<Option<remote::Remote>>>,
}

mod fork;
mod remote;

/// 桌面壳自带的那份 provider 表。**只在单机模式下用。**
///
/// 连上服务端之后一律改用下发的目录:决定 restricted 内容能不能出门的
/// `locality`,不该由被路由的这台机器自己声明。
const BUILTIN_PROVIDERS: &str = include_str!("../../providers.toml");

/// 用一份 provider 配置重建路由器。
///
/// `policy` 从现有 router 继承——目录换的是"有哪些通道",不是"什么密级能上云",
/// 后者是另一件事(`cloud_max`),不该被 provider 目录顺手改掉。
fn router_from_config(
    cfg: muster_provider::RegistryConfig,
    policy: OrgPolicy,
) -> Result<Arc<Router>, String> {
    let registry = ProviderRegistry::from_config(cfg).map_err(|e| {
        // 最常见的失败是环境变量没设。原样带出变量名——
        // "provider 加载失败"这种话让人无从下手
        format!("provider 目录加载失败:{e}")
    })?;
    Ok(Arc::new(Router::from_registry(&registry, policy)))
}

/// 连上服务端后把路由器换成**服务端下发的目录**。
///
/// ## 拿不到目录就不算连上
///
/// 不回退到本地那份。回退看着体贴,实际是给出一条**降级路径**:
/// 谁能让节点连不上服务端,谁就能让它改用本机声明的 locality——
/// 而那正是这次要堵的洞。所以拿不到就报错,由人决定是重试还是断开。
///
/// 目录为空是允许的(组织还没配),但那意味着一个通道都没有,
/// 跑任务会被路由拒绝——那是如实的结果,不是故障。
async fn adopt_remote_catalog(
    state: &State<'_, AppState>,
    r: &remote::Remote,
) -> Result<usize, String> {
    let raw = r.provider_catalog().await?;
    let n = raw.get("providers").and_then(|p| p.as_object()).map(|o| o.len()).unwrap_or(0);
    let cfg: muster_provider::RegistryConfig = serde_json::from_value(raw)
        .map_err(|e| format!("服务端下发的 provider 目录解析失败:{e}"))?;

    let policy = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.policy.clone()
    };
    let router = router_from_config(cfg, policy)?;
    let mut guard = state.0.lock().unwrap();
    guard.as_mut().ok_or("后端未初始化")?.router = router;
    Ok(n)
}

/// 解析频道。**连着服务端时向服务端要,不查本地那份 demo 清单。**
///
/// 两个理由:
/// 1. 服务端的频道 id 与本地 demo 的不是一套(`platform-main` vs `platform`),
///    查本地只会得到"未知频道",而那句话看不出根因;
/// 2. **密级必须来自可信来源**。让前端把 level 传下来,等于让客户端自己声明
///    密级——它可以声明成 open 然后把 restricted 的内容送去云端。
async fn resolve_channel(
    state: &State<'_, AppState>,
    channel_id: &str,
) -> Result<ChannelInfo, String> {
    if let Some(r) = remote_of(state) {
        if !remote::is_personal(channel_id) {
            let c = r.channel(channel_id).await?;
            return Ok(ChannelInfo {
                id: c.id,
                name: c.name,
                team_id: c.team_id.clone(),
                team: c.team_id,
                level: remote::parse_level(&c.level),
                level_note: "频道密级由服务端组织配置决定".into(),
                desc: String::new(),
                personal: false,
            });
        }
    }
    demo_channels()
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or_else(|| format!("未知频道 {channel_id}"))
}


/// 随提问一起发给模型的历史条数与字符上限。
///
/// 两个上限都要:只限条数,几条长回复就能把上下文撑爆;只限字符,
/// 一百条碎消息又会把提示词稀释成噪音。
const CTX_MAX_MSGS: usize = 24;
const CTX_MAX_CHARS: usize = 12_000;

/// 挑出要随提问发出去的历史。**取最近的**,同时受条数与字符两个上限约束。
///
/// ## 为什么滤掉 failed
///
/// 那些是「⚠️ 开流失败」「路由拒绝」这类**系统错误文本**,不是助手说过的话。
/// 把它们喂回去,模型会当成自己上一轮的输出,然后一本正经地接着往下编。
///
/// ## 为什么从后往前取
///
/// 上下文放不下时该丢的是**最早的**。从前往后取会出现"聊了一小时,
/// 模型只记得开头那几句"——比没有上下文更让人困惑。
fn context_window(history: &[StoredMsg], max_msgs: usize, max_chars: usize) -> Vec<&StoredMsg> {
    let mut picked: Vec<&StoredMsg> = Vec::new();
    let mut chars = 0usize;
    for m in history.iter().rev() {
        if m.status == "failed" || m.text.trim().is_empty() {
            continue;
        }
        if picked.len() >= max_msgs {
            break;
        }
        let n = m.text.chars().count();
        if chars + n > max_chars && !picked.is_empty() {
            break;
        }
        chars += n;
        picked.push(m);
    }
    picked.reverse(); // 发出去要时间正序
    picked
}

/// 取某频道的对话历史。**团队频道连着服务端时向服务端要**——
/// 那才是所有人共同看到的那份;只读本地的话,Bob 提问时模型看不见
/// Alice 刚才聊的,"多人共用"就是空话。
async fn channel_history(
    remote: Option<&remote::Remote>,
    store: &Arc<Mutex<StateStore>>,
    channel_id: &str,
) -> Vec<StoredMsg> {
    match remote {
        Some(r) if !remote::is_personal(channel_id) => r
            .messages(channel_id, None)
            .await
            .map(|v| {
                v.into_iter()
                    .map(|m| StoredMsg {
                        channel_id: channel_id.to_string(),
                        thread_id: main_thread(channel_id),
                        role: m.role,
                        text: m.body,
                        run_id: m.run_id,
                        status: "done".into(),
                        ts_ms: m.ts_ms,
                    })
                    .collect()
            })
            // 拉不到历史不该让提问发不出去:退化成无上下文,与从前一致
            .unwrap_or_default(),
        _ => store
            .lock()
            .unwrap()
            .thread_history(&main_thread(channel_id))
            .unwrap_or_default(),
    }
}

/// 过一轮棘轮:算出本轮的决策来源,持久化新状态,抬升则进审计链。
///
/// ## 为什么抬升要单独记一条审计事件
///
/// 只看 `route.decide` 的话,只能在**下一次决策时**发现会话已经被污染了。
/// 这里记的是污染发生的**那一刻**——UI 置灰和徽章解释都指向它。
///
/// ## 为什么写失败要中止本轮
///
/// 棘轮没存住,下一轮就回到低密级——那正是"只升不降"要防的事。
/// 与状态库其他写入"失败降级为日志"的姿态刻意相反。
async fn turn_sources_and_persist(
    store: &Arc<Mutex<StateStore>>,
    audit: &Arc<Mutex<AuditStore>>,
    session_id: &str,
    touched: &[LabelSource],
) -> Result<Vec<LabelSource>, String> {
    let (sources, raise, snapshot) = {
        let st = store.lock().unwrap();
        let mut r = st.load_ratchet(session_id);
        let (sources, raise) = r.turn_sources(touched);
        st.save_ratchet(session_id, &r)?;
        (sources, raise, r)
    };
    let _ = snapshot;
    if let Some(raise) = raise {
        audit
            .lock()
            .unwrap()
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human("me"),
                scope: Scope::default(),
                run_id: None,
                session_id: Some(session_id.to_string()),
                policy_version: Some(POLICY_VERSION.into()),
                label: None,
                locality: None,
                body: EventBody::SessionLockRaise {
                    from_level: raise.from,
                    to_level: raise.to,
                    cause: raise.cause,
                    turn: raise.turn,
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
    }
    Ok(sources)
}

/// `record_msg` 的别名,参数顺序与 `StateStore::insert` 一致,
/// 便于把原地的本地写换成"本地或服务端"而不动参数。
async fn record_msg_body(
    remote: Option<&remote::Remote>,
    store: &Arc<Mutex<StateStore>>,
    thread_id: &str,
    channel_id: &str,
    role: &str,
    text: &str,
    run_id: Option<&str>,
    status: &str,
) -> Result<(), String> {
    record_msg(remote, store, thread_id, channel_id, role, text, run_id, status).await
}

/// 一条消息该记在哪:**个人频道永远本地,团队频道连着服务端时走服务端**。
///
/// 服务端写失败**不静默降级到本地**:那会让人以为发到了团队、其实只存在自己
/// 机器上——比发不出去更糟。失败就返回 Err,让界面显出来。
async fn record_msg(
    remote: Option<&remote::Remote>,
    store: &Arc<Mutex<StateStore>>,
    // 写进哪条本地对话。**团队频道忽略它**——那边的历史在服务端,
    // 一个频道就是一条流,本地再分线程只会与所有人看到的那份分岔。
    thread_id: &str,
    channel_id: &str,
    role: &str,
    text: &str,
    run_id: Option<&str>,
    status: &str,
) -> Result<(), String> {
    match remote {
        Some(r) if !remote::is_personal(channel_id) => {
            r.send(channel_id, text, role, run_id).await.map(|_| ())
        }
        _ => {
            store.lock().unwrap().insert_in(thread_id, channel_id, role, text, run_id, status);
            Ok(())
        }
    }
}


fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("时钟早于 epoch").as_millis() as u64
}

#[derive(Default)]
struct AppState(Mutex<Option<Backend>>);

// ---------------------------------------------------------------- 载荷

#[derive(Serialize, Clone)]
struct ProviderCard {
    id: String,
    display_name: String,
    model: String,
    locality: String,
}

#[derive(Serialize)]
struct BootstrapInfo {
    channels: Vec<ChannelInfo>,
    providers: Vec<ProviderCard>,
    policy_cloud_max: Sensitivity,
    audit_db: String,
    egress_locked: bool,
}

#[derive(Serialize)]
struct DrillReportOut {
    model_calls: u64,
    egress_bytes: u64,
    unmetered_calls: u64,
    local_calls: u64,
    cloud_calls: u64,
    ok: bool,
}

#[derive(Serialize)]
struct DrillStatus {
    on: bool,
    drill_id: Option<String>,
    report: Option<DrillReportOut>,
}

#[derive(Serialize, Clone)]
struct StartPayload {
    run_id: String,
    channel_id: String,
    plan: RoutePlan,
    provider: ProviderCard,
    /// 落到最终 provider 前失败的尝试(fail-closed 轨迹,展示用)。
    attempts: Vec<String>,
}

#[derive(Serialize, Clone)]
struct DeltaPayload {
    run_id: String,
    text: String,
}

#[derive(Serialize, Clone)]
struct DiffPayload {
    run_id: String,
    branch: String,
    files_changed: usize,
    insertions: u32,
    deletions: u32,
    files: Vec<muster_runner::FileChange>,
    patch: String,
}

#[derive(Serialize, Clone)]
struct DonePayload {
    run_id: String,
    latency_ms: u64,
    finish: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    chars: usize,
}

#[derive(Serialize, Clone)]
struct FailPayload {
    run_id: String,
    channel_id: String,
    message: String,
}

#[derive(Serialize)]
struct AuditRow {
    event_id: String,
    ts_ms: u64,
    event_type: String,
    actor: String,
    run_id: Option<String>,
    channel: Option<String>,
    label: Option<String>,
    locality: Option<String>,
}

#[derive(Serialize)]
struct ChainStatus {
    ok: bool,
    rows: u64,
    detail: String,
}

// ---------------------------------------------------------------- 首页数据

#[derive(Serialize)]
struct DayBar {
    date: String,
    weekday: String,
    local: u64,
    cloud: u64,
}

#[derive(Serialize)]
struct DrillLast {
    ts_ms: u64,
    drill_id: String,
    egress_bytes: u64,
    unmetered_calls: u64,
    ok: bool,
}

#[derive(Serialize)]
struct DowngradeItem {
    ts_ms: u64,
    run_id: Option<String>,
    text: String,
}

#[derive(Serialize)]
struct RunItem {
    ts_ms: u64,
    run_id: String,
    outcome: String,
    duration_ms: u64,
}

/// 首页全部数字——每一项都由 muster-audit::queries 的一条 SQL 产出(G1 口径)。
#[derive(Serialize)]
struct HomeStats {
    runs_week: u64,
    runs_prev_week: u64,
    egress_week_bytes: u64,
    egress_prev_week_bytes: u64,
    unmetered_week: u64,
    cloud_calls_week: u64,
    local_calls_week: u64,
    pending_approvals: u64,
    drill_last: Option<DrillLast>,
    throughput: Vec<DayBar>,
    downgrades: Vec<DowngradeItem>,
    recent_runs: Vec<RunItem>,
}

const WEEK_MS: u64 = 7 * 24 * 3600 * 1000;

#[tauri::command]
fn home_stats(state: State<'_, AppState>) -> Result<HomeStats, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let now = now_ms();
    let week_ago = now.saturating_sub(WEEK_MS);
    let two_weeks_ago = now.saturating_sub(2 * WEEK_MS);

    let week = drill_report(conn, week_ago, now).map_err(|e| e.to_string())?;
    let prev = drill_report(conn, two_weeks_ago, week_ago).map_err(|e| e.to_string())?;

    let drill_last = recent_events_of(conn, "drill.end", 1)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .map(|e| {
            let egress = e.payload["egress_bytes_snapshot"].as_u64().unwrap_or(0);
            let unmetered = e.payload["unmetered_calls_snapshot"].as_u64().unwrap_or(0);
            DrillLast {
                ts_ms: e.ts_ms,
                drill_id: e.payload["drill_id"].as_str().unwrap_or("?").to_string(),
                egress_bytes: egress,
                unmetered_calls: unmetered,
                ok: egress == 0 && unmetered == 0,
            }
        });

    let recent_runs = recent_events_of(conn, "run.finish", 8)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|e| {
            let outcome = match &e.payload["outcome"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(o) => o
                    .get("failed")
                    .and_then(|f| f["class"].as_str())
                    .map(|c| format!("failed:{c}"))
                    .unwrap_or_else(|| "?".into()),
                _ => "?".into(),
            };
            RunItem {
                ts_ms: e.ts_ms,
                run_id: e.run_id.unwrap_or_else(|| "—".into()),
                outcome,
                duration_ms: e.payload["duration_ms"].as_u64().unwrap_or(0),
            }
        })
        .collect();

    Ok(HomeStats {
        runs_week: distinct_runs(conn, week_ago, now).map_err(|e| e.to_string())?,
        runs_prev_week: distinct_runs(conn, two_weeks_ago, week_ago).map_err(|e| e.to_string())?,
        egress_week_bytes: week.egress_bytes,
        egress_prev_week_bytes: prev.egress_bytes,
        unmetered_week: week.unmetered_calls,
        cloud_calls_week: week.cloud_calls,
        local_calls_week: week.local_calls,
        pending_approvals: pending_approvals(conn, AGENT_BADGE).map_err(|e| e.to_string())?,
        drill_last,
        throughput: day_throughput(conn, 7)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| DayBar { date: d.date, weekday: d.weekday, local: d.local, cloud: d.cloud })
            .collect(),
        downgrades: downgrades_zh(conn, week_ago, now)
            .map_err(|e| e.to_string())?
            .into_iter()
            .rev()
            .take(6)
            .map(|(ts_ms, run_id, text)| DowngradeItem { ts_ms, run_id, text: text.to_string() })
            .collect(),
        recent_runs,
    })
}

// ---------------------------------------------------------------- 命令

#[tauri::command]
fn bootstrap(state: State<'_, AppState>) -> Result<BootstrapInfo, String> {
    let mut guard = state.0.lock().unwrap();
    if guard.is_none() {
        // 启动时先用自带那份(单机模式)。连上服务端后会被下发的目录整个换掉,
        // 见 adopt_remote_catalog——**连着服务端时,locality 由服务端说了算**。
        let registry = ProviderRegistry::from_toml_str(BUILTIN_PROVIDERS)
            .map_err(|e| format!("provider 注册表加载失败:{e}"))?;
        let policy = OrgPolicy::new(Sensitivity::Internal).map_err(|e| format!("组织策略非法:{e:?}"))?;
        let router = Arc::new(Router::from_registry(&registry, policy.clone()));

        let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
        let dir = format!("{home}/.muster");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {dir} 失败:{e}"))?;
        let db = format!("{dir}/desktop-audit.db");
        let audit = AuditStore::open(&db).map_err(|e| format!("审计库打开失败:{e}"))?;
        // **启动即验链**(fail-closed)。此前只有手点「审计中心」才验一次——
        // 链断了应用照样跑,得靠人想起来去点。证据层坏掉却继续记新证据,
        // 等于在一份已经不可信的账本上接着记账。
        verify_or_refuse(&audit)?;
        let state_db = format!("{dir}/desktop-state.db");
        let state = StateStore::open(&state_db).map_err(|e| format!("状态库打开失败:{e}"))?;

        *guard = Some(Backend {
            router,
            policy,
            audit: Arc::new(Mutex::new(audit)),
            state: Arc::new(Mutex::new(state)),
            run_seq: Arc::new(AtomicU64::new(0)),
            drill: Arc::new(Mutex::new(None)),
            remote: Arc::new(Mutex::new(None)),
        });

        // 保留期到点即清(仅在明确设了 MUSTER_TRANSCRIPT_KEEP_DAYS 时)。
        // 清理失败不阻断启动:正文治理是运维事项,不该让人开不了应用;
        // 但**审计写失败会让本次清理整体失败**,不会出现"删了没记"。
        if let Some(days) = retention_days() {
            let b = guard.as_ref().unwrap();
            let cutoff = now_ms().saturating_sub(days as u64 * 86_400_000);
            match purge_and_record(b, &Selector::OlderThan(cutoff), Some(days)) {
                Ok(n) if n > 0 => eprintln!("正文保留期 {days} 天:已清理 {n} 条"),
                Ok(_) => {}
                Err(e) => eprintln!("正文保留期清理失败(已跳过,不阻断启动):{e}"),
            }
        }
    }
    let backend = guard.as_ref().unwrap();
    let providers = backend
        .router
        .candidates()
        .into_iter()
        .map(|m| ProviderCard {
            id: m.id,
            display_name: m.display_name,
            model: m.model,
            locality: format!("{:?}", m.locality).to_lowercase(),
        })
        .collect();
    let home = remote::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
    Ok(BootstrapInfo {
        channels: demo_channels(),
        providers,
        policy_cloud_max: backend.router.policy_snapshot().cloud_max(),
        audit_db: format!("{home}/.muster/desktop-audit.db"),
        egress_locked: backend.router.egress_locked(),
    })
}

#[tauri::command]
async fn send_message(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    text: String,
    // 写进哪条对话。None = 主对话(团队频道永远是它)。
    thread_id: Option<String>,
) -> Result<String, String> {
    let (router, audit, store, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化(先调用 bootstrap)")?;
        (b.router.clone(), b.audit.clone(), b.state.clone(), b.run_seq.clone())
    };
    let channel = resolve_channel(&state, &channel_id).await?;
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let session_id = format!("session:{}", channel.id);
    let rmt = remote_of(&state);
    let thread = thread_id.clone().unwrap_or_else(|| main_thread(&channel.id));

    // ---- E3 会话棘轮:密级只升不降
    //
    // 顺序有讲究(见 muster-route/src/session.rs):**先**用既有锁参与本轮决策,
    // **再**把本轮触碰吸收进棘轮。这样触碰 restricted 资源的那一轮,
    // deciders 里写的是资源本身;之后的轮次才由 session-lock 解释——
    // "为什么是这个级别"始终指向信息量最大的肇因。
    let sources = turn_sources_and_persist(&store, &audit, &session_id, &label_sources(&channel))
        .await?;
    record_msg(rmt.as_ref(), &store, &thread, &channel.id, "user", &text, None, "done").await?;

    // ---- E2 路由决策(含探活,fail-closed)
    let route_req = RouteRequest {
        sources: &sources,
        requested_provider: None,
        default_provider: Some("kimi"),
    };
    let resolution = match router.resolve(&route_req).await {
        Ok(r) => r,
        Err(e) => {
            // 拒绝也是证据:route.refuse 落审计(E4),写失败按命令失败处理。
            let (body, label, locality) = EventBody::route_refuse(&e, POLICY_VERSION);
            audit
                .lock()
                .unwrap()
                .append(NewEvent {
                    ts_ms: None,
                    actor: Actor::agent(AGENT_BADGE),
                    scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                    run_id: Some(run_id.clone()),
                    session_id: Some(session_id.clone()),
                    policy_version: Some(POLICY_VERSION.into()),
                    label,
                    locality,
                    body,
                })
                .map_err(|er| format!("审计写入失败:{er}"))?;
            let msg = e.to_string();
            // 路由拒绝也广播:队友该知道这个频道的密级挡下了什么,
            // 而不是只看见一个没人回答的问题
            let _ = record_msg_body(
                remote_of(&state).as_ref(),
                &store,
                &thread,
                &channel.id,
                "agent",
                &format!("⛔ 路由拒绝(fail-closed,绝不静默升云)\n{msg}"),
                Some(&run_id),
                "refused",
            );
            app.emit(
                "task-refused",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id.clone(), message: msg },
            )
            .ok();
            return Ok(run_id);
        }
    };
    let plan = resolution.plan.clone();
    let meta = resolution.provider.metadata().clone();
    let provider_card = ProviderCard {
        id: meta.id.clone(),
        display_name: meta.display_name.clone(),
        model: meta.model.clone(),
        locality: format!("{:?}", meta.locality).to_lowercase(),
    };

    // ---- A9 审计:route.decide(证据层写失败按命令失败处理,fail-closed)
    {
        let mut store = audit.lock().unwrap();
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::agent(AGENT_BADGE),
                scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                run_id: Some(run_id.clone()),
                session_id: Some(session_id.clone()),
                policy_version: Some(POLICY_VERSION.into()),
                label: Some(plan.effective),
                locality: Some(plan.primary_locality),
                body: EventBody::RouteDecide {
                    effective_label: plan.effective,
                    deciders: plan.deciders.clone(),
                    policy_version: POLICY_VERSION.into(),
                    locality: plan.primary_locality,
                    provider_id: plan.primary.clone(),
                    fallbacks: plan.fallbacks.clone(),
                    downgrade: plan.downgraded.as_ref().map(|d| d.reason),
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
    }

    app.emit(
        "task-start",
        StartPayload {
            run_id: run_id.clone(),
            channel_id: channel.id.clone(),
            plan: plan.clone(),
            provider: provider_card,
            attempts: resolution.attempts.iter().map(|a| format!("{a:?}")).collect(),
        },
    )
    .ok();

    // ---- A2 流式调用(请求载荷的规范化表示,供哈希与字节记账)
    let request_repr = serde_json::json!({
        "provider": meta.id,
        "model": meta.model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": text },
        ],
    })
    .to_string();
    let request_bytes = request_repr.len() as u64;

    // 带上本频道的历史。**在 record_msg 之后取**——刚写进去的那条提问
    // 已经在里面了,所以下面要把它排除,否则模型会看到同一句问两遍。
    let history = channel_history(rmt.as_ref(), &store, &channel.id).await;
    let ctx = context_window(&history, CTX_MAX_MSGS, CTX_MAX_CHARS);
    let mut messages = Vec::with_capacity(ctx.len() + 2);
    messages.push(ChatMessage::system(SYSTEM_PROMPT));
    for m in &ctx {
        // 刚写进去的这一问不重复放
        if m.role == "user" && m.text == text {
            continue;
        }
        messages.push(if m.role == "user" {
            ChatMessage::user(m.text.clone())
        } else {
            ChatMessage::assistant(m.text.clone())
        });
    }
    messages.push(ChatMessage::user(text.clone()));

    let req = ChatRequest {
        messages,
        run_id: Some(run_id.clone()),
        ..Default::default()
    };

    let started = Instant::now();
    let mut stream = match resolution.provider.chat_stream(req).await {
        Ok(s) => s,
        Err(e) => {
            // 失败也广播:被路由拒绝 / 开流失败时,队友只看到你的问题挂在那儿
            // 没有回答,完全看不出发生了什么
            let _ = record_msg(
                rmt.as_ref(),
                &store,
                &thread,
                &channel.id,
                "agent",
                &format!("⚠️ 开流失败:{e}"),
                Some(&run_id),
                "failed",
            )
            .await;
            app.emit(
                "task-failed",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id.clone(), message: format!("开流失败:{e}") },
            )
            .ok();
            return Ok(run_id);
        }
    };

    let mut full = String::new();
    let mut prompt_tokens = None;
    let mut completion_tokens = None;
    let mut finish = String::new();
    let mut stream_err: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(StreamEvent::TextDelta(t)) => {
                full.push_str(&t);
                app.emit("task-delta", DeltaPayload { run_id: run_id.clone(), text: t }).ok();
            }
            // v1 未声明工具,正常情况下不会出现;出现即忽略(不伪装执行)。
            Ok(StreamEvent::ToolCallDelta { .. }) => {}
            Ok(StreamEvent::Usage(u)) => {
                prompt_tokens = Some(u.prompt_tokens as u64);
                completion_tokens = Some(u.completion_tokens as u64);
            }
            Ok(StreamEvent::Finish(f)) => finish = format!("{f:?}"),
            Err(e) => {
                stream_err = Some(e.to_string());
                break;
            }
        }
    }
    let latency_ms = started.elapsed().as_millis() as u64;

    // ---- A9 审计:model.call(外发记账唯一来源;字节为载荷近似,wire 级计量属 A2 后续)
    {
        let mut store = audit.lock().unwrap();
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::agent(AGENT_BADGE),
                scope: Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
                run_id: Some(run_id.clone()),
                session_id: Some(session_id),
                policy_version: Some(POLICY_VERSION.into()),
                label: Some(plan.effective),
                locality: Some(plan.primary_locality),
                body: EventBody::ModelCall {
                    provider_id: meta.id.clone(),
                    model: meta.model.clone(),
                    locality: plan.primary_locality,
                    label: plan.effective,
                    tokens_in: prompt_tokens,
                    tokens_out: completion_tokens,
                    bytes_in: full.len() as u64,
                    // 外发口径 = 公网出口字节:本地回环不算外发,记 Measured(0)
                    //(演习报告对所有 Measured 求和,本地若记载荷字节会误报违规)。
                    bytes_out: if plan.primary_locality == Locality::Cloud {
                        EgressBytes::Measured(request_bytes)
                    } else {
                        EgressBytes::Measured(0)
                    },
                    latency_ms,
                    request_hash: ContentHash::sha256(request_repr.as_bytes()),
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
    }

    match stream_err {
        Some(msg) => {
            let _ = record_msg_body(
                rmt.as_ref(),
                &store,
                &thread,
                &channel.id,
                "agent",
                &format!("{full}\n\n⚠️ 中流失败:{msg}"),
                Some(&run_id),
                "failed",
            );
            app.emit(
                "task-failed",
                FailPayload {
                    run_id: run_id.clone(),
                    channel_id: channel.id.clone(),
                    message: format!("中流失败(v1 不重试,重试策略属 B1 Runner):{msg}"),
                },
            )
            .ok();
        }
        None => {
            let _ = record_msg(rmt.as_ref(), &store, &thread, &channel.id, "agent", &full, Some(&run_id), "done").await;
            app.emit(
                "task-done",
                DonePayload {
                    run_id: run_id.clone(),
                    latency_ms,
                    finish,
                    prompt_tokens,
                    completion_tokens,
                    chars: full.chars().count(),
                },
            )
            .ok();
        }
    }
    Ok(run_id)
}

/// Runner 事件 → 前端通道的统一转发(任务模式与能力运行共用一份,
/// 避免两处各写一套导致 UI 表现不一致)。
fn forward_runner_event(
    a: &tauri::AppHandle,
    rid: &str,
    chan: &str,
    tr: &Arc<Mutex<String>>,
    ev: RunnerEvent,
) {
    let rid = rid.to_string();
    let chan = chan.to_string();
    match ev {

        RunnerEvent::Planned { plan, provider_id, provider_name, model, locality, attempts, .. } => {
            a.emit(
                "task-start",
                StartPayload {
                    run_id: rid.clone(),
                    channel_id: chan.clone(),
                    plan,
                    provider: ProviderCard {
                        id: provider_id,
                        display_name: provider_name,
                        model,
                        locality,
                    },
                    attempts,
                },
            )
            .ok();
        }
        RunnerEvent::TextDelta { text } => {
            tr.lock().unwrap().push_str(&text);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text }).ok();
        }
        RunnerEvent::ToolCall { name, arguments, .. } => {
            let line = format!("\n🔧 {name} {arguments}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::ToolResult { summary, .. } => {
            let line = format!("   ↳ {summary}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::Notice { text } => {
            let line = format!("\n⚠️ {text}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::ApprovalRequested { approval_id, branch, .. } => {
            let line = format!(
                "\n⏳ 已申请合入(审批号 {approval_id},分支 {branch})——需人工裁决,Agent 不自行合入\n"
            );
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
            a.emit("approvals-changed", ()).ok();
        }
        RunnerEvent::WorkspaceReady { branch, .. } => {
            let line = format!("🌿 隔离工作区就绪 · 分支 {branch}\n");
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
        }
        RunnerEvent::Diff { diff, branch } => {
            let line = if diff.is_empty() {
                "\n📄 本次运行没有产生代码改动\n".to_string()
            } else {
                format!(
                    "\n📄 代码变更:{} 个文件 +{} −{}(分支 {branch})\n",
                    diff.files_changed, diff.insertions, diff.deletions
                )
            };
            tr.lock().unwrap().push_str(&line);
            a.emit("task-delta", DeltaPayload { run_id: rid.clone(), text: line }).ok();
            a.emit(
                "task-diff",
                DiffPayload {
                    run_id: rid.clone(),
                    branch,
                    files_changed: diff.files_changed,
                    insertions: diff.insertions,
                    deletions: diff.deletions,
                    files: diff.files,
                    patch: diff.patch,
                },
            )
            .ok();
        }
        RunnerEvent::Finished { outcome, latency_ms, turns, prompt_tokens, completion_tokens } => {
            if outcome == "success" {
                a.emit(
                    "task-done",
                    DonePayload {
                        run_id: rid.clone(),
                        latency_ms,
                        finish: format!("{turns} 回合"),
                        prompt_tokens: Some(prompt_tokens),
                        completion_tokens: Some(completion_tokens),
                        chars: 0,
                    },
                )
                .ok();
            } else {
                a.emit(
                    "task-failed",
                    FailPayload {
                        run_id: rid.clone(),
                        channel_id: chan.clone(),
                        message: format!("任务未完成:{outcome}"),
                    },
                )
                .ok();
            }
        }
    }
}

/// B1 任务模式:在真实工作区上跑工具循环(muster-runner),事件转发给
/// 与聊天模式相同的前端通道——工具活动以文本行形式流进任务卡。
#[tauri::command]
async fn run_workspace_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    text: String,
    thread_id: Option<String>,
) -> Result<String, String> {
    let (router, audit, store, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化(先调用 bootstrap)")?;
        (b.router.clone(), b.audit.clone(), b.state.clone(), b.run_seq.clone())
    };
    let channel = resolve_channel(&state, &channel_id).await?;
    // P2:发起任务需权限(访客不得调用敏感 Runner;禁任务频道对谁都禁)
    require(
        muster_identity::Action::CreateTask,
        muster_identity::Scope::Channel(channel.id.clone()),
    )?;
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let workspace = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    // **上服务端。** 只写本地的话,队友完全看不见你在这个频道跑了什么任务
    // ——而团队频道存在的理由就是彼此看得见
    let rmt = remote_of(&state);
    let thread = thread_id.clone().unwrap_or_else(|| main_thread(&channel.id));
    record_msg(rmt.as_ref(), &store, &thread, &channel.id, "user", &format!("▶ 任务:{text}"), None, "done")
        .await?;
    // 持久化的任务轨迹 = 前端看到的一切(文本增量 + 工具行 + 通告)。
    let transcript = Arc::new(Mutex::new(String::new()));

    // P1-04:每 run 一个隔离 worktree(WORKSPACE_ROOT 可配),写工具因此可用;
    // 非 git 仓时 runner 会如实降级只读并发 Notice。
    let workspace_root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    // 任务同样过棘轮。只让聊天守规矩的话,**跑一次任务就能绕过会话密级**
    // ——而任务送出去的内容(工作区文件、命令输出)比聊天多得多
    let session_id = format!("session:{}", channel.id);
    let sources =
        turn_sources_and_persist(&store, &audit, &session_id, &label_sources(&channel)).await?;
    let spec = TaskSpec {
        run_id: run_id.clone(),
        session_id: Some(session_id.clone()),
        team: Some(channel.team.clone()),
        channel: Some(channel.id.clone()),
        sources,
        requested_provider: None,
        default_provider: Some("kimi".into()),
        prompt: text,
        workspace: workspace.into(),
        workspace_root: Some(workspace_root.into()),
        propose_merge: true,
    };
    let cfg = RunnerConfig { policy_version: POLICY_VERSION.into(), ..Default::default() };

    let a = app.clone();
    let rid = run_id.clone();
    let chan = channel.id.clone();
    let tr = transcript.clone();
    let result = run_task(&router, &audit, &cfg, spec, move |ev| {
        forward_runner_event(&a, &rid, &chan, &tr, ev)
    })
    .await;

    let saved = transcript.lock().unwrap().clone();
    match result {
        Ok(s) => {
            let status = if s.outcome == "success" { "done" } else { "failed" };
            let text = if saved.is_empty() { s.final_text.clone() } else { saved };
            // 结果同样要广播。失败也广播——**失败对队友一样是信息**,
            // 而且比"问题挂在那儿没有回答"清楚得多
            let _ =
                record_msg(rmt.as_ref(), &store, &thread, &channel.id, "agent", &text, Some(&run_id), status)
                    .await;
            Ok(s.run_id)
        }
        // Model 错误的 UI 反馈已由 Finished(failed:stream) 事件完成,不重复发。
        Err(RunnerError::Model(msg)) => {
            let _ = record_msg_body(
                rmt.as_ref(),
                &store,
                &thread,
                &channel.id,
                "agent",
                &format!("{saved}\n\n⚠️ 中流失败(已重试一次):{msg}"),
                Some(&run_id),
                "failed",
            );
            Ok(run_id)
        }
        Err(RunnerError::Refused(msg)) => {
            store.lock().unwrap().insert(
                &channel.id,
                "agent",
                &format!("⛔ 路由拒绝(fail-closed,绝不静默升云)\n{msg}"),
                Some(&run_id),
                "refused",
            );
            app.emit(
                "task-refused",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id, message: msg },
            )
            .ok();
            Ok(run_id)
        }
        Err(e) => {
            let _ = record_msg(
                rmt.as_ref(),
                &store,
                &thread,
                &channel.id,
                "agent",
                &format!("⚠️ {e}"),
                Some(&run_id),
                "failed",
            )
            .await;
            app.emit(
                "task-failed",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id, message: e.to_string() },
            )
            .ok();
            Ok(run_id)
        }
    }
}

/// C1:启动时取回全部频道的历史消息(时间正序,总量截断)。
/// 分叉当前会话。**照抄 codex 的 Esc Esc**:选中一条早先的提问,
/// 从它之前分叉,并把那条提问原样交回给输入框等你改。
///
/// 父线程一个字节都不动——所以是分支不是移动。
#[tauri::command]
fn fork_conversation(
    state: State<'_, AppState>,
    channel_id: String,
    thread_id: Option<String>,
    nth_user_message: usize,
    persistence: Option<String>,
) -> Result<ForkResult, String> {
    let store = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.state.clone()
    };
    let src = thread_id.unwrap_or_else(|| main_thread(&channel_id));
    let mode = fork::Persistence::parse(persistence.as_deref().unwrap_or("copied"));
    let (id, inherited, reopened) = store
        .lock()
        .unwrap()
        .fork_thread(&src, &channel_id, nth_user_message, mode)
        .map_err(|e| format!("分叉失败:{e}"))?;
    Ok(ForkResult { thread_id: id, forked_from: src, inherited, reopened_prompt: reopened })
}

/// 把团队频道的一段对话 fork 到个人空间。
///
/// ## 为什么这个方向要先做,而且必须抬升密级
///
/// 方向本身是安全的:团队内容进个人空间不会外泄给更多人。
/// 但有一个反直觉的坑——**个人空间默认 open**。把一个 restricted 频道的
/// 内容搬进来却不抬升,后续在个人空间里的提问就会以 open 的身份路由:
/// **restricted 的内容进来了,路由却还按 open 走。**
///
/// 那是密级泄漏,而且外部完全看不出来:界面上个人频道的徽章仍写着 open,
/// 一切"正常工作"。
///
/// 所以搬历史和抬棘轮是**同一个动作的两半**,不是两步。棘轮抬升不了(写不进去)
/// 就不搬——宁可这次 fork 失败,也不要搬完之后密级没跟上。
#[tauri::command]
async fn fork_to_personal(
    state: State<'_, AppState>,
    channel_id: String,
    thread_id: Option<String>,
    nth_user_message: usize,
) -> Result<ForkResult, String> {
    if remote::is_personal(&channel_id) {
        return Err("这已经是个人空间了".into());
    }
    let (store, audit) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.state.clone(), b.audit.clone())
    };
    let src_channel = resolve_channel(&state, &channel_id).await?;
    let rmt = remote_of(&state);

    // 团队频道的历史在服务端,不在本地——**要向服务端取那一份**,
    // 那才是所有人共同看到的
    let history = channel_history(rmt.as_ref(), &store, &channel_id).await;
    let keep = fork::truncate_before_nth_user_message(&history, nth_user_message);
    let reopened = history.get(keep).filter(|m| m.role == "user").map(|m| m.text.clone());

    // **先抬棘轮,再搬内容。** 反过来的话,抬升失败时内容已经进了个人空间,
    // 而密级没跟上——正是这个函数要防的那件事。
    let personal_session = "session:personal";
    let touched = label_sources(&src_channel);
    if !touched.is_empty() {
        turn_sources_and_persist(&store, &audit, personal_session, &touched).await?;
    }

    let src = thread_id.unwrap_or_else(|| main_thread(&channel_id));
    let personal_main = main_thread("personal");
    {
        let st = store.lock().unwrap();
        // **追加到个人主线程,不新建线程。**
        //
        // 同频道分叉要新线程(那是"另一条走法");但拉到个人空间的意图是
        // "带过来接着聊",而"接着聊"发生在主线程上。落进一条界面不显示的
        // 分支里,等于什么都没发生——实测就是这个结果。
        //
        // 来源不靠线程行记录,靠下面这条分隔线 + 审计链里的 session.lock.raise
        // (它记着是哪个频道把密级抬上去的)。
        st.insert_in(
            &personal_main,
            "personal",
            "system",
            &format!(
                "↘ 以下 {keep} 条来自 #{}(密级 {}),不是在这里说的",
                src_channel.name, format!("{:?}", src_channel.level).to_lowercase()
            ),
            None,
            "done",
        );
        for m in history.iter().take(keep) {
            st.insert_in(&personal_main, "personal", &m.role, &m.text, m.run_id.as_deref(), &m.status);
        }
    }

    Ok(ForkResult {
        thread_id: personal_main,
        forked_from: src,
        inherited: keep,
        reopened_prompt: reopened,
    })
}

/// 开一条空对话。
///
/// 与 fork 的区别:fork 继承一段历史,这个从零开始。都落在同一张 thread 表上,
/// 因为对使用者来说它们是同一种东西——**左边列表里的一行**。
#[tauri::command]
fn new_conversation(
    state: State<'_, AppState>,
    channel_id: String,
    title: Option<String>,
) -> Result<ThreadInfo, String> {
    let store = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.state.clone()
    };
    let id = format!("conv:{}", uuid_like());
    let title = title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "新对话".into());
    store
        .lock()
        .unwrap()
        .create_thread(&id, &channel_id, &title, None, 0, fork::Persistence::Copied)
        .map_err(|e| format!("新建对话失败:{e}"))?;
    Ok(ThreadInfo {
        id,
        title,
        forked_from: None,
        inherited_count: 0,
        persistence: "copied".into(),
        created_ms: now_ms() as i64,
    })
}

/// 某频道的全部对话,**含主对话**。
///
/// 主对话不在 thread 表里(它是老库回填出来的隐式概念),但列表里必须有它
/// ——不然界面上"当前这条"没有名字,而它恰恰是绝大多数人一直待着的那条。
#[tauri::command]
fn list_threads(state: State<'_, AppState>, channel_id: String) -> Result<Vec<ThreadInfo>, String> {
    let store = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.state.clone()
    };
    let mut v = vec![ThreadInfo {
        id: main_thread(&channel_id),
        title: "主对话".into(),
        forked_from: None,
        inherited_count: 0,
        persistence: "copied".into(),
        created_ms: 0,
    }];
    v.extend(store.lock().unwrap().threads_of(&channel_id).map_err(|e| e.to_string())?);
    Ok(v)
}

#[tauri::command]
fn thread_history(state: State<'_, AppState>, thread_id: String) -> Result<Vec<StoredMsg>, String> {
    let store = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.state.clone()
    };
    let v = store.lock().unwrap().thread_history(&thread_id).map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
fn history_bulk(state: State<'_, AppState>, limit: u32) -> Result<Vec<StoredMsg>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.state.lock().unwrap();
    store.bulk(limit).map_err(|e| e.to_string())
}

#[tauri::command]
fn audit_tail(state: State<'_, AppState>, limit: u64) -> Result<Vec<AuditRow>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let events = recent_events(store.conn(), limit).map_err(|e| e.to_string())?;
    Ok(events
        .into_iter()
        .map(|e| AuditRow {
            event_type: e
                .payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            event_id: e.event_id,
            ts_ms: e.ts_ms,
            actor: format!("{:?}", e.actor),
            run_id: e.run_id,
            channel: e.scope.channel,
            label: e.label.map(|l| format!("{l:?}").to_lowercase()),
            locality: e.locality.map(|l| format!("{l:?}").to_lowercase()),
        })
        .collect())
}

/// E6 主权演习开关。演习不是特殊逻辑,是策略的一个取值:复用同一条
/// fail-closed 路由路径;结束时以 SQL 聚合窗口出第 8 幕报告。
#[tauri::command]
fn toggle_drill(state: State<'_, AppState>, on: bool) -> Result<DrillStatus, String> {
    // 演习切断全组织外联,只有组织级角色能开(§4.3)
    let who = require(muster_identity::Action::ToggleDrill, muster_identity::Scope::Org)?;
    let (router, audit, drill) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.router.clone(), b.audit.clone(), b.drill.clone())
    };
    let mut d = drill.lock().unwrap();
    if on {
        if let Some(cur) = d.as_ref() {
            return Ok(DrillStatus { on: true, drill_id: Some(cur.id.clone()), report: None });
        }
        let from_ms = now_ms();
        let id = format!("DRILL-{from_ms}");
        router.set_egress_locked(true);
        audit
            .lock()
            .unwrap()
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human(&who.id),
                scope: Scope::default(),
                run_id: None,
                session_id: None,
                policy_version: Some(POLICY_VERSION.into()),
                label: None,
                locality: None,
                body: EventBody::DrillStart { drill_id: id.clone() },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
        *d = Some(DrillState { id: id.clone(), from_ms });
        Ok(DrillStatus { on: true, drill_id: Some(id), report: None })
    } else {
        let Some(cur) = d.take() else {
            return Ok(DrillStatus { on: false, drill_id: None, report: None });
        };
        router.set_egress_locked(false);
        let to_ms = now_ms();
        let mut store = audit.lock().unwrap();
        let report = drill_report(store.conn(), cur.from_ms, to_ms).map_err(|e| e.to_string())?;
        store
            .append(NewEvent {
                ts_ms: None,
                actor: Actor::human(&who.id),
                scope: Scope::default(),
                run_id: None,
                session_id: None,
                policy_version: Some(POLICY_VERSION.into()),
                label: None,
                locality: None,
                body: EventBody::DrillEnd {
                    drill_id: cur.id.clone(),
                    egress_bytes_snapshot: report.egress_bytes,
                    unmetered_calls_snapshot: report.unmetered_calls,
                },
            })
            .map_err(|e| format!("审计写入失败:{e}"))?;
        Ok(DrillStatus {
            on: false,
            drill_id: Some(cur.id),
            report: Some(DrillReportOut {
                model_calls: report.model_calls,
                egress_bytes: report.egress_bytes,
                unmetered_calls: report.unmetered_calls,
                local_calls: report.local_calls,
                cloud_calls: report.cloud_calls,
                ok: report.ok(),
            }),
        })
    }
}

// ---------------------------------------------------------------- P4 能力库

#[derive(Serialize)]
struct CapsuleOut {
    capsule_id: String,
    name: String,
    version: String,
    scope: String,
    /// 密级(继承自源运行;跨团队引入时随包迁移,不可降密)。
    label: Option<String>,
    /// 所属团队(锻造事件的 scope.team)。
    owner_team: Option<String>,
    source_run_id: String,
    forged_ms: u64,
    forged_by: String,
    verify_passed: u64,
    verify_total: u64,
    /// **未验真是 None,不是 0%**——"没验过"与"验证失败"必须能区分。
    verified_rate: Option<f64>,
    adopted: u64,
}

#[tauri::command]
fn capsules_list(state: State<'_, AppState>) -> Result<Vec<CapsuleOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    Ok(capsules(store.conn())
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|c| CapsuleOut {
            verified_rate: c.verified_rate(),
            capsule_id: c.capsule_id,
            name: c.name,
            version: c.version,
            scope: c.scope,
            source_run_id: c.source_run_id,
            forged_ms: c.forged_ms,
            forged_by: c.forged_by,
            label: c.label,
            owner_team: c.owner_team,
            verify_passed: c.verify_passed,
            verify_total: c.verify_total,
            adopted: c.adopted,
        })
        .collect())
}

/// 可锻造的运行(成功结束、有 run.start、尚未锻造过)。
#[derive(Serialize)]
struct ForgeableRun {
    run_id: String,
    ts_ms: u64,
    /// 该运行的产出摘要(取 run.finish 的 output_hash 前缀,仅作标识)。
    output_hash: String,
    duration_ms: u64,
}

#[tauri::command]
fn forgeable_runs(state: State<'_, AppState>) -> Result<Vec<ForgeableRun>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let mut out = Vec::new();
    for e in recent_events_of(conn, "run.finish", 40).map_err(|e| e.to_string())? {
        let Some(run_id) = e.run_id.clone() else { continue };
        if e.payload["outcome"] != "success" {
            continue;
        }
        if !forgeable(conn, &run_id).map_err(|e| e.to_string())?.0 {
            continue;
        }
        out.push(ForgeableRun {
            run_id,
            ts_ms: e.ts_ms,
            output_hash: e.payload["output_hash"].as_str().unwrap_or("").to_string(),
            duration_ms: e.payload["duration_ms"].as_u64().unwrap_or(0),
        });
    }
    Ok(out)
}

/// 锻造:先取草稿(把散在事件里的事实聚拢),再落 capsule.forge。
/// 草稿是建议不是结论——`goal` 由调用方给定,允许人改写。
#[tauri::command]
fn capsule_forge(
    state: State<'_, AppState>,
    run_id: String,
    goal: String,
    visibility: String,
) -> Result<String, String> {
    require(muster_identity::Action::ForgeCapsule, muster_identity::Scope::Org)?;
    let audit = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.audit.clone()
    };
    let tools = vec![
        "list_dir".to_string(),
        "read_file".to_string(),
        "grep".to_string(),
        "write_file".to_string(),
        "replace_in_file".to_string(),
    ];
    let spec = muster_runner::draft_spec(&audit, &run_id, &goal, tools).map_err(|e| e.to_string())?;
    let store = capsule_store()?;
    // 审计存哈希、存储存正文,两者由 CapsuleStore::load 的哈希校验绑定
    let out = muster_runner::forge_and_store(
        &audit,
        &store,
        AGENT_BADGE,
        POLICY_VERSION,
        &run_id,
        spec,
        &visibility,
        Scope::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("已锻造 {} · {}(源运行 {run_id})", out.spec.name, out.capsule_id))
}

/// 运行一个 Capsule——复用已锻造的能力去干活。
///
/// 权限上等同手动发起任务(`CreateTask`):**用能力干活也是干活**,
/// 不能因为"是从能力库点的"就绕过访客限制。
#[tauri::command]
async fn capsule_run(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    capsule_id: String,
    channel_id: String,
    context: Option<String>,
) -> Result<String, String> {
    let channel = resolve_channel(&state, &channel_id).await?;
    require(
        muster_identity::Action::CreateTask,
        muster_identity::Scope::Channel(channel.id.clone()),
    )?;

    let (router, audit, store_state, run_seq) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.router.clone(), b.audit.clone(), b.state.clone(), b.run_seq.clone())
    };
    let run_id = format!("RUN-{}", 2231 + run_seq.fetch_add(1, Ordering::SeqCst));
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let workspace = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let cstore = capsule_store()?;
    let cfg = RunnerConfig { policy_version: POLICY_VERSION.into(), ..Default::default() };

    store_state.lock().unwrap().insert(
        &channel.id,
        "user",
        &format!("▶ 运行能力 {capsule_id}{}", context.as_deref().map(|c| format!(":{c}")).unwrap_or_default()),
        None,
        "done",
    );

    let a = app.clone();
    let rid = run_id.clone();
    let chan = channel.id.clone();
    let transcript = Arc::new(Mutex::new(String::new()));
    let tr = transcript.clone();

    let result = muster_runner::run_capsule(
        &router,
        &audit,
        &cstore,
        &cfg,
        &capsule_id,
        &run_id,
        std::path::Path::new(&workspace),
        std::path::Path::new(&root),
        context.as_deref(),
        turn_sources_and_persist(
            &store_state,
            &audit,
            &format!("session:{}", channel.id),
            &label_sources(&channel),
        )
        .await?,
        Scope { team: Some(channel.team.clone()), channel: Some(channel.id.clone()) },
        move |ev| forward_runner_event(&a, &rid, &chan, &tr, ev),
    )
    .await;

    let saved = transcript.lock().unwrap().clone();
    match result {
        Ok(s) => {
            store_state.lock().unwrap().insert(
                &channel.id,
                "agent",
                if saved.is_empty() { &s.final_text } else { &saved },
                Some(&run_id),
                if s.outcome == "success" { "done" } else { "failed" },
            );
            Ok(s.run_id)
        }
        Err(e) => {
            store_state.lock().unwrap().insert(&channel.id, "agent", &format!("⚠️ {e}"), Some(&run_id), "failed");
            app.emit(
                "task-failed",
                FailPayload { run_id: run_id.clone(), channel_id: channel.id, message: e.to_string() },
            )
            .ok();
            Ok(run_id)
        }
    }
}

/// 跨团队引入。**不接受目标密级参数**——密级只能随包迁移,引入不得降密。
#[tauri::command]
fn capsule_adopt(
    state: State<'_, AppState>,
    capsule_id: String,
    to_team: String,
) -> Result<String, String> {
    let who = require(
        muster_identity::Action::AdoptCapsule,
        muster_identity::Scope::Group(to_team.clone()),
    )?;
    let audit = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.audit.clone()
    };
    muster_runner::adopt(&audit, &who.id, POLICY_VERSION, &capsule_id, &to_team)
        .map(|o| o.detail)
        .map_err(|e| e.to_string())
}

fn capsule_store() -> Result<muster_runner::CapsuleStore, String> {
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    muster_runner::CapsuleStore::open(format!("{home}/.muster/capsules"))
        .map_err(|e| format!("Capsule 存储打开失败:{e}"))
}

/// 影子重放验真。**三种结局**:通过 / 不通过 / 没法验真(后者不落库,
/// 否则验真率的分母被"我们没条件验"污染)。
#[tauri::command]
async fn capsule_verify(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    capsule_id: String,
) -> Result<String, String> {
    let (router, audit) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.router.clone(), b.audit.clone())
    };
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let workspace = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let store = capsule_store()?;
    let cfg = RunnerConfig { policy_version: POLICY_VERSION.into(), ..Default::default() };

    let a = app.clone();
    let out = muster_runner::verify(
        &router,
        &audit,
        &store,
        &cfg,
        &capsule_id,
        std::path::Path::new(&workspace),
        std::path::Path::new(&root),
        move |ev| {
            if let RunnerEvent::Notice { text } = ev {
                a.emit("capsule-verify-progress", text).ok();
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    app.emit("approvals-changed", ()).ok();
    Ok(format!(
        "{} · {}",
        if out.passed { "✓ 验真通过" } else { "✗ 产出不一致" },
        out.detail
    ))
}

// ---------------------------------------------------------------- P5 审批

/// 未决审批(等人裁决的合入申请)。
#[derive(Serialize)]
struct PendingApprovalOut {
    approval_id: String,
    ts_ms: u64,
    actor_id: String,
    run_id: Option<String>,
    channel: Option<String>,
    requested_capability: String,
    reason: String,
    command_hash: String,
    /// 该 run 的隔离分支与 worktree 路径(裁决时用)。
    branch: String,
    worktree_path: String,
    /// worktree 是否仍在(被保留策略回收过就没了,此时只能拒绝)。
    worktree_exists: bool,
}

#[tauri::command]
fn approvals_pending(state: State<'_, AppState>) -> Result<Vec<PendingApprovalOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let home = remote::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));

    Ok(pending_approval_list(store.conn(), None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| {
            let run_id = p.run_id.clone().unwrap_or_default();
            let slug: String = run_id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let wt = format!("{root}/run-{slug}");
            PendingApprovalOut {
                worktree_exists: std::path::Path::new(&wt).exists(),
                branch: muster_runner::approval::branch_for(&run_id),
                worktree_path: wt,
                approval_id: p.approval_id,
                ts_ms: p.ts_ms,
                actor_id: p.actor_id,
                run_id: p.run_id,
                channel: p.channel,
                requested_capability: p.requested_capability,
                reason: p.reason,
                command_hash: p.command_hash,
            }
        })
        .collect())
}

/// 人的裁决:批准则合入主仓,拒绝则丢弃;两者都写审计,处置后回收 worktree。
#[tauri::command]
fn approvals_decide(
    state: State<'_, AppState>,
    run_id: String,
    granted: bool,
    note: Option<String>,
) -> Result<String, String> {
    let (audit, channel_scope) = {
        let guard = state.0.lock().unwrap();
        let b = guard.as_ref().ok_or("后端未初始化")?;
        (b.audit.clone(), Scope::default())
    };
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let base = std::env::var("MUSTER_WORKSPACE").unwrap_or_else(|_| format!("{home}/muster"));
    let root = std::env::var("MUSTER_WORKSPACE_ROOT")
        .unwrap_or_else(|_| format!("{home}/.muster/worktrees"));
    let slug: String =
        run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let wt_path = std::path::PathBuf::from(format!("{root}/run-{slug}"));

    let p = current_principal();
    muster_runner::decide_as(
        &audit,
        Some((&p, &directory(), &prohibitions())),
        &p.id,
        POLICY_VERSION,
        &run_id,
        channel_scope,
        std::path::Path::new(&base),
        wt_path.exists().then_some(wt_path.as_path()),
        granted,
        note.as_deref(),
    )
    .map(|o| o.detail)
    .map_err(|e| e.to_string())
}

/// D6 编制页:一行 = 审计链里真实干过活的 actor。
#[derive(Serialize)]
struct RosterEntryOut {
    actor_kind: String,
    actor_id: String,
    /// 展示名:Agent 用工牌人格名,人类用 id 本身。
    display_name: String,
    role: String,
    first_seen_ms: u64,
    last_seen_ms: u64,
    runs: u64,
    local_calls: u64,
    cloud_calls: u64,
    refusals: u64,
    events: u64,
    pending_approvals: u64,
    /// 当前路由倾向:最近一次 model.call 的落点。
    last_locality: Option<String>,
}

/// 侧栏「N人·M AI」:与在册编制同一口径(审计链里真干过活的 actor)。
#[derive(Serialize)]
struct TeamCountOut {
    team: String,
    people: u64,
    agents: u64,
}

/// 前端据此认出「链断了」这一种启动失败,并给出封存入口;
/// 其余启动失败照常只显示文本。
const CHAIN_BROKEN_TAG: &str = "AUDIT_CHAIN_BROKEN";

/// 启动自检:链不完整就**拒绝启动**。
///
/// 为什么不是"警告一下继续跑":证据层的价值全部建立在"这份账本没被动过"
/// 之上。链断之后继续往里追加新事件,新旧混在一本账里,连"断裂之前那部分
/// 还可信"都会被后来的追加搅浑。宁可不启动。
///
/// 但 fail-closed 不等于把人锁死在门外——[`audit_archive_broken`] 提供唯一
/// 出路:封存(不是删除)坏掉的那份,重开一条新链,并在新链第一条里
/// 写明来龙去脉。
fn verify_or_refuse(audit: &AuditStore) -> Result<(), String> {
    match audit.verify_chain() {
        Ok(Ok(_n)) => Ok(()),
        Ok(Err(e)) => {
            let ctx = audit
                .conn()
                .query_row(
                    "SELECT event_type, ts_ms FROM audit_event WHERE event_id = ?1",
                    rusqlite::params![e.event_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .ok();
            let what = match ctx {
                Some((ty, ts)) => format!("{ty}(ts={ts})"),
                None => "该事件已读不出来".into(),
            };
            Err(format!(
                "{CHAIN_BROKEN_TAG}|审计链在第 {} 条断裂:事件 {} · {}。\n\n                 这意味着这份账本被改过、或文件损坏。断裂之前的 {} 条仍然可信,\n                 之后的一律不可信——因此拒绝启动,不在坏账本上继续记账。\n\n                 可选处置:封存这份(不删除,留在盘上供取证)并重开新链。\n                 新链第一条会写明它断在哪、被挪到哪去了。",
                e.index + 1,
                e.event_id,
                what,
                e.index
            ))
        }
        Err(e) => Err(format!("审计链校验无法执行:{e}")),
    }
}

/// 封存断链并重开一条。**封存不是删除**:坏掉的那份改名留在原目录。
#[tauri::command]
fn audit_archive_broken(state: State<'_, AppState>) -> Result<String, String> {
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let db = format!("{home}/.muster/desktop-audit.db");
    let stamp = now_ms();
    let archived = format!("{db}.broken-{stamp}");

    // 先读出断裂位置,好写进新链的第一条(读不开就照实记 None)
    let (broken_at, verified_before) = match AuditStore::open(&db) {
        Ok(old) => match old.verify_chain() {
            Ok(Err(e)) => (Some(e.event_id), e.index),
            _ => (None, 0),
        },
        Err(_) => (None, 0),
    };

    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{db}{suffix}");
        if std::path::Path::new(&from).exists() {
            std::fs::rename(&from, format!("{archived}{suffix}"))
                .map_err(|e| format!("封存 {from} 失败:{e}"))?;
        }
    }

    let mut fresh = AuditStore::open(&db).map_err(|e| format!("新链创建失败:{e}"))?;
    fresh
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::system("audit"),
            scope: Scope::default(),
            run_id: None,
            session_id: None,
            policy_version: None,
            label: None,
            locality: None,
            body: EventBody::AuditArchived {
                archived_to: archived.clone(),
                broken_at_event_id: broken_at,
                verified_before_break: verified_before,
            },
        })
        .map_err(|e| format!("新链首条写入失败:{e}"))?;

    // 让下次 bootstrap 重新装配后端(它会拿到这条新链)
    *state.0.lock().unwrap() = None;
    Ok(archived)
}

// ---------------------------------------------------------------- 正文存储治理

#[derive(Serialize)]
struct TranscriptStats {
    messages: u64,
    text_bytes: u64,
    oldest_ts_ms: Option<u64>,
    /// 保留期(天);None = 未开启,正文永久保留
    keep_days: Option<u32>,
    /// 导出落地目录(固定位置,免得到处散落)
    export_dir: String,
}

#[tauri::command]
fn transcript_stats(state: State<'_, AppState>) -> Result<TranscriptStats, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let (messages, text_bytes, oldest_ts_ms) =
        b.state.lock().unwrap().stats().map_err(|e| e.to_string())?;
    let home = remote::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
    Ok(TranscriptStats {
        messages,
        text_bytes,
        oldest_ts_ms,
        keep_days: retention_days(),
        export_dir: format!("{home}/.muster/exports"),
    })
}

/// 记一笔正文治理事件。**审计写失败即操作失败**——正文治理必须留痕,
/// 否则"删得掉"就成了"删得掉且没人知道",那是另一回事。
fn append_transcript_event(b: &Backend, body: EventBody) -> Result<(), String> {
    let me = current_principal();
    b.audit
        .lock()
        .unwrap()
        .append(NewEvent {
            ts_ms: None,
            actor: Actor::human(&me.id),
            scope: Scope::default(),
            run_id: None,
            session_id: None,
            policy_version: Some("policy-v1".into()),
            label: None,
            locality: None,
            body,
        })
        .map(|_| ())
        .map_err(|e| format!("审计写入失败,操作已取消:{e}"))
}

#[tauri::command]
fn transcript_export(
    state: State<'_, AppState>,
    kind: String,
    value: Option<String>,
) -> Result<String, String> {
    let sel = Selector::parse(&kind, value)?;
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let rows = b.state.lock().unwrap().select(&sel).map_err(|e| e.to_string())?;

    let mut jsonl = String::new();
    for m in &rows {
        jsonl.push_str(&serde_json::to_string(m).map_err(|e| e.to_string())?);
        jsonl.push('\n');
    }
    let home = remote::home_dir().map(|p| p.display().to_string()).ok_or("找不到家目录(HOME / USERPROFILE 都没有)")?;
    let dir = format!("{home}/.muster/exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {dir} 失败:{e}"))?;
    let path = format!("{dir}/transcript-{}.jsonl", now_ms());
    std::fs::write(&path, jsonl.as_bytes()).map_err(|e| format!("写入 {path} 失败:{e}"))?;

    append_transcript_event(b, EventBody::TranscriptExport {
        selector: sel.tag(None),
        exported: rows.len() as u64,
        content_hash: ContentHash::sha256(jsonl.as_bytes()),
    })?;
    Ok(path)
}

#[tauri::command]
fn transcript_purge(
    state: State<'_, AppState>,
    kind: String,
    value: Option<String>,
) -> Result<u64, String> {
    let sel = Selector::parse(&kind, value)?;
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    purge_and_record(b, &sel, None)
}

/// 删除 + 留痕。**先取区间再删**:删完就问不出"删掉的是哪一段"了。
fn purge_and_record(b: &Backend, sel: &Selector, keep_days: Option<u32>) -> Result<u64, String> {
    let store = b.state.lock().unwrap();
    let doomed = store.select(sel).map_err(|e| e.to_string())?;
    if doomed.is_empty() {
        return Ok(0);
    }
    let oldest = doomed.iter().map(|m| m.ts_ms as u64).min();
    let newest = doomed.iter().map(|m| m.ts_ms as u64).max();
    let n = store.purge(sel).map_err(|e| e.to_string())?;
    drop(store);

    append_transcript_event(b, EventBody::TranscriptPurge {
        selector: sel.tag(keep_days),
        deleted: n,
        oldest_ts_ms: oldest,
        newest_ts_ms: newest,
    })?;
    Ok(n)
}

// ---------------------------------------------------------------- C1 接服务端

#[derive(Serialize)]
struct RemoteStatus {
    connected: bool,
    base: Option<String>,
    account_id: Option<String>,
    display_name: Option<String>,
}

#[tauri::command]
async fn remote_login(
    state: State<'_, AppState>,
    base: String,
    id: String,
    password: String,
) -> Result<RemoteStatus, String> {
    // 登录先做完再取锁:HTTP 要 await,而 std::sync::Mutex 的 guard 不能跨 await
    let r = remote::Remote::login(&base, &id, &password).await?;
    let handle = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.remote.clone()
    };
    let st = RemoteStatus {
        connected: true,
        base: Some(r.base.clone()),
        account_id: Some(r.account_id.clone()),
        display_name: Some(r.display_name.clone()),
    };
    // **先换目录,再算连上。** 拿不到目录就不该进入"已连接"——
    // 那种状态下用的是本机声明的 locality,正是这次要堵的洞。
    let n = adopt_remote_catalog(&state, &r).await?;
    if n == 0 {
        tracing_warn("服务端还没配任何 provider:能连上,但跑任务会被路由拒绝");
    }
    r.save_session();
    *handle.lock().unwrap() = Some(r);
    Ok(st)
}

/// 没引 tracing,又不想为一句话引。stderr 足够——这行是给运维看的。
fn tracing_warn(msg: &str) {
    eprintln!("[warn] {msg}");
}

#[tauri::command]
fn remote_logout(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let b = guard.as_mut().ok_or("后端未初始化")?;
    *b.remote.lock().unwrap() = None;
    // 退回自带那份:回到单机模式,行为与从未连过一致。
    // 留着服务端下发的目录会更"省事",但那份目录是组织的策略,
    // 断开之后它凭什么还管着这台机器
    let registry = ProviderRegistry::from_toml_str(BUILTIN_PROVIDERS)
        .map_err(|e| format!("provider 注册表加载失败:{e}"))?;
    b.router = Arc::new(Router::from_registry(&registry, b.policy.clone()));
    remote::Remote::clear_session();
    Ok(())
}

/// 启动时恢复上次的连接。**探不通就当没连上**——显示"已连接"却什么都拉不到
/// 比显示"单机模式"糟得多。
#[tauri::command]
async fn remote_restore(state: State<'_, AppState>) -> Result<RemoteStatus, String> {
    let handle = {
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("后端未初始化")?.remote.clone()
    };
    match remote::Remote::restore().await {
        Some(r) => {
            // 恢复连接同样要换目录。漏掉这一步的后果很隐蔽:**重启一次就
            // 悄悄退回本机声明的 locality**,而界面照常显示"已连接"。
            if let Err(e) = adopt_remote_catalog(&state, &r).await {
                // 拿不到目录就不算连上,与 remote_login 同一条规矩
                tracing_warn(&format!("恢复连接失败(拿不到 provider 目录):{e}"));
                remote::Remote::clear_session();
                return Ok(RemoteStatus {
                    connected: false,
                    base: None,
                    account_id: None,
                    display_name: None,
                });
            }
            let st = RemoteStatus {
                connected: true,
                base: Some(r.base.clone()),
                account_id: Some(r.account_id.clone()),
                display_name: Some(r.display_name.clone()),
            };
            *handle.lock().unwrap() = Some(r);
            Ok(st)
        }
        None => Ok(RemoteStatus {
            connected: false,
            base: None,
            account_id: None,
            display_name: None,
        }),
    }
}

/// 实时通道所需的令牌(C2)。前端拿它自己开 `EventSource`——
/// **浏览器的自动重连与 `Last-Event-ID` 是白送的**,经 Tauri 命令中转
/// 反而要把那套逻辑重写一遍。
#[tauri::command]
fn remote_token(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let r = b.remote.lock().unwrap();
    Ok(r.as_ref().map(|r| r.token.clone()))
}

#[tauri::command]
fn remote_status(state: State<'_, AppState>) -> Result<RemoteStatus, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let r = b.remote.lock().unwrap();
    Ok(match r.as_ref() {
        Some(r) => RemoteStatus {
            connected: true,
            base: Some(r.base.clone()),
            account_id: Some(r.account_id.clone()),
            display_name: Some(r.display_name.clone()),
        },
        None => RemoteStatus {
            connected: false,
            base: None,
            account_id: None,
            display_name: None,
        },
    })
}

/// 取当前服务端句柄的克隆(不持锁跨 await)。
fn remote_of(state: &State<'_, AppState>) -> Option<remote::Remote> {
    let guard = state.0.lock().ok()?;
    let b = guard.as_ref()?;
    let r = b.remote.lock().ok()?;
    r.clone()
}

/// 组织的频道清单(登录后拉)。
///
/// 与 `bootstrap` 分开不是权宜:**bootstrap 回答"这台机器有什么",
/// 本命令回答"这个组织有什么"**。两者来源不同、失败方式也不同——
/// 服务端拉不到时前端仍要能显示本地那份,而不是整个界面起不来。
///
/// 个人频道不在返回值里:它不上服务端,所以服务端也不会有它。
#[tauri::command]
async fn remote_channels(state: State<'_, AppState>) -> Result<Vec<ChannelInfo>, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    let rows = r.channels().await?;
    Ok(rows
        .into_iter()
        .map(|c| ChannelInfo {
            id: c.id,
            name: c.name,
            team_id: c.team_id.clone(),
            team: c.team_id,
            level: remote::parse_level(&c.level),
            level_note: "频道密级由服务端组织配置决定".into(),
            desc: String::new(),
            personal: false,
        })
        .collect())
}

// ---- C3:会议
#[tauri::command]
async fn remote_meetings(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<Vec<remote::RemoteMeeting>, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.meetings(&channel_id).await
}

#[tauri::command]
async fn remote_meeting_start(
    state: State<'_, AppState>,
    channel_id: String,
    title: String,
) -> Result<remote::RemoteMeeting, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.start_meeting(&channel_id, &title).await
}

/// 拿入会票。票里的 `can_publish` 是服务端 `can()` 的判定结果——
/// **前端照着它决定要不要显示开麦按钮,而不是自己判一遍**。
#[tauri::command]
async fn remote_meeting_join(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<remote::JoinInfo, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.join_meeting(&meeting_id).await
}

#[tauri::command]
async fn remote_meeting_agent(
    state: State<'_, AppState>,
    meeting_id: String,
    want: bool,
) -> Result<(), String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.set_wants_agent(&meeting_id, want).await
}

#[tauri::command]
async fn remote_meeting_end(state: State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.end_meeting(&meeting_id).await
}

#[tauri::command]
async fn remote_action_items(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<remote::ActionItem>, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.action_items(&meeting_id).await
}

/// 批准 / 驳回一条行动项。**判定在服务端**——这里不自己判权限,
/// 前端更不判:界面只照着服务端的答复显示。
#[tauri::command]
async fn remote_decide_action(
    state: State<'_, AppState>,
    id: String,
    confirm: bool,
) -> Result<remote::ActionItem, String> {
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    r.decide_action(&id, confirm).await
}

/// 团队频道的消息从服务端拉;**个人频道永不走这条路**(见 remote.rs 模块文档)。
#[tauri::command]
async fn remote_history(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<Vec<StoredMsg>, String> {
    if remote::is_personal(&channel_id) {
        return Err("个人频道的内容不上服务端,这是产品承诺".into());
    }
    let Some(r) = remote_of(&state) else { return Err("未连接服务端".into()) };
    let msgs = r.messages(&channel_id, None).await?;
    Ok(msgs
        .into_iter()
        .map(|m| StoredMsg {
            channel_id: channel_id.clone(),
            // 服务端来的消息属于该频道的主线程:分叉是本地会话的概念,
            // 团队频道的历史不由某台机器分叉
            thread_id: main_thread(&channel_id),
            role: m.role,
            text: m.body,
            run_id: m.run_id,
            status: "done".into(),
            ts_ms: m.ts_ms,
        })
        .collect())
}

#[tauri::command]
fn roster_counts_cmd(state: State<'_, AppState>) -> Result<Vec<TeamCountOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    Ok(muster_audit::roster_counts(store.conn())
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(team, people, agents)| TeamCountOut { team, people, agents })
        .collect())
}

#[tauri::command]
fn roster_stats(state: State<'_, AppState>, team: Option<String>) -> Result<Vec<RosterEntryOut>, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let rows = roster(conn, team.as_deref()).map_err(|e| e.to_string())?;
    let recent = recent_events_of(conn, "model.call", 200).map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let last_locality = recent
                .iter()
                .find(|e| e.actor.id == r.actor_id)
                .and_then(|e| e.locality.map(|l| format!("{l:?}").to_lowercase()));
            let (display_name, role) = match (r.actor_kind.as_str(), r.actor_id.as_str()) {
                ("agent", AGENT_BADGE) => ("小七".to_string(), "代码评审员".to_string()),
                ("agent", id) => (id.to_string(), "Agent".to_string()),
                ("human", id) => (id.to_string(), "成员".to_string()),
                (_, id) => (id.to_string(), "系统".to_string()),
            };
            RosterEntryOut {
                pending_approvals: pending_approvals(conn, &r.actor_id).unwrap_or(0),
                actor_kind: r.actor_kind,
                actor_id: r.actor_id,
                display_name,
                role,
                first_seen_ms: r.first_seen_ms,
                last_seen_ms: r.last_seen_ms,
                runs: r.runs,
                local_calls: r.local_calls,
                cloud_calls: r.cloud_calls,
                refusals: r.refusals,
                events: r.events,
                last_locality,
            }
        })
        .collect())
}

/// Agent 档案页统计:入职(首条审计事件)/ 累计 Runs / 累计外发 / 活动热力。
#[derive(Serialize)]
struct AgentStats {
    badge: String,
    first_seen_ms: Option<u64>,
    hired_days: u64,
    total_runs: u64,
    total_egress_bytes: u64,
    /// 近 48 周逐日活动(model.call 数),供贡献热力图。
    heat: Vec<DayBar>,
}

#[tauri::command]
fn agent_stats(state: State<'_, AppState>) -> Result<AgentStats, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    let conn = store.conn();
    let now = now_ms();
    let first = actor_first_seen(conn, AGENT_BADGE).map_err(|e| e.to_string())?;
    Ok(AgentStats {
        badge: AGENT_BADGE.into(),
        first_seen_ms: first,
        hired_days: first.map(|f| (now.saturating_sub(f)) / 86_400_000 + 1).unwrap_or(0),
        total_runs: distinct_runs(conn, 0, now).map_err(|e| e.to_string())?,
        total_egress_bytes: drill_report(conn, 0, now).map_err(|e| e.to_string())?.egress_bytes,
        heat: day_throughput(conn, 336)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| DayBar { date: d.date, weekday: d.weekday, local: d.local, cloud: d.cloud })
            .collect(),
    })
}

#[tauri::command]
fn verify_chain(state: State<'_, AppState>) -> Result<ChainStatus, String> {
    let guard = state.0.lock().unwrap();
    let b = guard.as_ref().ok_or("后端未初始化")?;
    let store = b.audit.lock().unwrap();
    match store.verify_chain().map_err(|e| e.to_string())? {
        Ok(rows) => Ok(ChainStatus { ok: true, rows, detail: format!("{rows} 行哈希链完整") }),
        Err(e) => Ok(ChainStatus { ok: false, rows: 0, detail: format!("{e:?}") }),
    }
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            send_message,
            run_workspace_task,
            audit_tail,
            verify_chain,
            toggle_drill,
            home_stats,
            agent_stats,
            history_bulk,
            roster_stats,
            roster_counts_cmd,
            audit_archive_broken,
            transcript_stats,
            transcript_export,
            transcript_purge,
            remote_login,
            remote_logout,
            remote_status,
            remote_restore,
            remote_history,
            remote_channels,
            remote_token,
            remote_meetings,
            remote_meeting_start,
            remote_meeting_join,
            fork_conversation,
            fork_to_personal,
            list_threads,
            new_conversation,
            thread_history,
            remote_action_items,
            remote_decide_action,
            remote_meeting_end,
            remote_meeting_agent,
            approvals_pending,
            approvals_decide,
            capsules_list,
            forgeable_runs,
            capsule_forge,
            capsule_verify,
            capsule_adopt,
            capsule_run,
            whoami
        ])
        .run(tauri::generate_context!())
        .expect("muster-desktop 启动失败");
}

#[cfg(test)]
mod label_tests {
    use super::*;

    fn ch(id: &str, level: Sensitivity) -> ChannelInfo {
        ChannelInfo {
            id: id.into(),
            name: id.into(),
            team_id: "t".into(),
            team: "t".into(),
            level,
            level_note: String::new(),
            desc: String::new(),
            personal: false,
        }
    }

    /// **密级标签按频道的 level 生成,不按 id 打表。**
    ///
    /// 曾经这里是按 demo 频道 id 匹配的,于是服务端来的频道(id 不在表里)
    /// 拿到空标签——一个 restricted 的服务端频道会被当成 open 路由。
    /// 那是密级泄漏,而且外部完全看不出来:界面上徽章显示得好好的,
    /// 路由却当它不存在。
    #[test]
    fn labels_follow_the_level_not_the_id() {
        // 服务端来的陌生 id,照样要带上密级
        let s = label_sources(&ch("platform-main", Sensitivity::Restricted));
        assert_eq!(s.len(), 1, "陌生 id 的 restricted 频道必须带标签");
        assert_eq!(s[0].level, Sensitivity::Restricted);
        assert_eq!(s[0].subject, "channel:platform-main");

        let s = label_sources(&ch("随便什么id", Sensitivity::Internal));
        assert_eq!(s[0].level, Sensitivity::Internal);
    }

    /// open 不产生标签是**有意的**:E1 里"未标注即 open"是默认值而非断言,
    /// 加一条 open 标签只会让「为什么是这个密级」里多一条无信息的来源。
    #[test]
    fn open_produces_no_label_on_purpose() {
        assert!(label_sources(&ch("general", Sensitivity::Open)).is_empty());
    }
}

#[cfg(test)]
mod transcript_tests {
    use super::*;

    fn store_with(msgs: &[(&str, &str, Option<&str>)]) -> (tempfile::TempDir, StateStore) {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        let s = StateStore::open(path.to_str().unwrap()).unwrap();
        for (chan, text, run) in msgs {
            s.insert(chan, "user", text, *run, "done");
        }
        (d, s)
    }

    /// 能导出什么就能删什么:两边共用同一套 Selector,不留"看得见却删不掉"的角落。
    #[test]
    fn select_and_purge_agree_on_every_selector() {
        for (kind, value, expect) in [
            ("all", None, 3),
            ("channel", Some("platform".to_string()), 2),
            ("run", Some("RUN-1".to_string()), 1),
        ] {
            let (_d, s) = store_with(&[
                ("platform", "甲", Some("RUN-1")),
                ("platform", "乙", None),
                ("pay", "丙", None),
            ]);
            let sel = Selector::parse(kind, value).unwrap();
            assert_eq!(s.select(&sel).unwrap().len(), expect, "{kind} 选取数");
            assert_eq!(s.purge(&sel).unwrap(), expect as u64, "{kind} 删除数");
            assert_eq!(s.select(&sel).unwrap().len(), 0, "{kind} 删完应为空");
        }
    }

    /// 保留期只删过期的,不碰新的。
    #[test]
    fn retention_only_removes_what_is_past_the_cutoff() {
        let (_d, s) = store_with(&[("platform", "旧", None), ("platform", "新", None)]);
        // 把第一条改成 100 天前
        let old_ts = now_ms() as i64 - 100 * 86_400_000;
        s.conn
            .execute("UPDATE messages SET ts_ms = ?1 WHERE id = 1", rusqlite::params![old_ts])
            .unwrap();

        let cutoff = now_ms().saturating_sub(90 * 86_400_000);
        assert_eq!(s.purge(&Selector::OlderThan(cutoff)).unwrap(), 1);
        let left = s.select(&Selector::All).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].text, "新", "只该删过期的那条");
    }

    /// **删了就要真的从文件里消失。** 只 DELETE 不 VACUUM 的话,SQLite 只把页
    /// 标记为空闲,正文仍躺在文件里,`strings` 一读就出来——对一个以"删得掉"
    /// 为卖点的功能来说那是假删。
    #[test]
    fn purged_text_is_gone_from_the_file_not_just_from_queries() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        let secret = "sk-live-绝密令牌-9f2c1a";
        {
            let s = StateStore::open(path.to_str().unwrap()).unwrap();
            for _ in 0..50 {
                s.insert("platform", "user", secret, None, "done");
            }
            let raw = std::fs::read(&path).unwrap();
            assert!(
                String::from_utf8_lossy(&raw).contains(secret),
                "前提检查:删之前正文确实在文件里"
            );
            assert_eq!(s.purge(&Selector::All).unwrap(), 50);
        }
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains(secret),
            "删除后正文仍能在文件里被找到——这是假删"
        );
    }

    /// 正文治理的口径串进审计,且**不含正文**。
    #[test]
    fn selector_tags_carry_scope_but_never_content() {
        assert_eq!(Selector::All.tag(None), "all");
        assert_eq!(Selector::Channel("platform".into()).tag(None), "channel:platform");
        assert_eq!(Selector::Run("RUN-9".into()).tag(None), "run:RUN-9");
        assert_eq!(Selector::OlderThan(0).tag(Some(90)), "retention:90d");
    }
}

#[cfg(test)]
mod fork_store_tests {
    use super::*;
    use crate::fork::Persistence;

    /// 一轮问答。`u` 进去,`a` 跟一条。
    fn conversation(s: &StateStore, chan: &str, turns: &[(&str, Option<&str>)]) {
        for (q, a) in turns {
            s.insert(chan, "user", q, None, "done");
            if let Some(a) = a {
                s.insert(chan, "agent", a, None, "done");
            }
        }
    }

    fn store() -> (tempfile::TempDir, StateStore) {
        let d = tempfile::tempdir().unwrap();
        let s = StateStore::open(d.path().join("state.db").to_str().unwrap()).unwrap();
        (d, s)
    }

    /// 分叉出来的是**新线程**,父线程一个字节都不动。
    #[test]
    fn fork_leaves_the_parent_untouched() {
        let (_d, s) = store();
        conversation(&s, "c1", &[("问一", Some("答一")), ("问二", Some("答二")), ("问三", None)]);
        let parent = main_thread("c1");
        let before = s.thread_history(&parent).unwrap().len();

        let (child, _, _) = s.fork_thread(&parent, "c1", 1, Persistence::Copied).unwrap();

        assert_eq!(s.thread_history(&parent).unwrap().len(), before, "父线程被动过了");
        assert_ne!(child, parent, "分叉必须是新 id,不是重命名");
    }

    /// 切在第 n 条提问之前,并把那条提问**原样交回**。
    ///
    /// 交回这件事是 codex 那个功能好用的全部理由:Esc Esc 之后提问重新出现在
    /// 输入框里等你改。少了它就只是"复制一份对话"。
    #[test]
    fn the_cut_prompt_comes_back_for_editing() {
        let (_d, s) = store();
        conversation(&s, "c1", &[("问一", Some("答一")), ("问二", Some("答二"))]);
        let (_, inherited, reopened) =
            s.fork_thread(&main_thread("c1"), "c1", 1, Persistence::Copied).unwrap();
        assert_eq!(inherited, 2, "第 1 条提问之前有两条(问一 + 答一)");
        assert_eq!(reopened.as_deref(), Some("问二"));
    }

    /// 两种 persistence **读出来必须一样**。
    ///
    /// 它们的差别只该在"历史存在哪":copied 抄一份,referenced 读时拼。
    /// 一旦读出来不同,那就不是两种存法,是两种语义。
    #[test]
    fn both_persistence_modes_read_back_identically() {
        let (_d, s) = store();
        conversation(&s, "c1", &[("问一", Some("答一")), ("问二", Some("答二")), ("问三", Some("答三"))]);
        let parent = main_thread("c1");

        let (copied, _, _) = s.fork_thread(&parent, "c1", 2, Persistence::Copied).unwrap();
        let (referenced, _, _) = s.fork_thread(&parent, "c1", 2, Persistence::Referenced).unwrap();

        let a: Vec<_> = s.thread_history(&copied).unwrap().into_iter().map(|m| m.text).collect();
        let b: Vec<_> = s.thread_history(&referenced).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(a, b, "两种存法读出来的历史不一致");
        assert_eq!(a, vec!["问一", "答一", "问二", "答二"]);
    }

    /// referenced 模式下,**新线程后来写的内容不会污染父线程**。
    #[test]
    fn referenced_child_writes_do_not_leak_into_the_parent() {
        let (_d, s) = store();
        conversation(&s, "c1", &[("问一", Some("答一")), ("问二", Some("答二"))]);
        let parent = main_thread("c1");
        let (child, _, _) = s.fork_thread(&parent, "c1", 1, Persistence::Referenced).unwrap();

        s.insert_in(&child, "c1", "user", "分叉后的新问题", None, "done");

        let p: Vec<_> = s.thread_history(&parent).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(p, vec!["问一", "答一", "问二", "答二"], "父线程被污染了");
        let c: Vec<_> = s.thread_history(&child).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(c, vec!["问一", "答一", "分叉后的新问题"]);
    }

    /// 分叉的分叉。referenced 要能一路往上拼。
    #[test]
    fn forks_of_forks_resolve_the_whole_chain() {
        let (_d, s) = store();
        conversation(&s, "c1", &[("问一", Some("答一")), ("问二", Some("答二"))]);
        let (a, _, _) = s.fork_thread(&main_thread("c1"), "c1", 1, Persistence::Referenced).unwrap();
        s.insert_in(&a, "c1", "user", "问二改", None, "done");
        s.insert_in(&a, "c1", "agent", "答二改", None, "done");
        let (b, _, _) = s.fork_thread(&a, "c1", 1, Persistence::Referenced).unwrap();

        let got: Vec<_> = s.thread_history(&b).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(got, vec!["问一", "答一"], "二级分叉应当只继承到第 1 条提问之前");
    }

    /// 老库(没有 thread_id 列)打开后仍读得出历史。
    ///
    /// 迁移是**加列 + 回填**,不是重建表——重建会丢掉 rowid 顺序,
    /// 而消息的先后全靠它。
    #[test]
    fn an_old_database_without_thread_id_still_reads() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        {
            // 手工造一个"老库":没有 thread_id 列
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE messages(
                    id INTEGER PRIMARY KEY AUTOINCREMENT, channel_id TEXT NOT NULL,
                    role TEXT NOT NULL, text TEXT NOT NULL, run_id TEXT,
                    status TEXT NOT NULL, ts_ms INTEGER NOT NULL);
                 INSERT INTO messages(channel_id, role, text, status, ts_ms)
                   VALUES('c1','user','老消息一','done',1),('c1','agent','老回复一','done',2);",
            )
            .unwrap();
        }
        let s = StateStore::open(path.to_str().unwrap()).unwrap();
        let got: Vec<_> =
            s.thread_history(&main_thread("c1")).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(got, vec!["老消息一", "老回复一"], "老库的消息没有被回填到主线程");
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn msg(role: &str, text: &str, status: &str) -> StoredMsg {
        StoredMsg {
            channel_id: "c1".into(),
            thread_id: main_thread("c1"),
            role: role.into(),
            text: text.into(),
            run_id: None,
            status: status.into(),
            ts_ms: 0,
        }
    }

    /// 放不下时丢的是**最早的**。
    ///
    /// 从前往后取会出现"聊了一小时,模型只记得开头那几句"——
    /// 比完全没有上下文更让人困惑,因为它看起来在记事,记的却是错的那头。
    #[test]
    fn keeps_the_most_recent_and_stays_in_order() {
        let h: Vec<_> = (1..=6).map(|i| msg("user", &format!("第{i}问"), "done")).collect();
        let got: Vec<_> = context_window(&h, 3, 9999).iter().map(|m| m.text.clone()).collect();
        assert_eq!(got, vec!["第4问", "第5问", "第6问"], "该留最近三条,且时间正序");
    }

    /// 失败的系统文本不进上下文。
    ///
    /// 「⚠️ 开流失败」不是助手说过的话。喂回去,模型会当成自己上一轮的输出,
    /// 然后一本正经地接着往下编。
    #[test]
    fn failed_system_text_is_not_context() {
        let h = vec![
            msg("user", "问一", "done"),
            msg("agent", "⚠️ 开流失败:连接超时", "failed"),
            msg("user", "问二", "done"),
        ];
        let got: Vec<_> = context_window(&h, 10, 9999).iter().map(|m| m.text.clone()).collect();
        assert_eq!(got, vec!["问一", "问二"]);
    }

    /// 两个上限都生效,而且字符上限不该把结果清空。
    #[test]
    fn both_caps_apply_and_one_message_always_survives() {
        let h = vec![msg("user", &"甲".repeat(50), "done"), msg("user", &"乙".repeat(50), "done")];
        let got = context_window(&h, 10, 60);
        assert_eq!(got.len(), 1, "字符上限只放得下一条");
        assert!(got[0].text.starts_with('乙'), "留下的该是最近那条");

        // 单条就超上限时也不能返回空:那等于悄悄退化成无上下文
        let big = vec![msg("user", &"丙".repeat(500), "done")];
        assert_eq!(context_window(&big, 10, 10).len(), 1, "单条超限也要保留,否则等于没有上下文");
    }

    #[test]
    fn empty_text_is_skipped() {
        let h = vec![msg("agent", "   ", "done"), msg("user", "问", "done")];
        assert_eq!(context_window(&h, 10, 9999).len(), 1);
    }
}

#[cfg(test)]
mod ratchet_tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StateStore) {
        let d = tempfile::tempdir().unwrap();
        let s = StateStore::open(d.path().join("state.db").to_str().unwrap()).unwrap();
        (d, s)
    }

    fn chan(id: &str, level: Sensitivity) -> ChannelInfo {
        ChannelInfo {
            id: id.into(),
            name: id.into(),
            team: "平台组".into(),
            team_id: "platform".into(),
            level,
            level_note: String::new(),
            desc: String::new(),
            personal: id == "personal",
        }
    }

    /// 棘轮**必须跨重启保留**。
    ///
    /// 这不是优化,是语义的一部分:底线只升不降。重启就归零的话,
    /// 关一次应用等于一次降密——而关应用是每天都会发生的事。
    #[test]
    fn the_ratchet_survives_a_restart() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        {
            let s = StateStore::open(path.to_str().unwrap()).unwrap();
            let mut r = SessionRatchet::new();
            r.observe(&label_sources(&chan("payments", Sensitivity::Restricted)));
            s.save_ratchet("session:personal", &r).unwrap();
        }
        let s = StateStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            s.load_ratchet("session:personal").floor(),
            Sensitivity::Restricted,
            "重启后底线掉回去了——那等于关一次应用就降一次密"
        );
    }

    /// 没有任何降低底线的路径。
    ///
    /// `SessionRatchet` 类型上就没有降级 API(接口即政策),这里验的是
    /// **我们的用法**没有绕过它:比如先读出来、改一个低的、再存回去。
    #[test]
    fn observing_a_lower_level_never_lowers_the_floor() {
        let (_d, s) = store();
        let mut r = SessionRatchet::new();
        r.observe(&label_sources(&chan("payments", Sensitivity::Restricted)));
        s.save_ratchet("sess", &r).unwrap();

        // 之后又碰了一个 internal 的频道
        let mut r2 = s.load_ratchet("sess");
        r2.observe(&label_sources(&chan("platform", Sensitivity::Internal)));
        s.save_ratchet("sess", &r2).unwrap();

        assert_eq!(s.load_ratchet("sess").floor(), Sensitivity::Restricted, "底线被拉低了");
    }

    /// 会话之间互不影响:抬升一个不该殃及另一个。
    #[test]
    fn sessions_are_independent() {
        let (_d, s) = store();
        let mut r = SessionRatchet::new();
        r.observe(&label_sources(&chan("payments", Sensitivity::Restricted)));
        s.save_ratchet("session:payments", &r).unwrap();

        assert_eq!(s.load_ratchet("session:personal").floor(), Sensitivity::Open);
    }

    /// open 频道不产生锁——open 是默认底,锁在 open 没有信息量。
    #[test]
    fn open_channels_do_not_lock_the_session() {
        let mut r = SessionRatchet::new();
        assert!(r.observe(&label_sources(&chan("general", Sensitivity::Open))).is_none());
        assert!(!r.is_locked());
    }
}
