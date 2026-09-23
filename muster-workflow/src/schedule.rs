//! 定时表达式:五字段 cron + 几个人话写法(`daily 12:00` 等),算"某时刻之后的下一次"。
//!
//! 不引入 cron 库:需要的只是匹配与逐分钟前推,写清楚比引依赖便宜,而且中文描述
//! ([`Schedule::describe_zh`])要自己产。日/周两个字段按 Vixie cron 惯例:两者都限定时
//! 取"或",否则取"且"。

use chrono::{DateTime, Datelike, Duration, Local, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    any: bool,
    set: Vec<u32>,
}

impl Field {
    fn any() -> Self {
        Field { any: true, set: Vec::new() }
    }
    fn matches(&self, v: u32) -> bool {
        self.any || self.set.binary_search(&v).is_ok()
    }
    fn single(&self) -> Option<u32> {
        if !self.any && self.set.len() == 1 {
            Some(self.set[0])
        } else {
            None
        }
    }
}

const DOW_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const DOW_ZH: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

fn parse_value(s: &str, names: bool) -> Result<u32, String> {
    if names {
        let l = s.to_ascii_lowercase();
        if let Some(i) = DOW_NAMES.iter().position(|n| l.starts_with(n)) {
            return Ok(i as u32);
        }
    }
    s.parse::<u32>().map_err(|_| format!("`{s}` 不是数字"))
}

fn parse_field(s: &str, min: u32, max: u32, names: bool, what: &str) -> Result<Field, String> {
    let mut set = Vec::new();
    let mut any = false;
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("{what} 字段有空项"));
        }
        let (base, step) = match part.split_once('/') {
            Some((b, st)) => (
                b,
                Some(st.parse::<u32>().map_err(|_| format!("{what} 的步长 `{st}` 不是数字"))?),
            ),
            None => (part, None),
        };
        if step == Some(0) {
            return Err(format!("{what} 的步长不能为 0"));
        }
        let (lo, hi) = if base == "*" {
            if step.is_none() {
                any = true;
                continue;
            }
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            (parse_value(a, names)?, parse_value(b, names)?)
        } else {
            let v = parse_value(base, names)?;
            match step {
                Some(_) => (v, max),
                None => (v, v),
            }
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("{what} 取值 `{part}` 超出 {min}-{max}"));
        }
        let st = step.unwrap_or(1);
        let mut v = lo;
        while v <= hi {
            set.push(v);
            v += st;
        }
    }
    if any {
        return Ok(Field::any());
    }
    if names {
        // 周字段:7 也是周日
        for v in set.iter_mut() {
            if *v == 7 {
                *v = 0;
            }
        }
    }
    set.sort_unstable();
    set.dedup();
    if set.is_empty() {
        return Err(format!("{what} 字段为空"));
    }
    Ok(Field { any: false, set })
}

/// 五字段 cron:分 时 日 月 周。支持 `*`、`a`、`a-b`、`a,b`、`*/n`、`a-b/n`、周几英文名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
    text: String,
}

