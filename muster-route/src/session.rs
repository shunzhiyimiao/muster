//! E3:会话污染棘轮(SessionRatchet)。
//!
//! 一句话:**会话一旦触碰更高密级的资源,底线只升不降,跨轮次持久。**
//!
//! ## 语义
//!
//! - 棘轮维护会话的密级**底线**(`floor`),初始为 `DEFAULT_SENSITIVITY`(Open)。
//! - 每轮用 [`SessionRatchet::observe`] 吸收该轮触碰的资源来源;观察到高于当前
//!   底线的来源即**抬升**并记录肇因(哪个资源、第几轮)。Open 观察永不产生
//!   锁——Open 就是默认底,锁在 Open 没有信息量。
//! - 抬升后,[`SessionRatchet::lock_source`] 产出一条
//!   `LabelOrigin::SessionLock` 来源,注入后续每轮 `decide()` 的 sources——
//!   这正是 E1 预留的席位:棘轮不改决策器一行代码,复用同一条
//!   `effective_sensitivity` → fail-closed 路径。
//! - **没有任何降低底线的 API**。接口即政策,与审计层"没有 UPDATE"同一哲学。
//!   会话要"解锁"只有一种方式:开新会话(v1.1 可加带审批的降密流程)。
//!
//! ## 每轮接线契约(顺序有讲究)
//!
//! [`SessionRatchet::turn_sources`] 固定为:**先**用既有锁参与本轮决策来源,
//! **再**把本轮触碰吸收进棘轮。这样触碰 restricted 资源的那一轮,deciders
//! 里写的是资源本身(`repo:payments-core`);之后的轮次才由
//! `session-lock:…` 解释——"为什么是这个级别"始终指向信息量最大的肇因。
//! 两种顺序算出的有效密级相同(max 对合并满足结合律),差别只在解释质量。

use serde::{Deserialize, Serialize};

use crate::label::{LabelOrigin, LabelSource, Sensitivity, DEFAULT_SENSITIVITY};

/// 一次抬升(审计事件 `session.lock.raise` 的数据来源)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Raise {
    pub from: Sensitivity,
    pub to: Sensitivity,
    /// 触发抬升的来源;并列时取本轮首个达到新级别者(顺序即触碰顺序)。
    pub cause: LabelSource,
    pub turn: u64,
}

/// 锁定状态(底线已高于 Open 时存在)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockState {
    pub level: Sensitivity,
    /// 首次把会话抬到**当前**级别的来源(解释"为什么锁在这")。
    pub cause: LabelSource,
    pub raised_at_turn: u64,
}

/// 会话污染棘轮。`Serialize`/`Deserialize`:跨进程重启持久(C1 存进会话行)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRatchet {
    lock: Option<LockState>,
    /// 观察轮次计数,单调,随持久化保留。
    turn: u64,
}

impl SessionRatchet {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前底线:锁定级,未锁定则为 `DEFAULT_SENSITIVITY`。
    pub fn floor(&self) -> Sensitivity {
        self.lock.as_ref().map(|l| l.level).unwrap_or(DEFAULT_SENSITIVITY)
    }

    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    pub fn lock(&self) -> Option<&LockState> {
        self.lock.as_ref()
    }

    pub fn turns_observed(&self) -> u64 {
        self.turn
    }

    /// 吸收一轮触碰。仅当本轮最高来源**严格高于**当前底线时抬升并返回
    /// [`Raise`];否则返回 `None`(重复触碰同级资源是幂等的)。
    pub fn observe(&mut self, touched: &[LabelSource]) -> Option<Raise> {
        self.turn += 1;
        let top = touched.iter().max_by_key(|s| s.level)?;
        if top.level <= self.floor() {
            return None;
        }
        // 并列取首个:iter().max_by_key 返回**最后**一个并列最大值,
        // 这里要的是首个,显式找一遍。
        let cause = touched
            .iter()
            .find(|s| s.level == top.level)
            .expect("top 来自同一切片,必然存在")
            .clone();
        let raise = Raise { from: self.floor(), to: top.level, cause: cause.clone(), turn: self.turn };
        self.lock = Some(LockState { level: top.level, cause, raised_at_turn: self.turn });
        Some(raise)
    }

