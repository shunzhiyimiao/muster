# 服务端本地起步

```bash
cd deploy && docker compose up -d          # PostgreSQL + LiveKit + whisper
export DATABASE_URL=postgres://muster:muster@localhost:5433/muster   # 5433:见 compose 注释
export MUSTER_JWT_SECRET=$(openssl rand -hex 32)   # 少于 32 字符会被拒绝启动
cargo run -p muster-server
```

`GET /health` 应答 `{"ok":true,...}` 即启动成功。迁移在连接时自动执行。

## 第一个账号

服务端**不内置任何默认账号**——默认口令是最常见的入侵路径。用 `bootstrap`
示例建第一个组织所有者(它是唯一能绕过鉴权的入口,且只在库里没有任何账号时可用):

```bash
cargo run -p muster-server --example bootstrap -- <账号> <口令> <显示名>
```

之后一切走 `POST /auth/login` 拿令牌,或直接用管理 CLI:

```bash
export MUSTER_SERVER=http://localhost:8787
cargo run -p muster-server --example admin -- login owner 你的口令
cargo run -p muster-server --example admin -- accounts
cargo run -p muster-server --example admin -- grant bob approver group 平台组
cargo run -p muster-server --example admin -- revoke bob approver group 平台组
cargo run -p muster-server --example admin -- disable bob
```

不带参数跑 `admin` 会打出完整命令表。**所有权限变更都会写进服务端审计链**
(`badge.update` 事件):谁改的、改了谁、改成什么(哈希),一条 SQL 可查。

服务端有自己的审计链(它也是一个节点),默认落在 `./muster-server-audit.db`,
可用 `MUSTER_SERVER_AUDIT_DB` 指定。链坏了服务端拒绝启动——
不在坏账本上继续记账。

## 配置

| 变量 | 必需 | 说明 |
|---|---|---|
| `DATABASE_URL` | 是 | 缺失即拒绝启动。注意端口是 **5433**——开发机常已有本地 Postgres 占着 5432 |
| `MUSTER_JWT_SECRET` | 是 | ≥32 字符。**不提供默认密钥**——默认密钥等于没有认证 |
| `MUSTER_BIND` | 否 | 默认 `127.0.0.1:8787` |
| `LIVEKIT_URL` | 会议需要 | 如 `ws://localhost:7880` |
| `LIVEKIT_API_KEY` | 会议需要 | compose 的 `--dev` 模式是 `devkey` |
| `LIVEKIT_API_SECRET` | 会议需要 | ≥32 字符,须与 compose 里 `LIVEKIT_KEYS` 的值一致。**仓库里那把是公开的,内网部署必须换** |

## 会议

```
POST /channels/:cid/meetings   {"title":"..."}   → 建会议(密级继承频道)
POST /meetings/:mid/join                          → 拿 LiveKit 入会令牌
POST /meetings/:mid/transcript {"speaker_id","text"}
POST /meetings/:mid/level      {"level","cause"}  → 密级抬升(只升不降)
POST /meetings/:mid/end
```

令牌里的 `canPublish` **不是配置项**,是 `muster_identity::can()` 判定结果的
直接映射:能在该频道发言的人才能开麦。会议密级 ≥ restricted 时禁止录制——
录像是长期留存、极易被搬走的正文,这不该是与会者的选择。

**转写这一步服务端不代劳**:`/transcript` 只收文本,音频转文本必须由调用方
经 `muster_route` 完成。这样服务端无从绕过密级路由,演习模式下云端 STT 会被
fail-closed 直接拒掉。

## 现在**没有**的东西

见 `muster-server/src/lib.rs` 的「诚实边界」。要点:无 Outbox、无断线补拉、
无节点链锚定、无速率限制。本版目标是先跑通功能,不是服务质量——
但那份清单必须在上线前逐条清掉。
