# 服务端本地起步

```bash
cd deploy && docker compose up -d          # PostgreSQL + LiveKit + whisper
export DATABASE_URL=postgres://muster:muster@localhost:5432/muster
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

之后一切走 `POST /auth/login` 拿令牌。

## 配置

| 变量 | 必需 | 说明 |
|---|---|---|
| `DATABASE_URL` | 是 | 缺失即拒绝启动,不用默认值悄悄连到别处 |
| `MUSTER_JWT_SECRET` | 是 | ≥32 字符。**不提供默认密钥**——默认密钥等于没有认证 |
| `MUSTER_BIND` | 否 | 默认 `127.0.0.1:8787` |

## 现在**没有**的东西

见 `muster-server/src/lib.rs` 的「诚实边界」。要点:无 Outbox、无断线补拉、
无节点链锚定、无速率限制。本版目标是先跑通功能,不是服务质量——
但那份清单必须在上线前逐条清掉。
