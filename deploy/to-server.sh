#!/usr/bin/env bash
# 从本机把 Muster 部署到一台公网服务器上。**在你的 Mac 上跑,不是在服务器上跑。**
#
#   ./deploy/to-server.sh root@1.2.3.4 muster.example.com
#
# ## 为什么用 rsync 推,而不是在服务器上 git clone
#
# 仓库是私有的。要在服务器上 clone,就得先在那台机器上配部署密钥或 PAT——
# 多一份长期有效的凭据放在一台对着公网的机器上,而它唯一的用途是拉代码。
# 推过去就没有这回事:服务器上不存在任何能访问仓库的凭据。
#
# ## 这个脚本做什么
#
# 装 Docker(如果没有)→ 推源码 → 生成密钥 → 起服务 → 验一遍。
# 可以重复跑:第二次起只同步改动、不覆盖已生成的密钥。
set -euo pipefail

# **变量后面紧跟中文时必须写 ${VAR}。** 不加花括号的话 shell 会把汉字的字节
# 当成标识符的一部分,报 "unbound variable" 而且指的是一个你没写过的变量名。
# 这个坑在 dev-up.sh 上已经踩过一次。
ok()   { printf '\033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '\033[31m✗\033[0m %s\n' "$*" >&2; }
step() { printf '\n\033[1m▶ %s\033[0m\n' "$*"; }
die()  { bad "$*"; exit 1; }

SERVER=${1:-}
DOMAIN=${2:-}
[ -n "$SERVER" ] && [ -n "$DOMAIN" ] || die "用法:$0 <user@host> <域名>
例:$0 root@1.2.3.4 muster.example.com"

REMOTE_DIR=/opt/muster
LOCAL_DIR=$(cd "$(dirname "$0")/.." && pwd)

# ---------------------------------------------------------------- 预检
#
# 全部放在动手之前。DNS 没配好就起 Caddy 的话,它会去 Let's Encrypt 碰一鼻子灰,
# **而失败次数是有配额的**——撞满了要等一小时才能再试。宁可现在多花十秒。

step "预检"

ssh -o BatchMode=yes -o ConnectTimeout=8 "$SERVER" true 2>/dev/null \
  || die "连不上 ${SERVER}。先确认 ssh ${SERVER} 能免密登录(公钥已装)。"
ok "ssh 通"

SERVER_IP=$(ssh "$SERVER" "curl -s4 --max-time 8 ifconfig.me || hostname -I | awk '{print \$1}'")
[ -n "$SERVER_IP" ] || die "取不到服务器的公网 IP"
ok "服务器公网 IP ${SERVER_IP}"

# 两条 A 记录都要指过来。少一条的表现是"应用能开、会议进不去",
# 而那时你会去查 LiveKit 配置——方向完全错。
dns_fail=0
for h in "$DOMAIN" "livekit.$DOMAIN"; do
  got=$(dig +short "$h" A | tail -1)
  if [ -z "$got" ]; then
    bad "$h 没有 A 记录"
    dns_fail=1
  elif [ "$got" != "$SERVER_IP" ]; then
    bad "$h 解析到 $got,不是 $SERVER_IP"
    dns_fail=1
  else
    ok "$h → $got"
  fi
done
if [ "$dns_fail" = 1 ]; then
  die "DNS 没配好。两条 A 记录都要指向 ${SERVER_IP}:
    ${DOMAIN}          A  ${SERVER_IP}
    livekit.${DOMAIN}  A  ${SERVER_IP}
  改完等生效(dig 查到就算)再重跑。"
fi

# 80/443 被占的话 Caddy 起不来,而报错要翻容器日志才看得见
busy=$(ssh "$SERVER" "ss -lntp 2>/dev/null | awk '\$4 ~ /:(80|443)\$/ {print \$4}'" || true)
if [ -n "$busy" ]; then
  bad "服务器上 80/443 已被占用:"
  echo "$busy" | sed 's/^/    /'
  die "先停掉占用的服务(常见是 nginx / apache),再重跑。"
fi
ok "80/443 空闲"

# ---------------------------------------------------------------- Docker

step "Docker"

if ssh "$SERVER" "command -v docker >/dev/null && docker compose version >/dev/null 2>&1"; then
  ok "已装 $(ssh "$SERVER" 'docker --version')"
else
  echo "  服务器上没有 Docker,正在装(用官方脚本)…"
  ssh "$SERVER" "curl -fsSL https://get.docker.com | sh" >/dev/null 2>&1 \
    || die "Docker 装失败。手动装完再重跑:https://docs.docker.com/engine/install/"
  ok "已装 $(ssh "$SERVER" 'docker --version')"
fi

# ---------------------------------------------------------------- 源码

step "同步源码到 ${REMOTE_DIR}"

ssh "$SERVER" "mkdir -p $REMOTE_DIR"
# --delete 让服务器上的树与本机一致(删掉的文件那边也删),
# 但**排除 .env 和数据卷**:那些是服务器上的东西,不该被本机的状态覆盖
rsync -az --delete \
  --exclude target --exclude node_modules --exclude .git \
  --exclude 'apps/desktop' --exclude 'deploy/.env' \
  "$LOCAL_DIR/" "$SERVER:$REMOTE_DIR/"
ok "已同步(不含 target / node_modules / .git / 桌面壳)"

# ---------------------------------------------------------------- 密钥

step "配置"

if ssh "$SERVER" "[ -f $REMOTE_DIR/deploy/.env ]"; then
  ok "deploy/.env 已存在,保持不动(要换密钥就手动改)"
  ssh "$SERVER" "sed -i 's|^MUSTER_DOMAIN=.*|MUSTER_DOMAIN=$DOMAIN|' $REMOTE_DIR/deploy/.env"
else
  # 在服务器上生成,不经过本机——少一处它们出现过的地方
  ssh "$SERVER" "cd $REMOTE_DIR && {
    gen() { openssl rand -base64 48 | tr -d '\n=+/' | head -c 48; }
    cat > deploy/.env <<EOF
MUSTER_DOMAIN=$DOMAIN
POSTGRES_PASSWORD=\$(gen)
MUSTER_JWT_SECRET=\$(gen)
LIVEKIT_API_KEY=musterkey
LIVEKIT_API_SECRET=\$(gen)
WHISPER_MODEL=Systran/faster-whisper-small
RUST_LOG=muster_server=info
EOF
    chmod 600 deploy/.env
  }"
  ok "已生成 deploy/.env(密钥在服务器上生成,本机不留副本)"
fi

# ---------------------------------------------------------------- 起服务

step "构建并启动(第一次要几分钟,在编 Rust)"

ssh "$SERVER" "cd $REMOTE_DIR && docker compose --env-file deploy/.env \
  -f deploy/docker-compose.prod.yml up -d --build" 2>&1 | tail -20

# ---------------------------------------------------------------- 验收
#
# 起来了不等于能用。下面每一条都是真发一次请求,不看容器状态——
# 容器 Running 而应用在崩溃重启,`ps` 是看不出来的。

step "验收"

echo "  等证书签发…"
https_ok=0
for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 "https://$DOMAIN/health" 2>/dev/null || echo 000)
  if [ "$code" = "200" ]; then https_ok=1; break; fi
  sleep 5
done

if [ "$https_ok" = 1 ]; then
  ok "https://$DOMAIN/health 通了(证书已签发)"
else
  bad "https://$DOMAIN/health 拿不到 200"
  echo "  看日志:ssh $SERVER 'cd $REMOTE_DIR && docker compose --env-file deploy/.env -f deploy/docker-compose.prod.yml logs caddy muster-server | tail -40'"
  echo "  最常见的两个原因:80 端口进不来(云主机安全组),或 DNS 还没生效。"
  exit 1
fi

curl -s -o /dev/null -w "  网页参会端 → HTTP %{http_code}\n" "https://$DOMAIN/web"
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 "https://livekit.$DOMAIN" 2>/dev/null || echo 000)
[ "$code" != "000" ] && ok "livekit.${DOMAIN} 的证书也好了(HTTP ${code}——它不认这个请求是正常的)" \
                     || bad "livekit.${DOMAIN} 连不上,会议会进不去"

