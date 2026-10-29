-- 2026-10-29 块 3:派发单——行动项的确认权收窄到被派到的本人。
-- 保留表名 meeting_action_item 与已有接口路径,只加列;老行的 status 'confirmed' 继续认。

ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS assignee TEXT REFERENCES account(id) ON DELETE SET NULL;
-- self(本人自己做)| private(交给私有 Agent,未实现,留空)
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS accepted_mode TEXT;
-- 来源的密级(会议密级),将来任务从这个密级起步
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS sensitivity TEXT;
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS source_kind TEXT NOT NULL DEFAULT 'meeting';
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS reject_reason TEXT;
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS assigned_by TEXT;
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS assigned_ms BIGINT;
-- proposed 超过这个时刻没人处理 ⇒ expired(提出时 +72 小时)
ALTER TABLE meeting_action_item ADD COLUMN IF NOT EXISTS expires_ms BIGINT;
CREATE INDEX IF NOT EXISTS idx_action_assignee ON meeting_action_item(assignee, status);

-- 会议主持人(建会的人):assignee 为空时由他指派
ALTER TABLE meeting ADD COLUMN IF NOT EXISTS host TEXT;
