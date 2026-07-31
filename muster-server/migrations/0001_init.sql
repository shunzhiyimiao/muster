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
