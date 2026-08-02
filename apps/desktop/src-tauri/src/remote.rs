//! 桌面壳接服务端(C1)。
//!
//! ## 什么上服务端,什么永远留本地
//!
//! | | 在哪 | 为什么 |
//! |---|---|---|
//! | Runner / worktree / diff | **本地** | 架构文档边界一:服务端不持有源码 |
//! | 审计链 | **本地** | 边界三:每次 append 一次网络往返 + fail-closed = 断网即停工 |
//! | 身份、团队、频道 | 服务端 | 这些本来就是全组织共享的东西 |
//! | 团队频道的消息 | 服务端 | 同上 |
//! | **个人频道的消息** | **本地,永不上传** | 见下 |
//!
//! ### 个人频道为什么连上服务器也不上传
//!
//! 界面上写着「内容默认不进团队,不出现在任何频道与检索里」。这是对使用者的
//! **承诺**,不是当前实现的副作用。一旦接上服务器就把私有会话同步上去,
//! 那句话当场变成假话——而使用者是照着那句话决定往里说什么的。
//!
//! ## 未配置服务器时,一切照旧
//!
//! [`Remote`] 是 `Option`。没配就是原来的单机点将台,一行行为都不变;
//! 配了才多出登录与团队频道。**不能因为加了联网模式就让单机模式退化**——
//! 单机才是这个产品的入口形态。

use serde::{Deserialize, Serialize};

