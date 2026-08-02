#!/usr/bin/env bash
# 本机把整套跑起来。**幂等**:重复执行不会重复建账号,可以随时再跑一次。
#
#   ./deploy/dev-up.sh          起环境 + 建账号 + 打印下一步
#   ./deploy/dev-up.sh --clean  连数据一起清掉重来
set -uo pipefail
cd "$(dirname "$0")/.."

SERVER=http://localhost:8787
STT=http://localhost:9000/v1

# 状态走 stderr,stdout 只留数据:否则 $(...) 捕获返回值时会把提示一起吃进去
say() { printf "\n\033[1m%s\033[0m\n" "$*" >&2; }
ok()  { printf "  ✓ %s\n" "$*" >&2; }
bad() { printf "  ✗ %s\n" "$*" >&2; }

if [ "${1:-}" = "--clean" ]; then
  say "清掉旧数据"
  (cd deploy && docker compose down -v) >/dev/null 2>&1
  rm -f /tmp/muster-dev-audit.db ~/.muster/admin-token
fi

# 局域网 IP:两台机器一起开会时,服务端与 LiveKit 都必须用它对外
# (127.0.0.1 只有本机连得上,第二台会报 "could not establish pc connection")
LAN_IP=$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null \
  || ifconfig 2>/dev/null | grep -oE 'inet 192\.168\.[0-9.]+' | head -1 | awk '{print $2}')
LAN_IP=${LAN_IP:-127.0.0.1}
export LIVEKIT_NODE_IP="$LAN_IP"

say "1/5 起依赖(PostgreSQL / LiveKit / whisper)"
ok "局域网地址 ${LAN_IP}(第二台机器用它连过来)"
if ! docker info >/dev/null 2>&1; then
  bad "Docker 没在跑。先打开 Docker Desktop,再重跑本脚本。"; exit 1
fi
(cd deploy && docker compose up -d) >/dev/null 2>&1 || { bad "compose 起不来"; exit 1; }
ok "容器已启动"

printf "  等 PostgreSQL"
for _ in $(seq 1 40); do
  PGPASSWORD=muster psql -h 127.0.0.1 -p 5433 -U muster -d muster -c 'select 1' >/dev/null 2>&1 && break
  printf "."; sleep 2
done
echo
PGPASSWORD=muster psql -h 127.0.0.1 -p 5433 -U muster -d muster -c 'select 1' >/dev/null 2>&1 \
  && ok "PostgreSQL 5433 可连" || { bad "PostgreSQL 连不上"; exit 1; }

printf "  等 whisper(首次要下模型,可能几分钟)"
for _ in $(seq 1 90); do
  curl -s -m 3 "$STT/models" >/dev/null 2>&1 && break
  printf "."; sleep 4
done
echo
curl -s -m 5 "$STT/models" >/dev/null 2>&1 && ok "whisper 就绪" || bad "whisper 还没起来(转写会失败,其余可继续)"

say "2/5 环境变量"
cat > /tmp/muster-dev.env <<'ENV'
export DATABASE_URL=postgres://muster:muster@localhost:5433/muster
export MUSTER_JWT_SECRET=devjwt_0123456789abcdef0123456789abcdef
# **必须是局域网地址,不能是 localhost。** 这个地址会**原样发给每一个客户端**
# (入会票里的 url),而别的机器上的 localhost 是它自己 —— 症状是
# "could not establish signal connection: Failed to fetch"。
# 注意它和 node_ip 是两回事:这条是**信令**(WebSocket 到 7880),
# node_ip 是**媒体**(ICE 候选)。两个都错过一次,报错长得完全不一样。
export LIVEKIT_URL=ws://LAN_IP_PLACEHOLDER:7880
export LIVEKIT_API_KEY=devkey
export LIVEKIT_API_SECRET=devsecret_0123456789abcdef0123456789abcdef
export MUSTER_SERVER_AUDIT_DB=/tmp/muster-dev-audit.db
export MUSTER_SERVER=http://localhost:8787
# 监听 0.0.0.0:局域网里的第二台机器才连得上(默认只听 127.0.0.1)
export MUSTER_BIND=0.0.0.0:8787
ENV
sed -i '' "s|LAN_IP_PLACEHOLDER|${LAN_IP}|" /tmp/muster-dev.env
ok "写入 /tmp/muster-dev.env(密钥是开发用的,内网部署必须换)"
# shellcheck disable=SC1091
source /tmp/muster-dev.env

