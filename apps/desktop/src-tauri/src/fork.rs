//! 会话分叉。**照抄 codex 的语义**(`codex-rs/core/src/thread_manager.rs`、
//! `thread_rollout_truncation.rs`),对应关系:
//!
//! | codex | 这里 |
//! |---|---|
//! | thread | 会话线程 |
//! | rollout(JSONL) | `messages` 表里属于该线程的行 |
//! | `ForkSnapshot::TruncateBeforeNthUserMessage(n)` | 同名 |
//! | `ForkPersistence::Copied` / `Referenced` | 同名 |
//! | Esc Esc 回溯改写早先的提问 | 点某条早先的提问 → 从它之前分叉 |
//!
//! ## 为什么分叉点只能落在用户消息边界上
//!
//! 这是 codex 那边最要紧的一条,原因在 Muster 这边同样成立:切在助手回合中间,
//! 会留下**有调用没结果的工具调用**——下一轮请求本身就不合法。
//!
//! 所以边界不是"任意一行",而是 `role == "user"` 的那些行。
//!
//! ## 审计链不分叉
//!
//! 一开始担心过:两条链共享一段前缀怎么办。查下来这个问题不存在——
//! **Muster 的审计链是每节点一条,不是每对话一条**。分叉只是链上多一条事件
//! (父、子、切在第几条),链本身仍是一条线。
//!
//! 这也是 codex 那边没有的问题:它的 rollout 只是文件,没有哈希链。

/// 参与分叉计算所需的最小信息。取这么窄是为了**能测**——
/// 真实的 `StoredMsg` 带着五个与分叉无关的字段。
pub trait ForkItem {
    fn is_user(&self) -> bool;
}

/// 分叉边界的位置。照抄 codex 的 `fork_turn_positions_in_rollout`。
///
/// codex 那边边界有三种(真实用户消息、带 `trigger_turn` 的 agent 间通信、
/// 旧格式信封),Muster 只有 user / agent 两种角色,所以边界就是用户消息。
pub fn fork_boundaries<T: ForkItem>(items: &[T]) -> Vec<usize> {
    items.iter().enumerate().filter(|(_, m)| m.is_user()).map(|(i, _)| i).collect()
}

/// 切在第 `n` 条用户消息**之前**,返回保留的条数。
///
/// 越界时的行为**照抄 codex**(见 `ForkSnapshot::TruncateBeforeNthUserMessage`
/// 的文档注释):
///
/// - `n` 在范围内 → 切在那条边界之前;
/// - `n` 越界且源线程**正卡在一个回合中间** → 切在最后一条边界之前,
///   把没跑完的那一截扔掉;
/// - `n` 越界且已在回合边界上 → 原样返回全部历史。
///
/// 第二条是这三条里唯一不显然的。它存在的理由:越界通常意味着"从最后重来",
/// 而如果最后那一问还没答完,把它连同问题一起带进新线程,新线程一开始就
/// 处在一个残缺的回合里。
pub fn truncate_before_nth_user_message<T: ForkItem>(items: &[T], n: usize) -> usize {
    let boundaries = fork_boundaries(items);
    if let Some(&cut) = boundaries.get(n) {
        return cut;
    }
    // 越界。卡在回合中间的判据:最后一条是用户消息(还没有回复)
    let mid_turn = items.last().is_some_and(ForkItem::is_user);
    match (mid_turn, boundaries.last()) {
        (true, Some(&last)) => last,
        _ => items.len(),
    }
}

/// 新线程的历史从哪来。照抄 codex 的 `ForkPersistence`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Persistence {
    /// 把前缀**抄一份**进新线程。独立、能单独验,代价是占空间。
    Copied,
    /// 只记"从谁那儿继承了多少条",读的时候拼上去。
    ///
    /// 省空间,但**父线程被清掉之后这条就残了**——而 Muster 的
    /// state.db 是有保留期和清理的(见 `StateStore::purge`),
    /// 这个代价比在 codex 那边实在。
    Referenced,
}

impl Persistence {
    pub fn as_str(self) -> &'static str {
        match self {
            Persistence::Copied => "copied",
            Persistence::Referenced => "referenced",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "referenced" => Persistence::Referenced,
            _ => Persistence::Copied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct M(bool);
    impl ForkItem for M {
        fn is_user(&self) -> bool {
            self.0
        }
    }
    /// `u` = 用户消息,`a` = 助手消息
    fn seq(pattern: &str) -> Vec<M> {
        pattern.chars().map(|c| M(c == 'u')).collect()
    }

    #[test]
    fn boundaries_are_user_messages_only() {
        // 切在助手回合中间会留下有调用没结果的工具调用,下一轮请求不合法
        assert_eq!(fork_boundaries(&seq("uaauaa")), vec![0, 3]);
        assert_eq!(fork_boundaries(&seq("aaa")), Vec::<usize>::new());
        assert_eq!(fork_boundaries(&seq("")), Vec::<usize>::new());
    }

    #[test]
    fn cuts_before_the_nth_user_message() {
        let s = seq("uaauaauaa"); // 边界在 0 / 3 / 6
        assert_eq!(truncate_before_nth_user_message(&s, 0), 0, "第 0 条之前 = 空");
        assert_eq!(truncate_before_nth_user_message(&s, 1), 3);
        assert_eq!(truncate_before_nth_user_message(&s, 2), 6);
    }

    /// 越界 + 已在回合边界 ⇒ 原样返回全部。
    #[test]
    fn out_of_range_at_a_boundary_keeps_everything() {
        let s = seq("uaauaa"); // 最后是助手消息 ⇒ 回合完整
        assert_eq!(truncate_before_nth_user_message(&s, 99), s.len());
    }

    /// 越界 + 卡在回合中间 ⇒ 丢掉没跑完的那一截。
    ///
    /// 这条是三条规则里唯一不显然的,也是照抄 codex 的重点:越界通常意味着
    /// "从最后重来",而如果最后那一问还没答完,把它连同问题一起带进新线程,
    /// 新线程一开始就处在一个残缺的回合里。
    #[test]
    fn out_of_range_mid_turn_drops_the_unfinished_tail() {
        let s = seq("uaau"); // 最后一条是用户消息,还没回复
        assert_eq!(truncate_before_nth_user_message(&s, 99), 3, "应当切在最后那条提问之前");
    }

    /// 一条用户消息都没有时,越界不该 panic 也不该乱切。
    #[test]
    fn no_user_messages_is_not_a_crash() {
        let s = seq("aaa");
        assert_eq!(truncate_before_nth_user_message(&s, 0), 3);
        assert_eq!(truncate_before_nth_user_message(&s, 99), 3);
        assert_eq!(truncate_before_nth_user_message::<M>(&[], 0), 0);
    }

    #[test]
    fn persistence_round_trips() {
        for p in [Persistence::Copied, Persistence::Referenced] {
            assert_eq!(Persistence::parse(p.as_str()), p);
        }
        // 认不出的值按 Copied 兜底:抄一份总是能读,引用会指向不存在的父线程
        assert_eq!(Persistence::parse("garbage"), Persistence::Copied);
    }
}