    /// 产出注入 `decide()` 的 SessionLock 来源。subject 携带原始肇因,
    /// 徽章文案可直接说"会话曾引用 {subject}"。
    pub fn lock_source(&self) -> Option<LabelSource> {
        self.lock.as_ref().map(|l| {
            LabelSource::new(
                LabelOrigin::SessionLock,
                l.level,
                format!("session-lock:{}", l.cause.subject),
            )
        })
    }

    /// 一轮的标准接线(见模块文档"顺序有讲究"):
    /// 返回 (本轮 `decide()` 应使用的来源列表, 本轮是否发生抬升)。
    pub fn turn_sources(&mut self, touched: &[LabelSource]) -> (Vec<LabelSource>, Option<Raise>) {
        let mut sources = touched.to_vec();
        if let Some(lock) = self.lock_source() {
            sources.push(lock);
        }
        let raise = self.observe(touched);
        (sources, raise)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(origin: LabelOrigin, level: Sensitivity, subject: &str) -> LabelSource {
        LabelSource::new(origin, level, subject)
    }

    #[test]
    fn open_touches_never_create_a_lock() {
        let mut r = SessionRatchet::new();
        assert!(r.observe(&[src(LabelOrigin::Channel, Sensitivity::Open, "channel:#general")]).is_none());
        assert!(!r.is_locked());
        assert_eq!(r.floor(), Sensitivity::Open);
        assert!(r.lock_source().is_none());
    }

    #[test]
    fn ratchet_goes_up_and_never_down() {
        let mut r = SessionRatchet::new();
        let raise = r
            .observe(&[src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:payments-core")])
            .expect("must raise");
        assert_eq!(raise.from, Sensitivity::Open);
        assert_eq!(raise.to, Sensitivity::Restricted);
        assert_eq!(raise.cause.subject, "repo:payments-core");

        // 之后触碰低密级:底线纹丝不动,且不产生新抬升。
        assert!(r.observe(&[src(LabelOrigin::Channel, Sensitivity::Open, "channel:#general")]).is_none());
        assert_eq!(r.floor(), Sensitivity::Restricted);
    }

    #[test]
    fn same_level_touch_is_idempotent_but_internal_to_restricted_raises_again() {
        let mut r = SessionRatchet::new();
        assert!(r.observe(&[src(LabelOrigin::Channel, Sensitivity::Internal, "channel:#platform")]).is_some());
        assert!(r.observe(&[src(LabelOrigin::Manual, Sensitivity::Internal, "manual:alice")]).is_none());
        // cause 保持首次抬到该级别的来源。
        assert_eq!(r.lock().unwrap().cause.subject, "channel:#platform");

        let raise2 = r
            .observe(&[src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:payroll")])
            .expect("second raise");
        assert_eq!(raise2.from, Sensitivity::Internal);
        assert_eq!(r.lock().unwrap().cause.subject, "repo:payroll");
    }

    #[test]
    fn tie_takes_first_in_touch_order() {
        let mut r = SessionRatchet::new();
        let raise = r
            .observe(&[
                src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:a"),
                src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:b"),
            ])
            .unwrap();
        assert_eq!(raise.cause.subject, "repo:a");
    }

    #[test]
    fn lock_source_carries_provenance_for_badge_text() {
        let mut r = SessionRatchet::new();
        r.observe(&[src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:payments-core")]);
        let ls = r.lock_source().unwrap();
        assert_eq!(ls.origin, LabelOrigin::SessionLock);
        assert_eq!(ls.level, Sensitivity::Restricted);
        assert_eq!(ls.subject, "session-lock:repo:payments-core");
    }

    #[test]
    fn serde_round_trip_preserves_lock_and_turn() {
        let mut r = SessionRatchet::new();
        r.observe(&[src(LabelOrigin::Channel, Sensitivity::Internal, "channel:#platform")]);
        let json = serde_json::to_string(&r).unwrap();
        let mut back: SessionRatchet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        // 反序列化后继续棘轮:轮次接续,不从零开始。
        let raise = back
            .observe(&[src(LabelOrigin::Repo, Sensitivity::Restricted, "repo:x")])
            .unwrap();
        assert_eq!(raise.turn, 2);
    }
}