say "3/5 起 collab-server"
if curl -s -m 2 "$SERVER/health" >/dev/null 2>&1; then
  ok "已经在跑"
else
  (cargo run -q -p muster-server > /tmp/muster-server.log 2>&1 &)
  printf "  编译并启动"
  for _ in $(seq 1 90); do
    curl -s -m 2 "$SERVER/health" >/dev/null 2>&1 && break
    printf "."; sleep 3
  done
  echo
  curl -s -m 3 "$SERVER/health" >/dev/null 2>&1 \
    && ok "已启动(日志 /tmp/muster-server.log)" \
    || { bad "起不来,看 /tmp/muster-server.log"; tail -5 /tmp/muster-server.log; exit 1; }
fi

say "4/5 账号与频道(已存在就跳过)"
adm() { cargo run -q -p muster-server --example admin -- "$@" 2>&1 | tail -1; }
cargo run -q -p muster-server --example bootstrap -- owner ownerpass123 老板 >/dev/null 2>&1 \
  && ok "创建组织所有者 owner / ownerpass123" || ok "owner 已存在"
adm login owner ownerpass123 >/dev/null 2>&1 && ok "已登录 owner"
adm team-add platform 平台组 >/dev/null 2>&1
adm channel-add platform-main platform 主频道 internal >/dev/null 2>&1
ok "团队/频道就绪(#主频道 · internal)"
# 建完要**验一次**再往外报口令。账号可能是上次用别的口令建的,
# 那时 account-add 会静默失败,而脚本照样打印一个登不进去的口令——
# 让人对着正确的界面输错误的凭据,查半天。
check_login() {
  local id="$1" pw="$2" label="$3"
  adm account-add "$id" "$pw" "$label" "${4:-human}" >/dev/null 2>&1
  adm grant "$id" member org >/dev/null 2>&1
  local tok
  tok=$(curl -s -X POST "$SERVER/auth/login" -H 'content-type: application/json' \
    -d "{\"id\":\"$id\",\"password\":\"$pw\"}" \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('token',''))" 2>/dev/null)
  if [ -n "$tok" ]; then
    ok "${id} / ${pw}(已验证可登录)"
    echo "$tok"
  else
    bad "$id 存在但口令不是 ${pw}——多半是上次用别的口令建的。"
    bad "  要么用你记得的那个口令,要么 ./deploy/dev-up.sh --clean 重来。"
    echo ""
  fi
}
check_login alice alicepass123 Alice human >/dev/null
check_login bob bobpass123 Bob human >/dev/null
AGENT_TOKEN=$(check_login A-007 agentpass123 小七 agent)

echo "$AGENT_TOKEN" > /tmp/muster-agent-token
[ -n "$AGENT_TOKEN" ] || bad "拿不到 A-007 的令牌,会议 Agent 跑不起来"

say "5/5 下一步"
cat <<NEXT
  桌面壳(第一个人):
    cd apps/desktop && pnpm tauri dev
    侧栏顶部「单机模式」→ 填 http://${LAN_IP}:8787,账号 alice / alicepass123

  第二个人:
    另一台机器 → 同样填 http://${LAN_IP}:8787,账号 bob / bobpass123
    同一台机器再开一个 → 必须换会话文件,否则两个壳互相挤掉:
      MUSTER_SESSION_FILE=/tmp/muster-bob.json pnpm tauri dev

  会议 Agent(先在桌面壳里发起会议,拿到会议 id 再跑):
    source /tmp/muster-dev.env
    export MUSTER_TOKEN=\$(cat /tmp/muster-agent-token)
    export MUSTER_STT_MODEL=Systran/faster-whisper-base
    export MUSTER_STT_PROMPT="以下是简体中文的技术周会记录。"
    export MUSTER_PROVIDER_CONFIG=<你的 provider.toml>   # 不配则只转写不作答
    cargo run -p muster-meeting-agent --features livekit --example agent -- <会议id>

  只想验转写通不通(最便宜的一步,不用开会):
    cargo run -p muster-meeting-agent --example transcribe_file -- <某个.wav>
NEXT