# ---------------------------------------------------------------- 首个账号

step "组织所有者"

DC="docker compose --env-file deploy/.env -f deploy/docker-compose.prod.yml"
OWNER_PW=$(openssl rand -base64 18 | tr -d '\n=+/' | head -c 18)

# bootstrap 只在 owner 不存在时成功。**不必先探测**——探测本身要一个口令,
# 而我们没有。让它自己失败,比先发一次注定失败的登录清楚。
if ssh "$SERVER" "cd $REMOTE_DIR && $DC exec -T muster-server \
     /usr/local/bin/muster-bootstrap owner '${OWNER_PW}' 老板" >/dev/null 2>&1; then
  # **建完要验一次再报口令。** 账号可能是上次用别的口令建的,那时 bootstrap
  # 会静默失败,而脚本照样打印一个登不进去的口令——让人对着正确的界面
  # 输错误的凭据,查半天。(dev-up.sh 上踩过同一个坑)
  if curl -s --max-time 8 -X POST "https://$DOMAIN/auth/login" \
       -H 'content-type: application/json' \
       -d "{\"id\":\"owner\",\"password\":\"$OWNER_PW\"}" | grep -q '"token"'; then
    ok "已创建组织所有者(已验证可登录)"
    printf '\n  \033[1m账号 owner  口令 %s\033[0m\n' "$OWNER_PW"
    echo "  这个口令只显示这一次,现在记下来。"
  else
    bad "owner 建出来了却登不进去。别拿上面那个口令去试——它是错的。"
    echo "  进服务器看日志:ssh ${SERVER} 'cd ${REMOTE_DIR} && $DC logs muster-server | tail -30'"
  fi
else
  ok "owner 已存在,跳过(要重置口令进服务器手动改)"
fi

step "完成"
cat <<EOF
  网页参会端   https://$DOMAIN
  桌面壳       侧栏填 https://$DOMAIN

  下一步(建团队和频道,否则登进去是空的):
    ssh $SERVER
    cd $REMOTE_DIR
    C="docker compose --env-file deploy/.env -f deploy/docker-compose.prod.yml exec muster-server"
    \$C muster-admin login owner '<口令>'
    \$C muster-admin team-add platform 平台组
    \$C muster-admin channel-add platform-main platform 主频道 internal
    \$C muster-admin account-add alice '<口令>' Alice human
    \$C muster-admin grant alice member org

  云主机记得在**安全组**里放行:80、443/tcp,7882、3478/udp。
  UDP 那两个不放行的话,登录和界面都正常,只有会议进不去——
  报 "could not establish pc connection"。
EOF
