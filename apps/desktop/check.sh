#!/usr/bin/env bash
# 验一遍 E3 棘轮与「团队 → 个人」fork 到底有没有生效。
#
#   ./apps/desktop/check.sh
#
# ## 为什么要有它
#
# 这两件事的失败方式都是**静默的**:棘轮没抬升,界面一切正常,只是 restricted
# 的内容按 open 路由;fork 只搬了一半,线程看着在那儿,历史却是残的。
# 测试覆盖了纯逻辑,但"真跑一遍之后库里是什么样"只有这里能看。
#
# 它**只读**,不改任何东西。
set -uo pipefail

ok()   { printf '\033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '\033[31m✗\033[0m %s\n' "$*"; FAIL=1; }
warn() { printf '\033[33m!\033[0m %s\n' "$*"; }
step() { printf '\n\033[1m▶ %s\033[0m\n' "$*"; }
FAIL=0

HOME_DIR=${HOME:-$USERPROFILE}
STATE="$HOME_DIR/.muster/desktop-state.db"
AUDIT="$HOME_DIR/.muster/desktop-audit.db"
command -v sqlite3 >/dev/null || { echo "需要 sqlite3"; exit 2; }
[ -f "$STATE" ] || { echo "找不到 $STATE ——桌面壳还没跑过?"; exit 2; }

q() { sqlite3 "$1" "$2" 2>/dev/null; }

# ---------------------------------------------------------------- 棘轮

step "E3 会话棘轮"

if [ "$(q "$STATE" "SELECT COUNT(*) FROM sqlite_master WHERE name='session_ratchet'")" != "1" ]; then
  bad "session_ratchet 表不存在——棘轮根本没接上,或者跑的是旧版本"
else
  n=$(q "$STATE" "SELECT COUNT(*) FROM session_ratchet")
  if [ "$n" = "0" ]; then
    warn "还没有任何棘轮记录。发一句话或跑一个任务再来看。"
  else
    sqlite3 "$STATE" -header -column "
      SELECT session_id AS 会话,
             COALESCE(json_extract(state,'\$.lock.level'),'open') AS 底线,
             COALESCE(json_extract(state,'\$.lock.cause.subject'),'—') AS 肇因,
             json_extract(state,'\$.turn') AS 轮次
      FROM session_ratchet ORDER BY session_id;" | sed 's/^/  /'
    ok "共 $n 条会话有棘轮状态"
  fi
fi

# 底线只升不降的证据在链上,不在这张表里——表只有当前值
step "抬升是否进了审计链"

if [ ! -f "$AUDIT" ]; then
  warn "找不到 $AUDIT"
else
  raises=$(q "$AUDIT" "SELECT COUNT(*) FROM audit_event WHERE event_type='session.lock.raise'")
  if [ "${raises:-0}" = "0" ]; then
    warn "链上没有 session.lock.raise。只碰过 open 频道的话这是对的
    (open 是默认底,锁在 open 没有信息量)。碰过 internal/restricted 却没有,
    那就是棘轮没接上。"
  else
    sqlite3 "$AUDIT" -header -column "
      SELECT datetime(ts_ms/1000,'unixepoch','localtime') AS 时刻,
             session_id AS 会话,
             json_extract(payload,'\$.from_level') AS 从,
             json_extract(payload,'\$.to_level') AS 到,
             json_extract(payload,'\$.cause.subject') AS 肇因
      FROM audit_event WHERE event_type='session.lock.raise'
      ORDER BY ts_ms DESC LIMIT 10;" | sed 's/^/  /'
    ok "链上有 $raises 次抬升"
  fi
fi

# ---------------------------------------------------------------- fork

step "会话分叉"

if [ "$(q "$STATE" "SELECT COUNT(*) FROM sqlite_master WHERE name='thread'")" != "1" ]; then
  bad "thread 表不存在——分叉没接上,或者跑的是旧版本"
else
  n=$(q "$STATE" "SELECT COUNT(*) FROM thread")
  if [ "$n" = "0" ]; then
    warn "还没分叉过。在对话里 hover 某条提问试试。"
  else
    sqlite3 "$STATE" -header -column "
      SELECT t.id AS 线程, t.channel_id AS 落在, t.persistence AS 存法,
             t.inherited_count AS 应继承,
             (SELECT COUNT(*) FROM messages m WHERE m.thread_id=t.id) AS 自有条数
      FROM thread t ORDER BY t.created_ms DESC LIMIT 10;" | sed 's/^/  /'

    # copied 模式下"应继承"必须等于自有条数——不等就是只搬了一半。
    # 这是这个脚本唯一能抓到的静默失败:线程看着在那儿,历史却是残的。
    broken=$(q "$STATE" "
      SELECT COUNT(*) FROM thread t
      WHERE t.persistence='copied'
        AND t.inherited_count <> (SELECT COUNT(*) FROM messages m WHERE m.thread_id=t.id)")
    if [ "${broken:-0}" != "0" ]; then
      bad "有 $broken 条 copied 线程的实存条数与应继承数对不上——搬了一半"
    else
      ok "$n 条线程,copied 的条数都对得上"
    fi

    # 拉到个人空间的,个人会话的底线必须 ≥ 来源频道的密级
    cross=$(q "$STATE" "SELECT COUNT(*) FROM thread WHERE channel_id='personal' AND forked_from NOT LIKE 'main:personal%' AND forked_from IS NOT NULL")
    if [ "${cross:-0}" != "0" ]; then
      floor=$(q "$STATE" "SELECT COALESCE(json_extract(state,'\$.lock.level'),'open') FROM session_ratchet WHERE session_id='session:personal'")
      echo "  从团队拉进个人空间的线程:$cross 条;个人会话当前底线:${floor:-open}"
      if [ "${floor:-open}" = "open" ]; then
        warn "底线仍是 open。**来源频道也是 open 的话这是对的**;
    来源是 internal/restricted 却还停在 open,那就是抬升没生效——
    而那意味着 restricted 的内容正在按 open 路由。"
      else
        ok "个人会话已被抬升到 ${floor}"
      fi
    fi
  fi
fi

step "结果"
[ "$FAIL" = 0 ] && ok "没有发现对不上的地方" || bad "上面有对不上的地方"
echo "  注:本脚本只看落库结果。**「抬升之后路由真的变了」它证明不了**——"
echo "  那要在个人空间里提一次问,看它落到本地通道还是被 fail-closed 拒掉。"
exit "$FAIL"
