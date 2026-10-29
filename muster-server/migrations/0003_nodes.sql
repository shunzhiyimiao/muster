-- 2026-10-29 块 2:节点(docs/节点与工牌-公用私有值班.md 实施顺序 1)。
-- 只加表、加列,不动已有数据。

-- 节点:一台登记到组织的机器。公钥用于会话验签(将来也签审计锚点);
-- owner_account 为空即组织节点。status:pending(等批准)/ approved / revoked。
CREATE TABLE IF NOT EXISTS node (
    id                 UUID PRIMARY KEY,
    name               TEXT NOT NULL,
    owner_account      TEXT REFERENCES account(id) ON DELETE SET NULL,
    pubkey             BYTEA NOT NULL UNIQUE,
    capabilities       JSONB NOT NULL DEFAULT '{}'::jsonb,
    version            TEXT,
    status             TEXT NOT NULL DEFAULT 'pending',
    -- 设备码:批准前有效,批准后清空
    user_code          TEXT UNIQUE,
    -- 节点轮询批准状态用的令牌,只存哈希
    poll_token_hash    TEXT,
    approved_by        TEXT,
    approved_ms        BIGINT,
    last_seen_ms       BIGINT,
    last_progress_ms   BIGINT,
    last_progress_kind TEXT,
    created_ms         BIGINT NOT NULL
);

-- 工牌(kind=agent 的账号)住在哪个节点
ALTER TABLE account ADD COLUMN IF NOT EXISTS node_id UUID REFERENCES node(id) ON DELETE SET NULL;

-- 组织设置(键值):duty_badge = 值班工牌
CREATE TABLE IF NOT EXISTS org_setting (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_by TEXT,
    updated_ms BIGINT NOT NULL
);

-- 最近一次「请 Agent」的时刻:节点据此判断熔断该不该解除(人又点了一次 = 解除)
ALTER TABLE meeting ADD COLUMN IF NOT EXISTS agent_requested_ms BIGINT;

-- 节点上报的熔断:对哪场会、什么错、连续几次。人再点「请 Agent」即清掉。
CREATE TABLE IF NOT EXISTS node_breaker (
    node_id     UUID NOT NULL REFERENCES node(id) ON DELETE CASCADE,
    meeting_id  UUID NOT NULL REFERENCES meeting(id) ON DELETE CASCADE,
    error_kind  TEXT NOT NULL,
    failures    INT NOT NULL,
    reported_ms BIGINT NOT NULL,
    PRIMARY KEY (node_id, meeting_id)
);