impl CronExpr {
    pub fn parse(text: &str) -> Result<Self, String> {
        let f: Vec<&str> = text.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!("cron 需要 5 个字段(分 时 日 月 周),收到 {} 个", f.len()));
        }
        Ok(CronExpr {
            minute: parse_field(f[0], 0, 59, false, "分钟")?,
            hour: parse_field(f[1], 0, 23, false, "小时")?,
            dom: parse_field(f[2], 1, 31, false, "日")?,
            month: parse_field(f[3], 1, 12, false, "月")?,
            dow: parse_field(f[4], 0, 7, true, "周")?,
            text: f.join(" "),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn matches(&self, t: &DateTime<Local>) -> bool {
        if !self.minute.matches(t.minute()) || !self.hour.matches(t.hour()) || !self.month.matches(t.month()) {
            return false;
        }
        let dom_ok = self.dom.matches(t.day());
        let dow_ok = self.dow.matches(t.weekday().num_days_from_sunday());
        if !self.dom.any && !self.dow.any {
            dom_ok || dow_ok
        } else {
            dom_ok && dow_ok
        }
    }

    /// `after` 之后(不含)的第一个匹配分钟;一年内找不到返回 `None`。
    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut t = after.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        for _ in 0..(366 * 24 * 60) {
            if self.matches(&t) {
                return Some(t);
            }
            t = t + Duration::minutes(1);
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// 只手动跑。
    Manual,
    Cron(CronExpr),
}

impl Schedule {
    /// 支持:`manual` / `hourly` / `daily HH:MM` / `weekdays HH:MM` /
    /// `weekly <mon…sun> HH:MM` / `monthly <1-31> HH:MM` / 五字段 cron。
    pub fn parse(s: &str) -> Result<Self, String> {
        let raw = s.trim();
        let lower = raw.to_ascii_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        let hm = |w: &str| -> Result<(u32, u32), String> {
            let (h, m) = w.split_once(':').ok_or_else(|| format!("时间 `{w}` 应写成 HH:MM"))?;
            let h: u32 = h.parse().map_err(|_| format!("小时 `{h}` 不是数字"))?;
            let m: u32 = m.parse().map_err(|_| format!("分钟 `{m}` 不是数字"))?;
            if h > 23 || m > 59 {
                return Err(format!("时间 `{w}` 超出 00:00-23:59"));
            }
            Ok((h, m))
        };
        let cron = |e: String| CronExpr::parse(&e).map(Schedule::Cron);
        match words.as_slice() {
            ["manual"] => Ok(Schedule::Manual),
            ["hourly"] => cron("0 * * * *".into()),
            ["daily", t] => {
                let (h, m) = hm(t)?;
                cron(format!("{m} {h} * * *"))
            }
            ["weekdays", t] => {
                let (h, m) = hm(t)?;
                cron(format!("{m} {h} * * 1-5"))
            }
            ["weekly", d, t] => {
                let (h, m) = hm(t)?;
                let d = parse_value(d, true).map_err(|_| format!("周几 `{d}` 不认识(mon…sun 或 0-7)"))?;
                cron(format!("{m} {h} * * {d}"))
            }
            ["monthly", d, t] => {
                let (h, m) = hm(t)?;
                let d: u32 = d.parse().map_err(|_| format!("日期 `{d}` 不是数字"))?;
                cron(format!("{m} {h} {d} * *"))
            }
            _ if words.len() == 5 => CronExpr::parse(raw).map(Schedule::Cron),
            _ => Err("看不懂的 schedule:支持 manual / hourly / daily HH:MM / weekdays HH:MM / \
                      weekly <mon…sun> HH:MM / monthly <1-31> HH:MM / 五字段 cron"
                .into()),
        }
    }

    pub fn is_manual(&self) -> bool {
        matches!(self, Schedule::Manual)
    }

    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            Schedule::Manual => None,
            Schedule::Cron(c) => c.next_after(after),
        }
    }

    /// 界面用的中文描述;认不出常见形态就原样给 cron。
    pub fn describe_zh(&self) -> String {
        let c = match self {
            Schedule::Manual => return "手动".into(),
            Schedule::Cron(c) => c,
        };
        let hm = |h: u32, m: u32| format!("{h:02}:{m:02}");
        if c.month.any {
            match (c.minute.single(), c.hour.single()) {
                (Some(m), Some(h)) => {
                    if c.dom.any && c.dow.any {
                        return format!("每天 {}", hm(h, m));
                    }
                    if c.dom.any && c.dow.set == [1, 2, 3, 4, 5] {
                        return format!("工作日 {}", hm(h, m));
                    }
                    if c.dom.any {
                        if let Some(d) = c.dow.single() {
                            return format!("每{} {}", DOW_ZH[d as usize], hm(h, m));
                        }
                    }
                    if c.dow.any {
                        if let Some(d) = c.dom.single() {
                            return format!("每月 {d} 日 {}", hm(h, m));
                        }
                    }
                }
                (Some(m), None) if c.hour.any && c.dom.any && c.dow.any => {
                    return format!("每小时第 {m} 分");
                }
                _ => {}
            }
        }
        format!("cron {}", c.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Weekday};

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn daily_runs_at_the_next_slot_and_then_tomorrow() {
        let s = Schedule::parse("daily 12:00").unwrap();
        assert_eq!(s.describe_zh(), "每天 12:00");
        let n = s.next_after(at(2026, 9, 23, 11, 59)).unwrap();
        assert_eq!((n.day(), n.hour(), n.minute()), (23, 12, 0));
        let n2 = s.next_after(at(2026, 9, 23, 12, 0)).unwrap();
        assert_eq!((n2.day(), n2.hour()), (24, 12), "正好在时刻上,下一次是明天");
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        let s = Schedule::parse("weekdays 09:30").unwrap();
        assert_eq!(s.describe_zh(), "工作日 09:30");
        // 2026-09-25 是周五
        assert_eq!(at(2026, 9, 25, 10, 0).weekday(), Weekday::Fri);
        let n = s.next_after(at(2026, 9, 25, 10, 0)).unwrap();
        assert_eq!(n.weekday(), Weekday::Mon);
        assert_eq!((n.day(), n.hour(), n.minute()), (28, 9, 30));
    }

    #[test]
    fn cron_steps_ranges_and_names() {
        let s = Schedule::parse("*/15 * * * *").unwrap();
        let n = s.next_after(at(2026, 9, 23, 10, 7)).unwrap();
        assert_eq!((n.hour(), n.minute()), (10, 15));
        assert_eq!(s.describe_zh(), "cron */15 * * * *");

        let w = Schedule::parse("weekly mon 08:00").unwrap();
        assert_eq!(w.describe_zh(), "每周一 08:00");
        let m = Schedule::parse("monthly 1 09:00").unwrap();
        assert_eq!(m.describe_zh(), "每月 1 日 09:00");
        let h = Schedule::parse("hourly").unwrap();
        assert_eq!(h.describe_zh(), "每小时第 0 分");
        // 周日既可写 0 也可写 7
        let sun = Schedule::parse("0 0 * * 7").unwrap();
        assert_eq!(sun.describe_zh(), "每周日 00:00");
    }

    #[test]
    fn dom_and_dow_both_restricted_means_either() {
        // Vixie 惯例:日与周都限定 ⇒ 任一匹配即可。2026-09-25 是周五,先于 10 月 13 日
        let s = Schedule::parse("0 0 13 * fri").unwrap();
        let n = s.next_after(at(2026, 9, 23, 0, 0)).unwrap();
        assert_eq!((n.month(), n.day()), (9, 25));
    }

    #[test]
    fn rejects_nonsense() {
        for bad in ["daily 25:00", "every day", "0 0 * *", "61 * * * *", "0 0 32 * *", "*/0 * * * *", "weekly xyz 08:00"] {
            assert!(Schedule::parse(bad).is_err(), "{bad} 应被拒绝");
        }
        assert!(Schedule::parse("manual").unwrap().is_manual());
        assert!(Schedule::parse("manual").unwrap().next_after(at(2026, 1, 1, 0, 0)).is_none());
    }
}
