//! 组织策略层(两层决策的第一层:组织定边界,用户在边界内选)。
//!
//! 硬编码不变量:**restricted 永不上云**——它不是策略的一个取值,而是产品
//! 契约。因此 `cloud_max` 的合法域只有 `Open | Internal`,构造时校验。
//!
//! `egress_locked` 是 E6 主权演习的接线柱:演习开始 = 置 true,所有新任务
//! 复用同一条 fail-closed 路径强制本地;演习结束 = 置 false。

use serde::{Deserialize, Serialize};

use crate::label::Sensitivity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgPolicy {
    /// 允许上云的最高密级(含)。合法值:Open / Internal。
    cloud_max: Sensitivity,
    /// 全局断外联(E6 演习;未来也可作为常态化「纯内网模式」运行开关)。
    egress_locked: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("cloud_max 不可设为 restricted:restricted 永不上云是硬编码不变量,无需也不允许配置")]
    RestrictedCloudMax,
}

impl OrgPolicy {
    pub fn new(cloud_max: Sensitivity) -> Result<Self, PolicyError> {
        if cloud_max == Sensitivity::Restricted {
            return Err(PolicyError::RestrictedCloudMax);
        }
        Ok(Self { cloud_max, egress_locked: false })
    }

    pub fn cloud_max(&self) -> Sensitivity {
        self.cloud_max
    }

    pub fn egress_locked(&self) -> bool {
        self.egress_locked
    }

    pub fn set_egress_locked(&mut self, locked: bool) {
        self.egress_locked = locked;
    }
}

impl Default for OrgPolicy {
    /// 默认:internal 及以下可上云,未锁外联。
    fn default() -> Self {
        Self { cloud_max: Sensitivity::Internal, egress_locked: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_cloud_max_is_rejected_at_construction() {
        assert_eq!(OrgPolicy::new(Sensitivity::Restricted), Err(PolicyError::RestrictedCloudMax));
        assert!(OrgPolicy::new(Sensitivity::Open).is_ok());
        assert!(OrgPolicy::new(Sensitivity::Internal).is_ok());
    }
}
