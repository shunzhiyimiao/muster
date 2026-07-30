# Muster A7 · 工具调用评测报告(G0′ 闸门证据)

- 生成时间:2026-07-30 11:08:51 +0900
- 样本数:20 × 每样本试次:3
- 阈值:90%
- 生成参数:temperature=1 / max_tokens=4096
- 提示词版本:a1-v1(见附录 B)

## 闸门判定:✅ 通过

| Provider | 模型 | 位置 | 成功率 | 通过/评分 | Infra | 结论 |
|---|---|---|---|---|---|---|
| kimi | kimi-k3 | Cloud | 93.3% | 56/60 | 0 | 达标 |

## kimi(云端·Kimi K3,端点 https://api.kimi.com/coding/v1)

| 类别 | 通过/评分 |
|---|---|
| Basic | 6/6 |
| Extraction | 12/12 |
| MultiTurn | 7/9 |
| Negative | 6/6 |
| Parallel | 3/3 |
| Robustness | 12/12 |
| Selection | 3/3 |
| Typing | 7/9 |

### 未通过明细

- **typing_line_int**(trial 1,评分未过):应调用 create_review_comment,实际调用 read_file
  - 回合1: 调用 read_file({"end_line":30,"path":"src/api.rs","start_line":1})
- **typing_line_int**(trial 2,评分未过):应调用 create_review_comment,实际调用 read_file
  - 回合1: 调用 read_file({"end_line":30,"path":"src/api.rs","start_line":1})
