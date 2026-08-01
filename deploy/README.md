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

---

# 怎么测

```bash
./deploy/dev-up.sh          # 起环境 + 建账号(幂等,可反复跑)
./deploy/dev-up.sh --clean  # 连数据一起清掉重来
```

脚本**建完账号会验一次登录再报口令**——账号可能是上次用别的口令建的,
那时 `account-add` 静默失败,而脚本照样打印一个登不进去的口令,
让人对着正确的界面输错误的凭据查半天。

## 排查顺序:从最便宜、最能隔离的一步开始

整条链最容易出问题的是**转写后端接口对不对**,而那与音视频无关。所以别一上来
就开会——按下面的顺序走,断在哪一步就知道是哪一层。

| # | 验什么 | 怎么验 | 对了应该看到 |
|---|---|---|---|
| 1 | 依赖起来了 | `docker compose ps` | 三个容器 Up |
| 2 | **转写通不通** | `say -o a.aiff "各位好"; afconvert -f WAVE -d LEI16@16000 -c 1 a.aiff a.wav`<br>`cargo run -p muster-meeting-agent --example transcribe_file -- a.wav` | 打印出中文,外发 0 字节 |
| 3 | 演习真的拦得住 | 同上加 `MUSTER_STT_CLOUD=1 MUSTER_DRILL=1` | ⛔ fail-closed 拒绝 |
| 4 | 服务端活着 | `curl localhost:8787/health` | `{"ok":true,...}` |
| 5 | 桌面壳能连 | `pnpm tauri dev` → 侧栏「单机模式」→ 填地址与 alice | 状态变「已连接团队服务器」 |
| 6 | 消息真到了服务端 | 桌面壳发一条,再 `curl .../channels/platform-main/messages` | 两边看到同一条 |
| 7 | 会议能建能进 | 团队 → 会议室 → 发起会议 | 进入会议室,显示「已入房间」 |
| 8 | Agent 在会里 | 拿会议 id 跑 agent | 日志出现「已入房间」 |
| 9 | **真人说话 → 转写** | 点开麦说一句 | 右侧实时纪要出现你说的话 |
| 10 | 叫它名字 | 说「小七,刚才说了什么」 | 纪要里出现小七的回答 |
| 11 | 散会提炼 | Ctrl-C 停掉 agent | 打印行动项,**待人确认** |

第 2 步最值得先做:它不需要 LiveKit、不需要开会、不需要麦克风,却能同时验证
provider、路由、密级和 whisper 后端。

## 常见的坑(都是真机上撞过的)

| 现象 | 原因 |
|---|---|
| `role "muster" does not exist` | 开发机上另有本地 Postgres 占着 5432。compose 已改用 **5433** |
| 开麦时应用直接崩 | macOS 缺 `NSMicrophoneUsageDescription`(已补);若仍崩,去系统设置放行麦克风 |
| 会议里两个身份互踢 | 两端用了同一个账号 ⇒ LiveKit `DuplicateIdentity`。**Agent 要用自己的账号** |
| 转写出繁体、术语错 | 设 `MUSTER_STT_PROMPT` 给一句简体的领域提示 |
| 第一次转写特别慢 | 在下模型。**别拿冷启动数据下结论**,热态 base 快 7 倍于实时 |
| `could not establish pc connection` | **信令通了、媒体没通**——两条路只断了后者。两个原因,都在 compose 里修了:①LiveKit 默认走 ICE 端口段 50000-60000,Docker 下等于要映射一万个端口 ⇒ 改 `rtc.udp_port: 7882` 单端口复用;②不指定 `rtc.node_ip` 时它广播的候选是容器内网 IP(172.x),宿主机上的浏览器路由不到 ⇒ 显式设成宿主机可达的地址。**排查时看 LiveKit 日志里 `[local][selected]` 那个候选地址**,不是宿主机能到的就是这个问题 |
| `令牌无效:ExpiredSignature` | 令牌 12 小时过期。Agent 改用 `MUSTER_ACCOUNT` + `MUSTER_PASSWORD` **当场登录**,不吃预签的令牌 |
| 喊了名字它没反应 | 转写可能把逗号吃了或名字转错。用 `MUSTER_AGENT_ALIASES` 把常见错法列进去 |