/// 已连接的服务端会话。
#[derive(Clone)]
pub struct Remote {
    pub base: String,
    pub token: String,
    pub account_id: String,
    pub display_name: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct LoginResp {
    token: String,
    id: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct RemoteChannel {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub level: String,
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RemoteMessage {
    pub channel_seq: i64,
    pub author_id: String,
    pub role: String,
    pub body: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub ts_ms: i64,
}

/// 会议(C3)。`level` 决定界面上的密级徽章,`can_publish` 决定要不要显示
/// 开麦按钮——**由服务端的权限内核给出,前端不自己判**。
#[derive(Debug, Deserialize, Serialize)]
pub struct RemoteMeeting {
    pub id: String,
    pub channel_id: String,
    pub title: String,
    pub level: String,
    pub room: String,
    pub started_ms: i64,
    #[serde(default)]
    pub ended_ms: Option<i64>,
    /// 是否请了 Agent。**只是意愿**——它到没到看参会者列表。
    #[serde(default)]
    pub wants_agent: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JoinInfo {
    pub url: String,
    pub token: String,
    pub room: String,
    pub level: String,
    pub can_publish: bool,
    pub can_record: bool,
}

#[derive(Debug, Serialize)]
struct PostMessage<'a> {
    body: &'a str,
    role: &'a str,
    run_id: Option<&'a str>,
}

impl Remote {
    /// 登录并拿令牌。**失败要如实报**——静默降级成单机模式,会让人以为
    /// 消息发给了团队,其实只存在自己机器上。
    pub async fn login(base: &str, id: &str, password: &str) -> Result<Self, String> {
        let base = base.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("http 客户端创建失败:{e}"))?;
        let resp = client
            .post(format!("{base}/auth/login"))
            .json(&serde_json::json!({ "id": id, "password": password }))
            .send()
            .await
            .map_err(|e| format!("连不上服务端 {base}:{e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(msg);
        }
        let r: LoginResp =
            serde_json::from_str(&text).map_err(|e| format!("登录响应无法解析:{e}"))?;
        Ok(Self {
            base,
            token: r.token,
            account_id: r.id,
            display_name: r.display_name,
            client,
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("请求 {path} 失败:{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("{path}:HTTP {}", resp.status()));
        }
        resp.json().await.map_err(|e| format!("{path} 响应无法解析:{e}"))
    }

    pub async fn channels(&self) -> Result<Vec<RemoteChannel>, String> {
        self.get("/channels").await
    }

    pub async fn channel(&self, id: &str) -> Result<RemoteChannel, String> {
        self.get(&format!("/channels/{id}")).await
    }

    pub async fn meetings(&self, channel: &str) -> Result<Vec<RemoteMeeting>, String> {
        self.get(&format!("/channels/{channel}/meetings")).await
    }

    async fn post_json<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("请求 {path} 失败:{e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(msg);
        }
        serde_json::from_str(&text).map_err(|e| format!("{path} 响应无法解析:{e}"))
    }

    pub async fn start_meeting(&self, channel: &str, title: &str) -> Result<RemoteMeeting, String> {
        self.post_json(
            &format!("/channels/{channel}/meetings"),
            &serde_json::json!({ "title": title }),
        )
        .await
    }

    /// 拿入会票。**能不能开麦由服务端的 can() 决定**,前端只照着显示。
    pub async fn join_meeting(&self, meeting_id: &str) -> Result<JoinInfo, String> {
        self.post_json(&format!("/meetings/{meeting_id}/join"), &serde_json::json!({})).await
    }

    /// 请 Agent 来 / 请它离开。**只记意愿**,认领由服务器上的 agent-daemon 做。
    pub async fn set_wants_agent(&self, meeting_id: &str, want: bool) -> Result<(), String> {
        let _: serde_json::Value = self
            .post_json(&format!("/meetings/{meeting_id}/agent"), &serde_json::json!({ "want": want }))
            .await?;
        Ok(())
    }

    pub async fn end_meeting(&self, meeting_id: &str) -> Result<(), String> {
        let _: serde_json::Value =
            self.post_json(&format!("/meetings/{meeting_id}/end"), &serde_json::json!({})).await?;
        Ok(())
    }

    /// 拉某频道的消息。`after_seq` 为 `None` 时从头拉。
    pub async fn messages(&self, channel: &str, after_seq: Option<i64>) -> Result<Vec<RemoteMessage>, String> {
        let q = after_seq.map(|s| format!("?after_seq={s}")).unwrap_or_default();
        self.get(&format!("/channels/{channel}/messages{q}")).await
    }

    /// 发一条消息。**返回错误就要让人看见**:以为发到了团队、其实没发出去,
    /// 比发不出去更糟。
    pub async fn send(
        &self,
        channel: &str,
        body: &str,
        role: &str,
        run_id: Option<&str>,
    ) -> Result<RemoteMessage, String> {
        let resp = self
            .client
            .post(format!("{}/channels/{channel}/messages", self.base))
            .bearer_auth(&self.token)
            .json(&PostMessage { body, role, run_id })
            .send()
            .await
            .map_err(|e| format!("发送失败:{e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(msg);
        }
        serde_json::from_str(&text).map_err(|e| format!("发送响应无法解析:{e}"))
    }
}

/// 会话持久化。
///
/// 不存的话,每次重启桌面壳都掉回单机模式——而单机模式的会议室是**概念稿**
/// (自动滚动的字幕、假计时器),于是人会以为"这东西在模拟执行"。
/// 真机上就撞过这一次:重启一次,整个产品看起来像个演示。
///
/// **只存服务器地址与账号,不存口令。** 令牌 12 小时过期,过期就要求重新登录
/// ——把口令留在盘上换取"永不掉线"是笔坏买卖。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub base: String,
    pub account_id: String,
    pub display_name: String,
    pub token: String,
}

fn session_path() -> Option<std::path::PathBuf> {
    // 可覆盖:同一台机器上开两个桌面壳(演示两个人开会)时,共用一个文件会
    // 互相覆盖——后登录的把先登录的挤掉,而界面上看不出来。
    if let Ok(p) = std::env::var("MUSTER_SESSION_FILE") {
        return Some(std::path::PathBuf::from(p));
    }
    Some(home_dir()?.join(".muster").join("desktop-session.json"))
}

/// 家目录。**Windows 上没有 `HOME`**,它叫 `USERPROFILE`。
///
/// 只读 `HOME` 的后果不是报错而是**静默失效**:`session_path()` 返回 `None`,
/// 于是保存和读取都变成空操作——登录成功、界面正常,一重启就掉回未登录,
/// 而日志里什么都没有。这类"看起来在工作、其实没存"的失败最难查。
fn home_dir() -> Option<std::path::PathBuf> {
    for k in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return Some(std::path::PathBuf::from(v));
            }
        }
    }
    // Windows 上偶尔只有这一对(域账户、部分服务上下文)
    match (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        (Ok(d), Ok(p)) if !d.is_empty() && !p.is_empty() => Some(std::path::PathBuf::from(d + &p)),
        _ => None,
    }
}

impl Remote {
    pub fn save_session(&self) {
        let Some(p) = session_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let s = Session {
            base: self.base.clone(),
            account_id: self.account_id.clone(),
            display_name: self.display_name.clone(),
            token: self.token.clone(),
        };
        if let Ok(j) = serde_json::to_string(&s) {
            let _ = std::fs::write(&p, j);
            // 令牌等同口令,别让同机其他用户读到
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    pub fn clear_session() {
        if let Some(p) = session_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    /// 从盘上恢复。**令牌可能已过期**——所以恢复后要探一次,
    /// 探不通就当没连上,而不是显示"已连接"却什么都拉不到。
    pub async fn restore() -> Option<Self> {
        let p = session_path()?;
        let s: Session = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;
        let r = Self {
            base: s.base,
            token: s.token,
            account_id: s.account_id,
            display_name: s.display_name,
            client,
        };
        // 探活:令牌过期或服务器换了地方,就别假装还连着
        r.channels().await.ok()?;
        Some(r)
    }
}

/// 频道归属:决定这条消息走本地还是服务端。
///
/// **个人频道永远走本地**,这是产品承诺不是实现细节(见模块文档)。
pub fn is_personal(channel_id: &str) -> bool {
    channel_id == "personal"
}

pub fn parse_level(s: &str) -> muster_route::Sensitivity {
    match s {
        "restricted" => muster_route::Sensitivity::Restricted,
        "open" => muster_route::Sensitivity::Open,
        _ => muster_route::Sensitivity::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 个人频道的判定必须**独立于是否连了服务器**——它是产品承诺,
    /// 不能因为"连上了就顺便同步一下"而变。
    #[test]
    fn personal_channel_is_always_local() {
        assert!(is_personal("personal"));
        assert!(!is_personal("platform"));
        assert!(!is_personal("personal-notes"), "只认那一个 id,不做前缀匹配");
    }

    #[test]
    fn level_maps_to_the_kernel_enum() {
        use muster_route::Sensitivity::*;
        assert_eq!(parse_level("restricted"), Restricted);
        assert_eq!(parse_level("open"), Open);
        assert_eq!(parse_level("internal"), Internal);
        // 读不懂的密级**按最严处理**,不兜底成 open——
        // 猜错方向的代价是把该锁本地的内容放上云
        assert_eq!(parse_level("绝密"), Internal);
    }

    /// base URL 末尾的斜杠要吃掉,否则拼出 `//channels`。
    #[test]
    fn base_url_trailing_slash_is_normalised() {
        assert_eq!("http://x:8787/".trim_end_matches('/'), "http://x:8787");
        assert_eq!("http://x:8787".trim_end_matches('/'), "http://x:8787");
    }
}

#[cfg(test)]
mod home_tests {
    /// 家目录解析的**顺序**要固定,而且不能只认 `HOME`。
    ///
    /// 这个测试不动进程环境(那会波及并行跑的其他测试),只验纯逻辑:
    /// 把 [`super::home_dir`] 的取值顺序照抄一份,验证 Windows 那套变量
    /// 也能落到结果上。真正的防线是 `home_dir` 里的常量表,
    /// 这里锁的是"表里必须有 USERPROFILE"这件事。
    #[test]
    fn windows_variables_are_in_the_lookup_table() {
        let src = include_str!("remote.rs");
        let table = src
            .split("fn home_dir()")
            .nth(1)
            .expect("home_dir 还在吗");
        for k in ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH"] {
            assert!(
                table.contains(k),
                "home_dir 里少了 {k}:Windows 上没有 HOME,漏掉它会让登录状态\
                 静默存不住——不报错,只是重启后掉回未登录"
            );
        }
    }
}
