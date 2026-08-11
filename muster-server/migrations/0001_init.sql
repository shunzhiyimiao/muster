-- P2/P3 初始 schema。命名与 muster-identity 的语义一一对应,
-- 免得服务端和内核各说各话。

CREATE TABLE IF NOT EXISTS account (
    id            TEXT PRIMARY KEY,
    display_name  TEXT NOT NULL,
    -- Argon2 PHC 串;OIDC 接入后该列可空(身份由 IdP 断言)
    password_hash TEXT,
    -- 'human' | 'agent' | 'service',与 muster_identity::PrincipalKind 同源
    kind          TEXT NOT NULL DEFAULT 'human',
    -- 外部身份(iss+sub 唯一),P2-04 用;当前为空
    ext_iss       TEXT,
    ext_sub       TEXT,
    created_ms    BIGINT NOT NULL,
    -- 停用而非删除:删账号会让历史里的 author_id 变成孤儿,
    -- 而"这条消息是谁发的"是不能丢的
    disabled      BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (ext_iss, ext_sub)
);

CREATE TABLE IF NOT EXISTS team (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS channel (
    id         TEXT PRIMARY KEY,
    team_id    TEXT NOT NULL REFERENCES team(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    -- open | internal | restricted,与 muster_route::Sensitivity 同源
    level      TEXT NOT NULL DEFAULT 'open',
    private    BOOLEAN NOT NULL DEFAULT FALSE,
    created_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_channel_team ON channel(team_id);

-- 角色绑定:作用域三选一(org / group:<team> / channel:<id>),
-- 与 muster_identity::RoleBinding 逐字段对应。
CREATE TABLE IF NOT EXISTS role_binding (
    id         BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    role       TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    scope_id   TEXT,
    created_ms BIGINT NOT NULL,
    UNIQUE (account_id, role, scope_kind, scope_id)
);

CREATE TABLE IF NOT EXISTS channel_member (
    channel_id TEXT NOT NULL REFERENCES channel(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    joined_ms  BIGINT NOT NULL,
    PRIMARY KEY (channel_id, account_id)
);

-- 消息。**channel_seq 是投递序,不是证据序**——证据序是各节点自己的哈希链,
-- 两者要求不同:链序必须防篡改,频道序只需每频道无空洞(供断线补拉)。
-- 别拿一个去实现另一个。
CREATE TABLE IF NOT EXISTS message (
    id          UUID PRIMARY KEY,
    channel_id  TEXT NOT NULL REFERENCES channel(id) ON DELETE CASCADE,
    channel_seq BIGINT NOT NULL,
    author_id   TEXT NOT NULL,
    role        TEXT NOT NULL,
    body        TEXT NOT NULL,
    run_id      TEXT,
    ts_ms       BIGINT NOT NULL,
    UNIQUE (channel_id, channel_seq)
);
CREATE INDEX IF NOT EXISTS idx_message_chan_seq ON message(channel_id, channel_seq DESC);

-- 每频道的序号发号器。单独一行做行锁,避免 MAX(seq)+1 的并发空洞。
CREATE TABLE IF NOT EXISTS channel_cursor (
    channel_id TEXT PRIMARY KEY REFERENCES channel(id) ON DELETE CASCADE,
    next_seq   BIGINT NOT NULL DEFAULT 1
);

-- 会议。媒体面在 LiveKit,这里只存**Muster 关心的那一半**:
-- 谁在场、密级、转写、产出了哪些任务。
CREATE TABLE IF NOT EXISTS meeting (
    id          UUID PRIMARY KEY,
    channel_id  TEXT NOT NULL REFERENCES channel(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    -- 会议密级:E3 棘轮只升不降(共享了 restricted 资源即抬升整场会)
    level       TEXT NOT NULL DEFAULT 'open',
    started_ms  BIGINT NOT NULL,
    ended_ms    BIGINT,
    -- LiveKit 房间名
    room        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_meeting_chan ON meeting(channel_id, started_ms DESC);

CREATE TABLE IF NOT EXISTS meeting_participant (
    meeting_id UUID NOT NULL REFERENCES meeting(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL,
    joined_ms  BIGINT NOT NULL,
    left_ms    BIGINT,
    PRIMARY KEY (meeting_id, account_id, joined_ms)
);

-- 转写正文。**这是正文存储侧**,与桌面壳的 state.db 同性质:
-- 可导出、可按保留期删除;审计链里只有它的哈希。
CREATE TABLE IF NOT EXISTS meeting_transcript (
    id          BIGSERIAL PRIMARY KEY,
    meeting_id  UUID NOT NULL REFERENCES meeting(id) ON DELETE CASCADE,
    speaker_id  TEXT NOT NULL,
    text        TEXT NOT NULL,
    ts_ms       BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transcript_meeting ON meeting_transcript(meeting_id, ts_ms);

-- 会议行动项。**这是提案,不是任务。**
--
-- 会上一句话不能直接变成在别人机器上跑的代码改动:转写会出错
-- (实测把"幂等键"转成"蜜等键"),会议发言是低保真输入;而且 Runner 在
-- 开发者机器上,服务端的 Agent 本来也跑不了任务。
-- 人确认那一步不是流程负担,是**授权边界**。
CREATE TABLE IF NOT EXISTS meeting_action_item (
    id           UUID PRIMARY KEY,
    meeting_id   UUID NOT NULL REFERENCES meeting(id) ON DELETE CASCADE,
    -- 行动项描述(模型从会议记录里提炼)
    text         TEXT NOT NULL,
    -- 会上提到的负责人。**可能是转错的名字**,只作提示,不作授权依据
    owner_hint   TEXT,
    -- 出处原话:人要能核对"它是不是听岔了"
    source_quote TEXT,
    -- proposed | confirmed | rejected | done
    status       TEXT NOT NULL DEFAULT 'proposed',
    decided_by   TEXT,
    decided_ms   BIGINT,
    -- 确认后由某个节点跑起来的 run;服务端只记号,不执行
    run_id       TEXT,
    created_ms   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_action_meeting ON meeting_action_item(meeting_id, created_ms);
CREATE INDEX IF NOT EXISTS idx_action_status ON meeting_action_item(status);

-- 全局投递序号(C2 SSE 恢复用)。
--
-- 为什么不用 BIGSERIAL:序列号是**事务外**分配的,拿到 5 的事务可能比拿到 6 的
-- 晚提交,于是消费者按 `id > last` 拉取时会永久跳过 5。这个坑在 outbox/CDC
-- 里很经典,而它丢的是消息。
--
-- 改用单行游标 + `UPDATE ... RETURNING`:锁持有到提交,**加锁顺序即提交顺序**,
-- 于是序号顺序就是可见顺序。代价是消息插入全局串行——团队规模下不值一提。
CREATE TABLE IF NOT EXISTS stream_cursor (
    id       INTEGER PRIMARY KEY DEFAULT 1,
    next_seq BIGINT NOT NULL DEFAULT 1,
    CHECK (id = 1)
);
INSERT INTO stream_cursor(id, next_seq) VALUES (1, 1) ON CONFLICT DO NOTHING;

ALTER TABLE message ADD COLUMN IF NOT EXISTS stream_seq BIGINT;
CREATE INDEX IF NOT EXISTS idx_message_stream ON message(stream_seq);

-- 这场会要不要 Agent。
--
-- **按钮不直接起进程。** 桌面壳自己 spawn 一个 Agent 的话,两个人开会就有两个
-- Agent 在同一个房间里各转各的,同一句话转两遍、纪要出现重复行;而且 Agent
-- 该在服务器上(架构文档的部署拓扑),不该在每个人的笔记本上。
-- 所以这里只记一个**意愿**,由常驻的 agent-daemon 去认领。
ALTER TABLE meeting ADD COLUMN IF NOT EXISTS wants_agent BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX IF NOT EXISTS idx_meeting_wants_agent ON meeting(wants_agent) WHERE wants_agent;

-- ---------------------------------------------------------------- provider 目录
--
-- **只存目录,不存密钥。** `api_key_env` 是环境变量的**名字**,值永远只在
-- 各节点自己的环境里——服务端被攻破也拿不到任何模型凭据,与"服务端不持有
-- 源码"是同一条姿态。
--
-- 为什么要集中:`locality` 决定 restricted 密级的内容能不能出门,而在此之前
-- 它由**各节点自己的配置文件声明**。谁在自己机器上把一个云端 base_url 标成
-- local,restricted 的会议内容就照常发出去,而系统会报告"本地"。
-- 铁律二说"绝不静默升云"——代码严格执行了它,但判断依据的来源方位不对。
--
-- 写这张表要 ChangePolicy 权限:它和 cloud_max 是同一个授权面,
-- 一个定"什么密级能上云",一个定"什么算云"。
CREATE TABLE IF NOT EXISTS provider (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL DEFAULT 'openai_compat',
    base_url      TEXT NOT NULL,
    model         TEXT NOT NULL,
    -- 'local' | 'cloud'
    locality      TEXT NOT NULL,
    display_name  TEXT,
    -- 环境变量**名**,不是值。留空表示该通道不需要密钥(如本机 Ollama)
    api_key_env   TEXT,
    timeout_secs  BIGINT NOT NULL DEFAULT 120,
    -- 停用而不删除:删掉就查不出"上周那次任务用的是哪条通道"
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    is_default    BOOLEAN NOT NULL DEFAULT FALSE,
    created_ms    BIGINT NOT NULL,
    updated_ms    BIGINT NOT NULL,
    CHECK (locality IN ('local', 'cloud'))
);

-- 默认通道只能有一个。靠唯一索引而不是应用层检查:两个并发的
-- "设为默认"请求都读到"当前没有默认",然后都写进去——应用层挡不住
CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_single_default ON provider(is_default) WHERE is_default;
