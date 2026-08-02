#!/usr/bin/env bash
# 多人会议测试的**验收脚本**:每一项自己判定通过与否,不靠眼睛看。
#
#   ./deploy/check.sh            环境自检(开测前跑)
#
# 在**第二台机器**上也能跑,指到第一台即可:
#   MUSTER_SERVER=http://<第一台IP>:8787 ./deploy/check.sh
#   ./deploy/check.sh <会议id>   针对某场会议查结果(测的过程中随时跑)
set -uo pipefail
cd "$(dirname "$0")/.."
SERVER=${MUSTER_SERVER:-http://localhost:8787}
PGC="PGPASSWORD=muster psql -h 127.0.0.1 -p 5433 -U muster -d muster -tA"

ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
bad()  { printf "  \033[31m✗\033[0m %s\n" "$*"; }
warn() { printf "  \033[33m!\033[0m %s\n" "$*"; }
sec()  { printf "\n\033[1m%s\033[0m\n" "$*"; }

TOK=$(curl -s -m 5 -X POST "$SERVER/auth/login" -H 'content-type: application/json' \
  -d '{"id":"owner","password":"ownerpass123"}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin).get('token',''))" 2>/dev/null)

sec "环境"
LAN=$(ipconfig getifaddr en0 2>/dev/null || echo "")
[ -n "$LAN" ] && ok "局域网地址 $LAN(第二台填 http://$LAN:8787)" || warn "取不到局域网地址"

if lsof -i :8787 2>/dev/null | grep -q "LISTEN"; then
  if lsof -i :8787 2>/dev/null | grep LISTEN | grep -q "\*:"; then
    ok "服务端听 0.0.0.0(第二台连得上)"
  else
    bad "服务端只听 127.0.0.1 —— 第二台连不上。重跑 ./deploy/dev-up.sh"
  fi
else
  bad "服务端没在跑"
fi

# 发给客户端的信令地址:localhost 的话别的机器连的是它自己
LKURL=$(grep -o 'LIVEKIT_URL=.*' /tmp/muster-dev.env 2>/dev/null | cut -d= -f2-)
case "$LKURL" in
  *localhost*|*127.0.0.1*)
    bad "LIVEKIT_URL=$LKURL —— **别的机器会连到它自己**,报 could not establish signal connection。重跑 dev-up.sh" ;;
  "") warn "取不到 LIVEKIT_URL(/tmp/muster-dev.env 在吗)" ;;
  *)  ok "入会票信令地址 $LKURL(别的机器可达)" ;;
esac

CAND=$(docker logs deploy-livekit-1 2>&1 | grep -oE '"nodeIP": "[^"]*"' | tail -1 | cut -d'"' -f4)
if [ "$CAND" = "127.0.0.1" ]; then
  bad "LiveKit 广播 127.0.0.1 —— 第二台会报 pc connection。重跑 ./deploy/dev-up.sh"
elif [ -n "$CAND" ]; then
  ok "LiveKit 广播 $CAND(第二台可达)"
else
  bad "LiveKit 没在跑"
fi

# 局域网可达性:从别的机器连过来靠的就是这三个口
if [ -n "$LAN" ]; then
  for p in 8787 7880 7881; do
    if nc -z -G 2 "$LAN" "$p" 2>/dev/null; then
      ok "$LAN:$p 可达"
    else
      bad "$LAN:$p 不可达 —— 第二台连不上(查防火墙 / 服务有没有起)"
    fi
  done
  # UDP 探不出来,只能看它在不在听
  lsof -nP -i UDP:7882 2>/dev/null | grep -q "7882" \
    && ok "UDP 7882 在听(媒体面;探不到就没法开会)" \
    || bad "UDP 7882 没在听 —— 会议连不上"
fi

pgrep -f "examples/daemon" >/dev/null && ok "Agent 常驻服务在跑" \
  || bad "Agent 常驻服务没跑 —— 点按钮也不会有人来"

for u in alice bob; do
  t=$(curl -s -m 5 -X POST "$SERVER/auth/login" -H 'content-type: application/json' \
    -d "{\"id\":\"$u\",\"password\":\"${u}pass123\"}" \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('token',''))" 2>/dev/null)
  [ -n "$t" ] && ok "$u 可登录" || bad "$u 登不进去"
done

MID=${1:-}
[ -z "$MID" ] && { echo; echo "带上会议 id 再跑一次可查该会议的结果:./deploy/check.sh <会议id>"; exit 0; }

sec "会议 $MID"
eval "$PGC -c \"SELECT title||' · 密级 '||level||' · '||CASE WHEN wants_agent THEN '已请 Agent' ELSE '未请 Agent' END FROM meeting WHERE id='$MID'\"" \
  | while read -r l; do ok "$l"; done

sec "① 说话人归属(每人一轨,不做声纹分离)"
eval "$PGC -c \"SELECT speaker_id||' × '||COUNT(*) FROM meeting_transcript WHERE meeting_id='$MID' GROUP BY speaker_id ORDER BY 1\"" \
  | while read -r l; do ok "$l"; done
N=$(eval "$PGC -c \"SELECT COUNT(DISTINCT speaker_id) FROM meeting_transcript WHERE meeting_id='$MID' AND speaker_id NOT IN ('系统')\"")
[ "${N:-0}" -ge 2 ] && ok "出现了 $N 个不同说话人 —— 归属没串成一个" \
  || warn "只有 ${N:-0} 个说话人,两人都说过话了吗?"

sec "② 同时说话(预期:两条时间重叠的记录,各自独立)"
eval "$PGC -c \"
SELECT a.speaker_id||' 与 '||b.speaker_id||' 在 '||to_char(to_timestamp(a.ts_ms/1000),'HH24:MI:SS')||' 重叠'
FROM meeting_transcript a JOIN meeting_transcript b
  ON a.meeting_id=b.meeting_id AND a.id<b.id AND a.speaker_id<>b.speaker_id
 AND abs(a.ts_ms-b.ts_ms) < 3000
WHERE a.meeting_id='$MID' LIMIT 3\"" | while read -r l; do
  [ -n "$l" ] && ok "$l"
done

sec "③ 转写有没有跟不上(丢帧 = 该上 GPU 或降到 tiny)"
D=$(grep -c "已丢帧" /tmp/daemon.log 2>/dev/null || echo 0)
if [ "$D" -eq 0 ]; then ok "没有丢帧"
else
  LAST=$(grep -oE "dropped=[0-9]+" /tmp/daemon.log 2>/dev/null | tail -1)
  warn "出现 $D 条丢帧警告(最近 $LAST)。散会那 20 秒的丢帧是正常的,会中丢就要处理"
fi

sec "④ 行动项(提案 ≠ 任务)"
curl -s -m 5 "$SERVER/meetings/$MID/action-items" -H "authorization: Bearer $TOK" \
  | python3 -c "
import sys,json
try: rows=json.load(sys.stdin)
except Exception: rows=[]
if not rows: print('  ! 还没有行动项(散会停 daemon 才会提炼)')
for a in rows:
    print(f\"  ✓ [{a['status']}] {a['text']}\")
    print(f\"      出处:{a.get('source_quote') or '—'}\")
" 2>/dev/null

sec "⑤ 最近的纪要"
eval "$PGC -c \"SELECT speaker_id||' → '||substr(text,1,54) FROM meeting_transcript WHERE meeting_id='$MID' ORDER BY ts_ms DESC LIMIT 6\"" \
  | while read -r l; do echo "     $l"; done
