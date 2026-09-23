//! 工作流的自有仓库与产物目录。
//!
//! 布局:`<runs 根>/<name>/repo`(git 仓库,Agent 在它的隔离 worktree 里写 data/ 与
//! report.html)、`<runs 根>/<name>/out/<时间戳>/`(渲染后的产物:html / pdf / summary)。
//!
//! **自动合入只对自己名下的仓库**:[`Runs::merge_branch`] 先做路径归属校验,不在
//! runs 根之下的一律拒绝。这是"Runner 永不自行合入"这条铁律的边界:它保护的是人的代码,
//! 而这里没有人的代码。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use muster_audit::{ArtifactRef, ContentHash};

use crate::spec::OutputKind;
use crate::WorkflowError;

pub struct Runs {
    root: PathBuf,
}

/// 一个产物:种类、落盘路径、内容哈希(进审计的是哈希)。
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub kind: String,
    pub path: PathBuf,
    pub content_hash: ContentHash,
}

impl Artifact {
    pub fn audit_ref(&self) -> ArtifactRef {
        ArtifactRef { kind: self.kind.clone(), content_hash: self.content_hash.clone() }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Rendered {
    pub out_dir: PathBuf,
    pub artifacts: Vec<Artifact>,
    /// 没做成的部分(没有 Chrome、pptx 未实现、Agent 没写 report.html…)。
    pub problems: Vec<String>,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, WorkflowError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=workflow@muster.local", "-c", "user.name=muster-workflow"])
        .args(args)
        .output()
        .map_err(|e| WorkflowError::Git(format!("启动 git 失败:{e}")))?;
    if !out.status.success() {
        return Err(WorkflowError::Git(format!(
            "git {} 失败:{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Runs {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, WorkflowError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| WorkflowError::Io(e.to_string()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn repo_dir(&self, name: &str) -> PathBuf {
        self.root.join(name).join("repo")
    }

    pub fn out_root(&self, name: &str) -> PathBuf {
        self.root.join(name).join("out")
    }

    /// 这个路径是不是本 runs 根之下的仓库(合入的前提)。
    pub fn is_owned(&self, repo: &Path) -> bool {
        match (repo.canonicalize(), self.root.canonicalize()) {
            (Ok(r), Ok(root)) => r.starts_with(&root),
            _ => false,
        }
    }

    /// 保证工作流的仓库存在:没有就 `git init` 并做一次初始提交(空仓库建不了 worktree)。
    pub fn ensure_repo(&self, name: &str) -> Result<PathBuf, WorkflowError> {
        let repo = self.repo_dir(name);
        if repo.join(".git").exists() {
            return Ok(repo);
        }
        std::fs::create_dir_all(repo.join("data")).map_err(|e| WorkflowError::Io(e.to_string()))?;
        std::fs::write(
            repo.join("README.md"),
            format!(
                "# 工作流 {name} 的自有仓库\n\n由 Muster 自动维护:每次运行在隔离分支上写 `data/` 与 `report.html`,\n完成后自动合入到 main(这里没有人的代码,不走审批;合入记录在审计链 `workflow.output`)。\n"
            ),
        )
        .map_err(|e| WorkflowError::Io(e.to_string()))?;
        std::fs::write(repo.join("data/.gitkeep"), "").map_err(|e| WorkflowError::Io(e.to_string()))?;
        if git(&repo, &["init", "-q", "-b", "main"]).is_err() {
            git(&repo, &["init", "-q"])?;
            git(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        }
        git(&repo, &["add", "-A"])?;
        git(&repo, &["commit", "-qm", &format!("init:工作流 {name} 的自有仓库")])?;
        Ok(repo)
    }

    /// 把隔离分支合入 main,回收 worktree 与分支;返回合入后的 commit。
    /// **只合入自己名下的仓库**——这是设计上的硬边界,不是配置项。
    pub fn merge_branch(
        &self,
        repo: &Path,
        branch: &str,
        worktree_path: Option<&Path>,
    ) -> Result<String, WorkflowError> {
        if !self.is_owned(repo) {
            return Err(WorkflowError::NotOwned(repo.display().to_string()));
        }
        git(repo, &["merge", "--no-ff", "-m", &format!("工作流产出:{branch}"), branch])?;
        let head = git(repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        if let Some(p) = worktree_path {
            if git(repo, &["worktree", "remove", "--force", &p.display().to_string()]).is_ok() {
                let _ = git(repo, &["branch", "-D", branch]);
            }
        }
        Ok(head)
    }

    /// `data/` 里已有的文件名(排好序;不含 .gitkeep),给提示词用。
    pub fn data_files(repo: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(repo.join("data"))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| !n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// summary.md 的内容(截到 2000 字),没有就 None。
    pub fn summary_text(repo: &Path) -> Option<String> {
        let s = std::fs::read_to_string(repo.join("summary.md")).ok()?;
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        Some(s.chars().take(2000).collect())
    }

    /// 把合入后的报告渲染成产物:html 原样复制,pdf 用本机 Chrome 打印,pptx 未实现。
    pub fn render(&self, name: &str, repo: &Path, outputs: &[OutputKind], stamp: &str) -> Rendered {
        let out = self.out_root(name).join(stamp);
        let mut r = Rendered { out_dir: out.clone(), ..Default::default() };
        if let Err(e) = std::fs::create_dir_all(&out) {
            r.problems.push(format!("建不了产物目录 {}:{e}", out.display()));
            return r;
        }
        let report = repo.join("report.html");
        let Ok(html) = std::fs::read(&report) else {
            r.problems.push("Agent 没有写出 report.html,没有可发布的报告".into());
            return r;
        };
        let html_out = out.join("report.html");
        if let Err(e) = std::fs::write(&html_out, &html) {
            r.problems.push(format!("写 {} 失败:{e}", html_out.display()));
            return r;
        }
        r.artifacts.push(Artifact { kind: "html".into(), path: html_out.clone(), content_hash: ContentHash::sha256(&html) });

        if let Ok(summary) = std::fs::read(repo.join("summary.md")) {
            let p = out.join("summary.md");
            if std::fs::write(&p, &summary).is_ok() {
                r.artifacts.push(Artifact { kind: "summary".into(), path: p, content_hash: ContentHash::sha256(&summary) });
            }
        }

        if outputs.contains(&OutputKind::Pdf) {
            match find_chrome() {
                Some(chrome) => {
                    let pdf = out.join("report.pdf");
                    match print_pdf(&chrome, &html_out, &pdf) {
                        Ok(bytes) => r.artifacts.push(Artifact { kind: "pdf".into(), path: pdf, content_hash: ContentHash::sha256(&bytes) }),
                        Err(e) => r.problems.push(format!("PDF 生成失败:{e}")),
                    }
                }
                None => r.problems.push(
                    "PDF 需要本机安装 Chrome / Chromium / Edge(或设 MUSTER_CHROME 指向可执行文件);本次只有 HTML".into(),
                ),
            }
        }
        if outputs.contains(&OutputKind::Pptx) {
            r.problems.push("pptx 输出尚未实现(v0 只出 html / pdf)".into());
        }
        r
    }
}

/// 找本机的 Chromium 系浏览器:`MUSTER_CHROME` 优先,其次按平台常见路径 / PATH。
pub fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MUSTER_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates: Vec<PathBuf> = match std::env::consts::OS {
        "macos" => vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
        "windows" => {
            let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
            let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
            let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
            vec![
                format!("{pf}\\Google\\Chrome\\Application\\chrome.exe"),
                format!("{pf86}\\Google\\Chrome\\Application\\chrome.exe"),
                format!("{local}\\Google\\Chrome\\Application\\chrome.exe"),
                format!("{pf86}\\Microsoft\\Edge\\Application\\msedge.exe"),
                format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe"),
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect()
        }
        _ => Vec::new(),
    };
    if let Some(p) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    // PATH 里找(Linux 与其他)
    let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "chrome"];
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// headless 打印:独立的临时 profile(与正在跑的浏览器互不干扰)。
///
/// **完成信号是"PDF 文件出现且大小稳定",不是进程退出**:实测 Chrome 新版 headless
/// 在临时 profile 下写完 PDF 后可能一直不退出,等它退出只会等到超时。文件到手就主动结束它。
fn print_pdf(chrome: &Path, html: &Path, pdf: &Path) -> Result<Vec<u8>, String> {
    let _ = std::fs::remove_file(pdf);
    let profile = pdf.parent().unwrap_or(Path::new(".")).join(".chrome-profile");
    let _ = std::fs::create_dir_all(&profile);
    // 用 Url::from_file_path 而不是拼字符串:Windows 的 `C:\\…` 拼出来不是合法 URL
    let url = reqwest::Url::from_file_path(html)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", html.display()));
    let mut child = Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--hide-scrollbars",
            "--no-pdf-header-footer",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", pdf.display()))
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 {} 失败:{e}", chrome.display()))?;
    let started = Instant::now();
    let mut seen: Option<(u64, Instant)> = None;
    let outcome: Result<(), String> = loop {
        if let Ok(md) = std::fs::metadata(pdf) {
            let len = md.len();
            if len > 0 {
                match seen {
                    Some((l, t)) if l == len && t.elapsed() > Duration::from_millis(500) => break Ok(()),
                    Some((l, _)) if l == len => {}
                    _ => seen = Some((len, Instant::now())),
                }
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => {
                break if pdf.exists() { Ok(()) } else { Err(format!("浏览器退出({st})但没有产出 PDF")) };
            }
            Ok(None) if started.elapsed() > Duration::from_secs(60) => break Err("浏览器 60 秒没打完,已终止".into()),
            Ok(None) => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => break Err(format!("等待浏览器失败:{e}")),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&profile);
    outcome?;
    let bytes = std::fs::read(pdf).map_err(|e| format!("读取 PDF 失败:{e}"))?;
    if !bytes.starts_with(b"%PDF") {
        return Err("产出的文件不是 PDF".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    #[test]
    fn ensure_repo_is_idempotent_and_has_a_baseline_commit() {
        let root = tempfile::tempdir().unwrap();
        let runs = Runs::open(root.path().join("runs")).unwrap();
        let repo = runs.ensure_repo("daily").unwrap();
        assert!(repo.join(".git").exists());
        assert!(repo.join("data/.gitkeep").exists());
        let head1 = sh(&repo, &["rev-parse", "HEAD"]);
        let repo2 = runs.ensure_repo("daily").unwrap();
        assert_eq!(repo, repo2);
        assert_eq!(head1, sh(&repo, &["rev-parse", "HEAD"]), "再调一次不该多出提交");
        assert!(Runs::data_files(&repo).is_empty(), ".gitkeep 不算数据文件");
        assert!(runs.is_owned(&repo));
    }

    #[test]
    fn merge_branch_lands_agent_output_and_refuses_foreign_repos() {
        let root = tempfile::tempdir().unwrap();
        let runs = Runs::open(root.path().join("runs")).unwrap();
        let repo = runs.ensure_repo("daily").unwrap();
        // 模拟 Runner:在隔离 worktree 的分支上写产出并提交
        let wt = root.path().join("wt");
        sh(&repo, &["worktree", "add", "-q", "-b", "run/WF-1", &wt.display().to_string(), "main"]);
        std::fs::write(wt.join("report.html"), "<html>报告</html>").unwrap();
        std::fs::write(wt.join("data/2026-09-23.json"), "{}").unwrap();
        sh(&wt, &["add", "-A"]);
        sh(&wt, &["commit", "-qm", "产出"]);

        let head = runs.merge_branch(&repo, "run/WF-1", Some(&wt)).unwrap();
        assert_eq!(head.len(), 40);
        assert!(repo.join("report.html").exists(), "产出应已合入 main");
        assert_eq!(Runs::data_files(&repo), vec!["2026-09-23.json"]);
        assert!(!wt.exists(), "worktree 应已回收");
        assert!(sh(&repo, &["branch", "--list", "run/WF-1"]).trim().is_empty(), "分支应已删除");

        // 别人的仓库一律拒绝——哪怕分支真的存在
        let foreign = root.path().join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        sh(&foreign, &["init", "-q", "-b", "main"]);
        let e = runs.merge_branch(&foreign, "main", None).unwrap_err();
        assert!(matches!(e, WorkflowError::NotOwned(_)), "{e}");
    }

    #[test]
    fn render_copies_html_hashes_it_and_reports_missing_pieces() {
        let root = tempfile::tempdir().unwrap();
        let runs = Runs::open(root.path().join("runs")).unwrap();
        let repo = runs.ensure_repo("daily").unwrap();
        // 没有 report.html:只有问题,没有产物
        let r = runs.render("daily", &repo, &[OutputKind::Html], "t1");
        assert!(r.artifacts.is_empty());
        assert!(r.problems[0].contains("report.html"));

        std::fs::write(repo.join("report.html"), "<html><body>hi</body></html>").unwrap();
        std::fs::write(repo.join("summary.md"), "要点一\n要点二").unwrap();
        let r = runs.render("daily", &repo, &[OutputKind::Html, OutputKind::Pptx], "t2");
        let kinds: Vec<&str> = r.artifacts.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, vec!["html", "summary"]);
        assert_eq!(r.artifacts[0].content_hash, ContentHash::sha256(b"<html><body>hi</body></html>"));
        assert!(r.out_dir.join("report.html").exists());
        assert!(r.problems.iter().any(|p| p.contains("pptx")));
        assert_eq!(Runs::summary_text(&repo).as_deref(), Some("要点一\n要点二"));
    }

    /// 有 Chrome 才真打一份 PDF;没有就只验"如实报告缺失"。
    #[test]
    fn pdf_is_printed_when_a_browser_exists_else_reported() {
        let root = tempfile::tempdir().unwrap();
        let runs = Runs::open(root.path().join("runs")).unwrap();
        let repo = runs.ensure_repo("daily").unwrap();
        std::fs::write(repo.join("report.html"), "<html><body><h1>PDF 测试</h1><svg width='10' height='10'></svg></body></html>").unwrap();
        let r = runs.render("daily", &repo, &[OutputKind::Pdf], "t3");
        if find_chrome().is_some() {
            let pdf = r.artifacts.iter().find(|a| a.kind == "pdf").unwrap_or_else(|| panic!("{:?}", r.problems));
            let bytes = std::fs::read(&pdf.path).unwrap();
            assert!(bytes.starts_with(b"%PDF"), "应是真正的 PDF");
        } else {
            assert!(r.problems.iter().any(|p| p.contains("Chrome")));
        }
    }
}
