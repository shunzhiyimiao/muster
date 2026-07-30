# muster-eval(任务 A7 · v0.1)

工具调用评测集:**20 个固定样本 × 双 provider × N 次试跑**,产出 G0′ 闸门证据
(`report.md` + `results.json`)。

## 跑法

```bash
# 正式测量(W3 闸门用,建议 trials=3)
DEEPSEEK_API_KEY=… DASHSCOPE_API_KEY=… \
cargo run -p muster-eval -- --config provider.example.toml \
  --providers deepseek,qwen --trials 3 --out eval-reports

# 查看评测集 / 调试单个样本 / 看报告长相(零 API 成本)
cargo run -p muster-eval -- --list
cargo run -p muster-eval -- --filter mt_ --providers deepseek --trials 1
cargo run -p muster-eval --example demo_report
```

退出码:0 = 闸门通过,1 = 未通过,2 = 配置错误 → 可直接挂 CI。

## 口径(报告中同样声明,评审逐条过)

1. **成功率 = Pass / (Pass + Fail)**。传输失败(Infra)不进分母、单独披露;
   Infra 占比 > 10% 时该 provider 结论**判无效**——不允许用掉线稀释分母凑数。
2. **评分是确定性纯函数**,不用 LLM 判卷:工具名、JSON 合法性、schema 外字段、
   字段值/类型/枚举、跨调用覆盖,全部代码断言,规则随报告附录 A 公开。
3. **走流式生产路径**(chat_stream + 增量重组),评的是我们实际要用的通道。
4. **temperature=0**,多 trial 仍可能波动(采样以外的非确定性),trials=3 取全体试次统计。
   思考型模型按其硬约束显式偏离(如 Kimi K3 仅接受 temperature=1:`--temperature 1 --max-tokens 4096`,
   思考计入输出 token),实际取值随报告公示,不允许静默偏离。
5. Auth/配置错误 → provider 级作废,当场报变量名,不静默重试。

## 评测集构成(8 类 × 20 样本)

基础 2 / 工具选择 1 / 参数抽取 4 / 类型正确性 3 / 负样本 2(不该调时不调)/
并行调用 1 / 多轮衔接 3 / 鲁棒性 4(引号转义、中文路径、多行内容、长 diff 定位)。
分布本身有快照测试锁定——改样本必须显式改分布断言,防止评测集被悄悄稀释。

## 诚实边界

- 度量对象是**「系统提示词 + provider」整体**,不是裸模型。A1 的正式 agent
  提示词落地后必须用它重跑;当前提示词见 `runner.rs::SYSTEM_PROMPT`。
- 负样本(`no_tool_*`)天然更难,若 provider 在此系统性失分,优先调提示词
  再下"能力不行"的结论。
- `parallel_two_cities` 若系统性失败,不必判死——那是给 Runner 的设计信号:
  该 provider 需要"逐个调用"的串行回退路径。
- **`mt_read_then_comment` 是已知的样本-模型张力**(a1-v1/v2 下均系统性失分):
  样本期望"恰好 1 条评审评论",而夹具代码 `auth.rs` 里客观存在两个问题
  (`unwrap()` panic 与密码明文传递),模型各开一条是**更负责任**的行为。
  该样本要测的是"多轮衔接:读文件 → 基于结果评论",数量并非被测能力。
  **明知如此仍不放宽期望**:闸门已在 90% 以上通过,没有为数字改样本的必要;
  放宽单样本上限与"评测集不得被悄悄稀释"的原则冲突。留作已登记事实,
  若将来重设样本,应在改分布断言的同时显式讨论。
- 20×3=60 试次的统计粒度有限;90% 阈值上下 1 个样本的波动请结合失败明细
  人工判读,不要只看总数。
