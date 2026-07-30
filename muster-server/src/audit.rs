//! 服务端自己的审计链。
//!
//! ## 服务端就是一个节点
//!
//! 架构文档的边界三说「审计链留在节点,服务端只锚定」。但权限变更、审批裁决
//! 这些事**发生在服务端**,不属于任何一台开发机——所以服务端本身也是一个节点,
//! 有自己的链。组织链就是服务端这条链,节点锚点将来也记在这里。
//!
//! 链仍用 `muster_audit`(SQLite),不搬进 PostgreSQL:
//! 一份实现、一套已验证过的哈希链代码,比"再写一遍 PG 版"可靠得多;
//! 而它的写入量(权限变更、审批、锚点)本就极低。
//!
//! ## 单写者
//!
//! `AuditStore::append` 的形状是「读上一条 hash → 插入新行」。单个实例内
//! `&mut self` 已由借用检查串行化;真正的风险是**多个实例指向同一份存储**——
//! 两个实例读到同一个 `prev_hash` 就会分叉出两条链。
//!
//! 所以服务端全程只有一个实例,且写入经由一个独占的 tokio 任务(mpsc 投递)。
//! 链的完整性由此在并发下自动成立,不靠调用方自觉。

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use muster_audit::{AuditStore, NewEvent};

use crate::ServerError;

type WriteReq = (NewEvent, oneshot::Sender<Result<String, String>>);

/// 审计写入句柄。**克隆随便克隆——它们共用同一个写者。**
#[derive(Clone)]
pub struct Audit {
    tx: mpsc::Sender<WriteReq>,
    /// 只读连接的路径(查询侧不必经过写者)
    path: Arc<String>,
}

impl Audit {
    /// 打开链并启动独占写者。启动时验链——链坏了就别继续往上追加,
    /// 与桌面壳同一姿态(fail-closed)。
    pub fn open(path: &str) -> Result<Self, String> {
        let store = AuditStore::open(path).map_err(|e| format!("审计库打开失败:{e}"))?;
        match store.verify_chain() {
            Ok(Ok(n)) => tracing::info!("服务端审计链完整,{n} 条"),
            Ok(Err(e)) => {
                return Err(format!(
                    "服务端审计链在第 {} 条断裂(事件 {})——拒绝启动,不在坏账本上继续记账",
                    e.index + 1,
                    e.event_id
                ))
            }
            Err(e) => return Err(format!("审计链校验无法执行:{e}")),
        }

        let (tx, mut rx) = mpsc::channel::<WriteReq>(256);
        let mut store = store;
        tokio::spawn(async move {
            while let Some((ev, reply)) = rx.recv().await {
                let r = store.append(ev).map(|e| e.event_id).map_err(|e| e.to_string());
                let _ = reply.send(r);
            }
        });
        Ok(Self { tx, path: Arc::new(path.to_string()) })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 追加一条。**写失败即操作失败**——调用方必须把错误往上抛,
    /// 不许"记不上就算了"(fail-closed,与 runner 同一哲学)。
    pub async fn append(&self, ev: NewEvent) -> Result<String, ServerError> {
        let (reply, wait) = oneshot::channel();
        self.tx
            .send((ev, reply))
            .await
            .map_err(|_| ServerError::Internal("审计写者已停止".into()))?;
        wait.await
            .map_err(|_| ServerError::Internal("审计写者未应答".into()))?
            .map_err(ServerError::Internal)
    }
}
