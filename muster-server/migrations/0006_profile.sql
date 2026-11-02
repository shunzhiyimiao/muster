-- 2026-11-03 用户资料、登录流水、找回口令(docs/数据库设计.md)。只加表、加列,不动已有数据。
--
-- 账号(account)是"能登录的主体",资料(user_profile)是"关于这个人的信息",分两张表:
-- agent / service 账号没有邮箱和头像,混进 account 就是一堆对它们没意义的空列。

CREATE TABLE IF NOT EXISTS user_profile (
    account_id         TEXT PRIMARY KEY REFERENCES account(id) ON DELETE CASCADE,
    -- 小写规范化后存;唯一(见下面的部分唯一索引);可空
    email              TEXT,
    email_verified     BOOLEAN NOT NULL DEFAULT FALSE,
    -- 头像文件在 MUSTER_AVATARS_DIR 下,库里只存文件名(正文不进库,与录像同一姿态)
    avatar_path        TEXT,
    department         TEXT,
    title              TEXT,
    bio                TEXT,
    -- 偏好的模型通道:provider 目录里的 id;空 = 跟组织默认。通道停用不删,所以用 SET NULL 兜底即可
    preferred_provider TEXT REFERENCES provider(id) ON DELETE SET NULL,
    created_ms         BIGINT NOT NULL,
    updated_ms         BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_user_profile_email ON user_profile (lower(email)) WHERE email IS NOT NULL;

-- 登录流水:成功与失败都记。**只记存在的账号**——不存在的账号不记,否则这张表就成了枚举器的日志。
CREATE TABLE IF NOT EXISTS login_event (
    id         BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    ok         BOOLEAN NOT NULL,
    -- 失败原因:bad_password | disabled | pending;成功为空
    reason     TEXT,
    ip         TEXT,
    user_agent TEXT,
    ts_ms      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_login_event_account ON login_event(account_id, ts_ms DESC);

-- 最近登录的快照放在 account 上:列表页一眼看到,不必每行去聚合流水
ALTER TABLE account ADD COLUMN IF NOT EXISTS last_login_ms BIGINT;
ALTER TABLE account ADD COLUMN IF NOT EXISTS last_login_ip TEXT;
-- 自上次成功登录以来连续失败的次数;成功即清零
ALTER TABLE account ADD COLUMN IF NOT EXISTS failed_logins INT NOT NULL DEFAULT 0;

-- 找回口令:一次性令牌,**只存哈希**;30 分钟有效,用过即废。
-- issued_by 非空 = 管理员签发(没有邮件通道时的路径);为空 = 本人凭邮箱申请。
CREATE TABLE IF NOT EXISTS password_reset (
    id         UUID PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    issued_by  TEXT,
    expires_ms BIGINT NOT NULL,
    used_ms    BIGINT,
    created_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_password_reset_account ON password_reset(account_id, created_ms DESC);