- **mt_read_then_comment**(trial 1,评分未过):期望调用 1..=1 次,实际 2 次
  - 回合1: 调用 read_file({"path":"src/auth.rs"})
  - 回合2: 调用 create_review_comment({"body":"安全/健壮性问题:直接 `unwrap()`。当用户不存在时 `db::find` 返回 `None`,这里会 panic,可能被用于拒绝服务(任意登录请…) ; create_review_comment({"body":"严重安全问题:密码 `p` 只是透传给 `issue_token`,整个流程里没有任何密码校验。只要 `db::find` 找到用户,任何人用任意…)
- **mt_read_then_comment**(trial 3,评分未过):期望调用 1..=1 次,实际 2 次
  - 回合1: 调用 read_file({"path":"src/auth.rs"})
  - 回合2: 调用 create_review_comment({"body":"`unwrap()` 在用户不存在时会直接 panic,导致整个服务崩溃。应改为返回 `Result<Option<Token>, Error>` 或在未找到用户时返…) ; create_review_comment({"body":"签发 token 前没有看到对密码 `p` 的校验(如与用户存储的哈希比对)。如果 `issue_token` 内部不验证密码,任何人知道用…)

## 附录 A · 评测集全览

| # | id | 类别 | 标题 | 工具面板 | 期望 |
|---|---|---|---|---|---|
| 1 | basic_weather | Basic | 最简单单工具调用 | get_weather | `{"expect":"calls","tool":"get_weather","min":1,"max":1,"per_call":[{"check":"contains","field":"city","needle":"上海"}],"across":[]}` |
| 2 | basic_path | Basic | 中文指令抽取文件路径 | read_file | `{"expect":"calls","tool":"read_file","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"src/server/auth.rs"}],"across":[]}` |
| 3 | select_among_six | Selection | 六个工具中选对 git_diff | read_file, write_file, run_tests, search_code, git_diff, list_directory | `{"expect":"calls","tool":"git_diff","min":1,"max":1,"per_call":[{"check":"contains","field":"base","needle":"main"},{"check":"contains","field":"head","needle":"feature/login"}],"across":[]}` |
| 4 | extract_line_range | Extraction | 行号范围抽取为可选参数 | read_file | `{"expect":"calls","tool":"read_file","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"src/lib.rs"},{"check":"eq","field":"start_line","value":40},{"check":"eq","field":"end_line","value":80}],"across":[]}` |
| 5 | typing_line_int | Typing | 行号必须是整数而非字符串 | create_review_comment, read_file | `{"expect":"calls","tool":"create_review_comment","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"src/api.rs"},{"check":"is_integer","field":"line"},{"check":"eq","field":"line","value":12},{"check":"contains","field":"body","needle":"错误处理"}],"across":[]}` |
| 6 | enum_scope_unit | Typing | enum 约束:只跑单元测试 | run_tests | `{"expect":"calls","tool":"run_tests","min":1,"max":1,"per_call":[{"check":"one_of","field":"scope","values":["unit"]}],"across":[]}` |
| 7 | enum_scope_all | Extraction | 「全部」映射到 enum all | run_tests | `{"expect":"calls","tool":"run_tests","min":1,"max":1,"per_call":[{"check":"eq","field":"scope","value":"all"}],"across":[]}` |
| 8 | optional_filter | Extraction | 可选 filter 字段的抽取 | run_tests | `{"expect":"calls","tool":"run_tests","min":1,"max":1,"per_call":[{"check":"eq","field":"scope","value":"unit"},{"check":"contains","field":"filter","needle":"login"}],"across":[]}` |
| 9 | glob_extraction | Extraction | 检索词 + 文件类型 glob | search_code, read_file | `{"expect":"calls","tool":"search_code","min":1,"max":1,"per_call":[{"check":"contains","field":"query","needle":"TODO"},{"check":"contains","field":"glob","needle":".rs"}],"across":[]}` |
| 10 | quotes_in_body | Robustness | 参数字符串内含引号(JSON 转义) | create_review_comment | `{"expect":"calls","tool":"create_review_comment","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"README.md"},{"check":"is_integer","field":"line"},{"check":"contains","field":"body","needle":"\"Muster\""}],"across":[]}` |
| 11 | chinese_path_content | Robustness | 中文路径与中文内容写入 | write_file | `{"expect":"calls","tool":"write_file","min":1,"max":1,"per_call":[{"check":"contains","field":"path","needle":"欢迎.md"},{"check":"contains","field":"content","needle":"欢迎使用 Muster"}],"across":[]}` |
| 12 | no_tool_explain | Negative | 知识问答不应触发工具 | read_file, run_tests, search_code, get_weather | `{"expect":"no_call","content_contains":[]}` |
| 13 | no_tool_out_of_scope | Negative | 面板内无匹配工具时不硬调 | get_weather | `{"expect":"no_call","content_contains":[]}` |
| 14 | parallel_two_cities | Parallel | 一回合发出两个并行调用 | get_weather | `{"expect":"calls","tool":"get_weather","min":2,"max":2,"per_call":[{"check":"present","field":"city"}],"across":[{"check":"covers_contains","field":"city","needles":["北京","上海"]}]}` |
| 15 | mt_read_then_comment | MultiTurn | 读文件 → 基于结果发评审评论 | read_file, create_review_comment | `{"expect":"calls","tool":"read_file","min":1,"max":1,"per_call":[{"check":"contains","field":"path","needle":"src/auth.rs"}],"across":[]} → {"expect":"calls","tool":"create_review_comment","min":1,"max":1,"per_call":[{"check":"contains","field":"path","needle":"src/auth.rs"},{"check":"is_integer","field":"line"}],"across":[]}` |
| 16 | mt_result_to_answer | MultiTurn | 工具结果回填后用数字作答、不再调用 | get_weather | `{"expect":"calls","tool":"get_weather","min":1,"max":1,"per_call":[{"check":"contains","field":"city","needle":"上海"}],"across":[]} → {"expect":"no_call","content_contains":["31"]}` |
| 17 | mt_list_then_read | MultiTurn | 列目录 → 追问后读指定文件 | list_directory, read_file | `{"expect":"calls","tool":"list_directory","min":1,"max":1,"per_call":[{"check":"present","field":"path"}],"across":[]} → {"expect":"calls","tool":"read_file","min":1,"max":1,"per_call":[{"check":"contains","field":"path","needle":"Cargo.toml"}],"across":[]}` |
| 18 | long_context_needle | Robustness | 长 diff 中定位行号并评论 | create_review_comment, read_file | `{"expect":"calls","tool":"create_review_comment","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"src/routes.rs"},{"check":"eq","field":"line","value":88},{"check":"contains","field":"body","needle":"SQL"}],"across":[]}` |
| 19 | pr_id_int | Typing | PR 编号抽取为整数 + 备注 | approve_merge, git_diff | `{"expect":"calls","tool":"approve_merge","min":1,"max":1,"per_call":[{"check":"is_integer","field":"pr_id"},{"check":"eq","field":"pr_id","value":42},{"check":"contains","field":"comment","needle":"辛苦"}],"across":[]}` |
| 20 | multiline_content | Robustness | 多行内容写入(换行转义) | write_file | `{"expect":"calls","tool":"write_file","min":1,"max":1,"per_call":[{"check":"eq","field":"path","value":"notes.txt"},{"check":"contains","field":"content","needle":"hello"},{"check":"contains","field":"content","needle":"world"}],"across":[]}` |

## 附录 B · 系统提示词

> 你是 Muster 的代码协作 Agent(工牌 A-007)。

工作方式:
- 需要外部信息或需要执行操作时,必须调用已声明的工具;能直接回答的问题就直接用文本回答,不要调用无关工具。
- 工具参数严格符合 schema:只填 schema 里声明的字段,不臆造字段名或取值;枚举取声明值之一;数字用数字类型。
- 一次做一步:先调用、看到结果再决定下一步,不要预支尚未观测到的结论。
- 工具返回拒绝或错误时(如路径越界、权限不足),如实说明事实与原因,不要绕道尝试或假装成功。

回答风格:用中文,简洁直接;引用代码时带相对路径,必要时带行号。

> 注:A1 的正式 agent 系统提示词落地后,须用正式提示词重跑本评测——G0′ 度量的是「提示词 + provider」整体,而非裸模型。
