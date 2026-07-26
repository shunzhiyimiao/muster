//! 敏感度标签模型(E1 的类型层;持久化 CRUD 属于 C1 服务器侧)。
//!
//! 核心语义:**标签跟数据走**。一个任务的有效密级不是谁"设置"的,而是它
//! 触碰到的所有资源密级的**最大值**——频道、仓库、手动标注,以及未来 E3
//! 会话棘轮注入的 `SessionLock` 来源,全部进同一个 max。

use serde::{Deserialize, Serialize};

/// 三级密级。派生 `Ord`:声明顺序即严格递增,`max()` 直接可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Open,
    Internal,
    Restricted,
}

/// 未贴任何标签时的默认密级。
///
/// 定为 `Open` 是一个**产品决策**:演示第 2 幕里未标注仓库要能走云端。
/// 保守部署可在组织策略里把云端上限压到 `Open` 之下(即禁云)达到同等效果。
pub const DEFAULT_SENSITIVITY: Sensitivity = Sensitivity::Open;

/// 标签来源(供 UI 解释「为什么被路由到这里」及审计留痕)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelOrigin {
    Channel,
    Repo,
    Manual,
    /// E3 污染棘轮:会话一旦引用 restricted 资源即注入此来源(本 crate 只
    /// 定义席位,棘轮状态机在 E3 实现)。
    SessionLock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelSource {
    pub origin: LabelOrigin,
    pub level: Sensitivity,
    /// 资源标识,如 `repo:payments-core`、`channel:#code-review`。
    pub subject: String,
}

impl LabelSource {
    pub fn new(origin: LabelOrigin, level: Sensitivity, subject: impl Into<String>) -> Self {
        Self { origin, level, subject: subject.into() }
    }
}

/// 计算有效密级,并返回**促成该密级的来源清单**(= max 的并列贡献者),
/// 供徽章悬浮文案与审计解释使用。
pub fn effective_sensitivity(sources: &[LabelSource]) -> (Sensitivity, Vec<LabelSource>) {
    let level = sources.iter().map(|s| s.level).max().unwrap_or(DEFAULT_SENSITIVITY);
    let deciders = sources.iter().filter(|s| s.level == level).cloned().collect();
    (level, deciders)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_strictly_increasing() {
        assert!(Sensitivity::Open < Sensitivity::Internal);
        assert!(Sensitivity::Internal < Sensitivity::Restricted);
    }

    #[test]
    fn effective_is_max_with_provenance() {
        let sources = vec![
            LabelSource::new(LabelOrigin::Channel, Sensitivity::Open, "channel:#general"),
            LabelSource::new(LabelOrigin::Repo, Sensitivity::Restricted, "repo:payments-core"),
            LabelSource::new(LabelOrigin::Manual, Sensitivity::Internal, "task:42"),
        ];
        let (level, deciders) = effective_sensitivity(&sources);
        assert_eq!(level, Sensitivity::Restricted);
        assert_eq!(deciders.len(), 1);
        assert_eq!(deciders[0].subject, "repo:payments-core");
    }

    #[test]
    fn empty_sources_fall_back_to_default() {
        let (level, deciders) = effective_sensitivity(&[]);
        assert_eq!(level, DEFAULT_SENSITIVITY);
        assert!(deciders.is_empty());
    }

    #[test]
    fn ties_report_all_contributors() {
        let sources = vec![
            LabelSource::new(LabelOrigin::Repo, Sensitivity::Internal, "repo:a"),
            LabelSource::new(LabelOrigin::SessionLock, Sensitivity::Internal, "session:s1"),
        ];
        let (_, deciders) = effective_sensitivity(&sources);
        assert_eq!(deciders.len(), 2);
    }
}
