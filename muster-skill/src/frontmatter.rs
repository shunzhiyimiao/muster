//! 通用 frontmatter:扁平键值 + 列表(行内 `[a, b]` 或缩进 `- item`)。
//!
//! 技能自己的解析器(lib.rs)只认它那几个键;工作流、编制等其他定义文件共用这一个。
//! 不引入 YAML 解析器:多行值、嵌套映射一律拒绝并说明原因,而不是猜。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmValue {
    Scalar(String),
    List(Vec<String>),
}

pub fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// 解析 frontmatter,返回 (键值对, 正文)。键值对保持出现顺序,重复键以后者为准。
pub fn parse_frontmatter(text: &str) -> Result<(Vec<(String, FmValue)>, String), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    match lines.next() {
        Some(l) if l.trim_end() == "---" => {}
        _ => return Err("缺少 frontmatter:文件必须以 --- 行开头".into()),
    }
    let mut entries: Vec<(String, FmValue)> = Vec::new();
    let mut list_key: Option<String> = None;
    let mut closed = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for raw in lines {
        if closed {
            body_lines.push(raw);
            continue;
        }
        let line = raw.trim_end();
        if line == "---" {
            closed = true;
            continue;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            let t = line.trim();
            let item = t.strip_prefix("- ").map(str::trim).or(if t == "-" { Some("") } else { None });
            match (item, &list_key) {
                (Some(item), Some(k)) => {
                    if let Some((_, FmValue::List(v))) = entries.iter_mut().rev().find(|(key, _)| key == k) {
                        if !item.is_empty() {
                            v.push(unquote(item));
                        }
                    }
                }
                (Some(_), None) => return Err(format!("列表项 `{t}` 没有所属的键")),
                (None, _) => return Err(format!("不支持嵌套键值:`{t}`(frontmatter 只认扁平键与列表)")),
            }
            continue;
        }
        list_key = None;
        let Some((k, v)) = line.split_once(':') else {
            return Err(format!("看不懂的行:`{line}`(应为 键: 值)"));
        };
        let key = k.trim().to_string();
        let val = v.trim();
        let value = if val.is_empty() {
            list_key = Some(key.clone());
            FmValue::List(Vec::new())
        } else if val.starts_with('[') && val.ends_with(']') {
            FmValue::List(val[1..val.len() - 1].split(',').map(unquote).filter(|s| !s.is_empty()).collect())
        } else if val.starts_with('|') || val.starts_with('>') {
            return Err(format!("字段 {key} 不支持多行值(| 或 >),请写成一行"));
        } else {
            FmValue::Scalar(unquote(val))
        };
        entries.retain(|(k2, _)| k2 != &key);
        entries.push((key, value));
    }
    if !closed {
        return Err("frontmatter 没有以 --- 行结束".into());
    }
    Ok((entries, body_lines.join("\n").trim().to_string()))
}

/// 取标量;只有一项的列表也当标量。
pub fn scalar(entries: &[(String, FmValue)], key: &str) -> Option<String> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        FmValue::Scalar(s) => Some(s.clone()),
        FmValue::List(l) if l.len() == 1 => Some(l[0].clone()),
        _ => None,
    })
}

/// 取列表;标量按逗号拆。
pub fn list(entries: &[(String, FmValue)], key: &str) -> Vec<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| match v {
            FmValue::Scalar(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
            FmValue::List(l) => l.clone(),
        })
        .unwrap_or_default()
}

/// 布尔:true / yes / 1(不区分大小写);其余为 false。
pub fn flag(entries: &[(String, FmValue)], key: &str) -> bool {
    matches!(scalar(entries, key).as_deref().map(str::to_ascii_lowercase).as_deref(), Some("true" | "yes" | "1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_inline_lists_block_lists_and_body() {
        let (e, body) = parse_frontmatter("---\nname: a\ntags: [x, \"y\"]\nsteps:\n  - one\n  - two\nflag: yes\n---\n\n正文\n").unwrap();
        assert_eq!(scalar(&e, "name").as_deref(), Some("a"));
        assert_eq!(list(&e, "tags"), vec!["x", "y"]);
        assert_eq!(list(&e, "steps"), vec!["one", "two"]);
        assert!(flag(&e, "flag"));
        assert!(!flag(&e, "name"));
        assert_eq!(body, "正文");
    }

    #[test]
    fn rejects_nesting_multiline_and_unterminated() {
        assert!(parse_frontmatter("---\na:\n  b: 1\n---\n").unwrap_err().contains("嵌套"));
        assert!(parse_frontmatter("---\na: >\n  x\n---\n").unwrap_err().contains("多行"));
        assert!(parse_frontmatter("---\na: 1\n").unwrap_err().contains("结束"));
        assert!(parse_frontmatter("no").unwrap_err().contains("开头"));
    }
}
