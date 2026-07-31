//! 判断一句话是不是在叫 Agent。
//!
//! ## 语音里没有 `@`
//!
//! 人在会上不会念出"at 小七",他们就是喊名字。而且转写会把名字转错——
//! "小七"可能变成"小奇""晓琪""小柒"。所以这里按**别名表**匹配,
//! 别名由部署方配置(把常见的转写错法一并列进去)。
//!
//! ## 宁可漏,不可滥
//!
//! 误触发比漏触发糟得多:会议里 Agent 冷不丁插一句没人问的话,会打断讨论,
//! 而漏了一次人再喊一遍就行。所以:
//!
//! 触发条件是二选一:**别名在句首**,或者**别名后面紧跟标点/空白**。
//!
//! 为什么不能只要求"后面跟标点":真机上撞到过——原话是「小七,刚才说的…」,
//! whisper 把逗号吃掉,转成「小七刚才说的…」,于是喊了它也听不见。
//! 中文本来就不用空格分词,转写又常丢标点,**拿标点当词边界在语音场景里不成立**。
//!
//! 为什么还要留"后面跟标点"这一条:句中称呼(「那么小七,你说呢」)也要能认。
//!
//! 两条都不满足就不触发,于是「这事儿小七七八八的还没定」不会误触发——
//! 它既不在句首,后面也不是标点。

/// 名字后面常跟的语气/称呼字,判词边界时要允许它们紧邻。
const TRAILING_OK: &[char] =
    &['，', ',', '。', '.', '?', '？', '!', '！', '、', ':', ':', ' ', '\u{3000}'];

#[derive(Debug, Clone)]
pub struct MentionRules {
    /// 别名表(含常见转写错法)。
    pub aliases: Vec<String>,
    /// 只在句首这么多**字符**内匹配才算数。0 表示不限制。
    pub head_chars: usize,
}

impl Default for MentionRules {
    fn default() -> Self {
        Self {
            aliases: vec!["小七".into(), "@小七".into(), "A-007".into()],
            head_chars: 12,
        }
    }
}

impl MentionRules {
    /// 这句话是不是在叫我。返回命中的别名。
    pub fn hit<'a>(&'a self, text: &str) -> Option<&'a str> {
        let chars: Vec<char> = text.chars().collect();
        let limit = if self.head_chars == 0 { chars.len() } else { self.head_chars.min(chars.len()) };

        // **长别名优先**:"@小七" 比 "小七" 更具体,命中它才能把 @ 一起剥掉。
        // 按表序匹配的话,结果取决于配置顺序——那是会悄悄变的东西。
        let mut ordered: Vec<&String> = self.aliases.iter().collect();
        ordered.sort_by_key(|a| std::cmp::Reverse(a.chars().count()));

        for alias in ordered {
            let a: Vec<char> = alias.chars().collect();
            if a.is_empty() || a.len() > chars.len() {
                continue;
            }
            for start in 0..=chars.len().saturating_sub(a.len()) {
                if start >= limit {
                    break;
                }
                if chars[start..start + a.len()] != a[..] {
                    continue;
                }
                // 句首,或后面紧跟标点/空白。见模块文档:只要求后跟标点
                // 在中文语音场景不成立(转写会把逗号吃掉)。
                let at_start = start == 0;
                let after = chars.get(start + a.len());
                let boundary = match after {
                    None => true,
                    Some(c) => TRAILING_OK.contains(c) || !is_name_char(*c),
                };
                if at_start || boundary {
                    return Some(alias);
                }
            }
        }
        None
    }

    /// 去掉称呼,留下真正的问题。叫完名字后的那句才是要问的。
    pub fn strip<'a>(&self, text: &'a str, alias: &str) -> &'a str {
        match text.find(alias) {
            Some(i) => text[i + alias.len()..].trim_start_matches(TRAILING_OK).trim(),
            None => text.trim(),
        }
    }
}

/// 可能构成名字的一部分的字符(汉字、字母、数字)。用来判词边界。
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> MentionRules {
        MentionRules::default()
    }

    #[test]
    fn plain_call_is_detected() {
        assert_eq!(r().hit("小七,帮我看看网关那个改动"), Some("小七"));
        assert_eq!(r().hit("@小七 这个怎么办"), Some("@小七"));
        assert_eq!(r().hit("小七?"), Some("小七"));
    }

    /// **句中出现且后面不是标点** ⇒ 不算叫它。
    #[test]
    fn substring_in_mid_sentence_does_not_trigger() {
        assert_eq!(r().hit("这事儿小七七八八的还没定"), None, "不该被 小七 误触发");
        assert_eq!(r().hit("我觉得小七哥说得对"), None, "「小七哥」是另一个人");
    }

    /// **转写会把逗号吃掉。** 真机上原话是「小七,刚才说的…」,
    /// 转成「小七刚才说的…」——只认标点边界的话,喊了也听不见。
    #[test]
    fn call_without_punctuation_still_triggers() {
        assert_eq!(r().hit("小七刚才说的幂等键定在哪一层"), Some("小七"));
        assert_eq!(r().hit("小七你怎么看"), Some("小七"));
        assert_eq!(r().hit("小七帮我查一下网关"), Some("小七"));
    }

    /// **只认句首**:长句中间蹦出同音词,多半是误转写,不该触发。
    /// 误触发比漏触发糟得多——Agent 冷不丁插话会打断讨论。
    #[test]
    fn only_the_head_of_the_sentence_counts() {
        assert_eq!(
            r().hit("我们上周讨论过这个方案后来又改了一版然后小七,说要重做"),
            None,
            "第 20 多个字才出现,多半是误转写"
        );
        assert_eq!(r().hit("那么小七,你说呢"), Some("小七"), "句首附近仍算");
    }

    #[test]
    fn no_mention_is_none() {
        assert_eq!(r().hit("我们开始今天的周会"), None);
        assert_eq!(r().hit(""), None);
    }

    /// 别名表可配:把常见的转写错法一起列进去,否则喊了也听不见。
    #[test]
    fn aliases_cover_mis_transcriptions() {
        let rules = MentionRules {
            aliases: vec!["小七".into(), "小奇".into(), "晓琪".into()],
            ..Default::default()
        };
        assert_eq!(rules.hit("小奇,帮我查一下"), Some("小奇"));
        assert_eq!(rules.hit("晓琪 这个改动有风险吗"), Some("晓琪"));
    }

    /// 去掉称呼后留下的才是问题本身。
    #[test]
    fn stripping_leaves_the_actual_question() {
        let rules = r();
        assert_eq!(rules.strip("小七,帮我看看网关那个改动", "小七"), "帮我看看网关那个改动");
        assert_eq!(rules.strip("@小七 这个怎么办", "@小七"), "这个怎么办");
        assert_eq!(rules.strip("小七?", "小七"), "");
    }
}
