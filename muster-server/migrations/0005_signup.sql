-- 自助注册(2026-11-03):账号可以处于「待批准」。
-- 注册模式(closed | approval | open)存在 org_setting,key = 'registration';缺省按 approval 处理。
ALTER TABLE account ADD COLUMN IF NOT EXISTS pending BOOLEAN NOT NULL DEFAULT FALSE;
