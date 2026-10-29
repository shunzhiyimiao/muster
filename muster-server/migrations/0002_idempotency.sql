-- 2026-10-29 块 1:消息 POST 幂等。
-- 同一作者带同一 Idempotency-Key 的重复请求只落一条;没带键的照旧(部分唯一索引不约束空值)。
-- 只加列、加索引,不动已有数据。
ALTER TABLE message ADD COLUMN IF NOT EXISTS idempotency_key TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS uq_message_author_idem
    ON message(author_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
