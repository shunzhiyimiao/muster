# Constellation M0 可验证 Agent 工作台开发计划
## Verified Patch Workbench / 可验证补丁工作台

- **计划版本**：v0.2
- **制定日期**：2026-07-16
- **状态**：执行基线草案
- **开发资源**：1 名全职开发者，5 天/周
- **计划周期**：6 周，30 个工作日
- **核心技术栈**：Rust 2024 + Tauri 2 + React + TypeScript + SQLite + JSON/YAML
- **首个演示场景**：AUTH-001 登录锁定契约修复
- **实现原则**：clean-room 独立实现，不复制或依赖第三方泄露源码

---

# 1. 执行摘要

Constellation M0 的目标不是完成一个通用 AI 操作系统，也不是复刻 Codex。M0 要交付一个具有 Codex 核心交互体验、但具备更严格控制链的本地 Agent 工作台：

> 用户可以输入任务、观看执行过程、审查 Diff、批准修改和执行回滚；与此同时，Agent 无法绕过 Namespace、Typed Operation、Verifier、Evidence、Autonomy Gate、Checkpoint 和 Event Log 直接改变真实工作区。

M0 产品暂定名为：

> **Verified Patch Workbench：可验证补丁工作台**

M0 只跑通一条窄而完整的操作系统级闭环：

~~~text
Intent
  → Session Namespace
  → Form
  → Canonical SkillScript DAG
  → Static Inspector
  → Proposal-only Agent
  → Scratch Workspace
  → Verifier
  → Evidence Registry
  → Autonomy Gate
  → Human Approval Grant
  → Dynamic Interlock
  → Durable Checkpoint
  → Workspace Commit Adapter
  → Event Log + Responsibility Map
  → Rollback
~~~

M0 成功的判断标准不是“界面看起来像 Codex”，而是：

> 用户能够完成一次 Codex-like 修复流程，同时系统可以证明 Agent 无法绕过控制链修改真实工作区。

## 1.1 v0.2 已冻结与仍开放的决策

已冻结的宪法级决策：

- Rust 承担可信 Kernel 和全部真实 Effect，TypeScript 只承担 UI 与 Projection。
- Agent、模型和外部运行时只能生成 Proposal。
- DenyUnmeasured 不能被人工授权覆盖。
- 真实工作区写入在 M0 中一律需要精确人工授权。
- Approval Grant 必须绑定 Plan、Proposal、Resource Version 和 Effect Scope。
- Event、Evidence 和 MeasurementRecord 采用 append-only 语义。
- Checkpoint 必须先于真实写入。
- Constellation 使用独立 clean-room 仓库和 Git 历史。

仍可通过实现经验调整的工程决策：

- Kernel 内部模块何时拆成独立 crate；
- SQLite 表和 Projection 的具体索引；
- Desktop 的视觉系统和组件库；
- Verifier 的内部实现和性能优化；
- M1 真实模型供应商及通信协议。

---

# 2. 产品定义

## 2.1 M0 的四个核心体验

1. **输入任务**：用户选择内置 AUTH-001 任务，或输入“修复 AUTH-001”。
2. **观看执行**：用户可以看到 Namespace、DAG、节点状态和局部 Context。
3. **审查与批准**：用户可以同时查看 Diff、验证报告、Evidence、Gate 决策和目标 Resource 版本。
4. **解释与恢复**：每次动作都能生成责任地图，并能通过 Checkpoint 完整回滚。

## 2.2 Golden Path

演示任务：

> 检查示例 TypeScript 项目的 AUTH-001 登录锁定契约，提出缺失修改，在隔离环境验证，经过精确授权后写入真实工作区，并能够恢复到修改前状态。

完整用户流程：

1. 启动工作台并创建 Session。
2. Session 挂载只读契约、源码 Resource、Scratch Resource 和固定 Verifier。
3. 用户选择 AUTH-001 任务。
4. 系统将任务映射到预置 Form；M0 不做通用自然语言到 DAG 编译。
5. Form 编译为规范化 SkillScript DAG，并生成稳定的 Plan Hash。
6. Static Inspector 在执行前检查 Resource、Capability、Verifier、Checkpoint 和 Compensation 声明。
7. Proposal Adapter 生成结构化 Patch；Adapter 没有真实文件写权限。
8. Patch 只应用到 Scratch，并由固定 Verifier 和测试执行器验证。
9. Review 页面分别展示 Verification Grade 和 Autonomy Decision。
10. 有效 Evidence 齐全时，真实工作区写入仍返回 RequireHuman。
11. 用户批准一项精确绑定的 Operation，生成一次性 Approval Grant。
12. 写入前重新运行 Dynamic Interlock 和 Autonomy Gate。
13. 系统先持久化 Checkpoint，再执行真实 Effect。
14. Event Log 记录完整因果链，并生成 Responsibility Map。
15. 用户可以执行 Rollback；历史不删除，只追加回滚事件。

## 2.3 M0 的主要产品页面

| 页面 | 主要职责 |
|---|---|
| Task / Session | 输入 Intent、启动任务、查看 Session 状态 |
| Namespace Explorer | 查看当前 Session 可见的 Mounted Resources |
| Plan View | 展示规范化 DAG、Plan Hash、Inspector 结果和节点状态 |
| Proposal Review | 展示 Patch Diff、目标 Resource、版本和 Effect Scope |
| Verification & Evidence | 分别展示原子断言、Grade、风险参数和 Evidence 生命周期 |
| Approval | 创建绑定具体 Operation 的一次性人工授权 |
| Chronicle | 展示 append-only Event Timeline 和状态变化 |
| Explain | 输出逐动作 Responsibility Map 和完整允许/拒绝原因 |
| Checkpoint & Rollback | 展示恢复点、提交状态和回滚操作 |

## 2.4 信任边界与威胁模型

M0 默认以下主体不可信：

- Agent 和模型输出；
- Proposal Adapter 返回的 Patch；
- 用户界面提交的参数；
- Form、YAML 和外部进程输出；
- 过期、示例、待测量或被驳回的 Evidence；
- 审批后发生变化的工作区状态。

M0 的最小可信计算基包括：

- Constellation Kernel 的纯领域规则；
- 版本化 Policy；
- Dynamic Interlock；
- 私有构造 Execution Permit 的签发路径；
- Workspace Commit Adapter；
- Append-only Event/Evidence Store；
- Checkpoint 与恢复实现。

UI 不持有文件系统、Shell 或数据库写权限。Proposal Adapter 不接收真实工作区写句柄。唯一真实写入调用链是：

~~~text
Validated Command
  → Kernel Decision
  → Matching Human Grant
  → Dynamic Interlock
  → Execution Permit
  → Workspace Commit Adapter
~~~

M0 不声称隔离已经获得本机任意代码执行权的恶意程序，也不声称抵御被攻破的操作系统。M0 要证明的是：在 Constellation 自身 API、进程权限和 Adapter 边界内，不存在绕过控制链的合法写路径。

---

# 3. 硬性设计规则

1. **可信默认值为 U，而不是通过。**
2. **A0/A1/B/C/U 是验证等级，不是自主权等级。**
3. **Verification Grade 不得直接转换成 Autonomy Decision。**
4. **所有外部 Effect 必须是类型化 Operation。**
5. **Operation 必须声明目标 Resource、输入版本、预期副作用、Effect Scope 和补偿语义。**
6. **Proposal/Model Adapter 永远没有真实工作区写权限。**
7. **只有 Workspace Commit Adapter 持有真实写能力。**
8. **Workspace Commit Adapter 只接受由 Kernel 私有构造的 Execution Permit。**
9. **Static Inspector 与 Dynamic Interlock 缺一不可。**
10. **任何缺少有效风险 Evidence 的变量都导致 deny_unmeasured。**
11. **deny_unmeasured 是硬拒绝，不允许人工批准覆盖。**
12. **Proof Status 与 Assumption Status 必须分开检查。**
13. **MeasurementRecord 永不修改、永不删除。**
14. **Evidence 的过期、替代和驳回只能追加 Event。**
15. **Current State 必须由 Fold(EventLog, as_of) 产生。**
16. **Fold、Gate 和 Verifier 内不得隐式读取系统当前时间。**
17. **真实工作区 Effect 前必须创建持久化 Checkpoint。**
18. **高风险、不可逆或跨设备 Effect 必须人工授权；M0 不实现这些真实 Effect。**
19. **完整历史进入 Event Log，不默认塞入每次模型上下文。**
20. **Context 是 Form、State 和 Event Log 的局部投影。**
21. **Namespace 与 Typed Resource 是机制；风险阈值和审批规则是版本化策略。**
22. **M0 关闭真实工作区自主写入；自主 Allow 最多用于受限 Scratch Operation。**
23. **系统采用 clean-room 实现，不复制外部 Agent 产品的非公开源码。**

---

# 4. 关键决策语义

## 4.1 Verification 与 Autonomy 分离

Verification 回答：

> 系统对这项 Proposal 的正确性掌握到什么程度？

Autonomy Gate 回答：

> 在当前 Evidence、风险、授权、政策、资源版本和可恢复性条件下，系统是否可以执行这项 Effect？

因此：

- A0 不能自动产生 Allow。
- U 必须阻止真实 Effect。
- 有效 Evidence 齐全但策略禁止自主执行时，返回 RequireHuman。
- 缺失或无效 Evidence 时，返回 DenyUnmeasured。
- 人工授权不能覆盖 DenyUnmeasured 或 Hard Policy。

人工授权后，策略事实仍然可以保持 RequireHuman。Executor 只有在 RequireHuman、有效匹配的 Human Grant、Interlock 通过和其他 Gate 条件同时成立时，才能签发 Execution Permit。该 Permit 的 Basis 记录为 Human Grant，不把决策模糊改写成自主 Allow。

## 4.2 人工授权必须精确绑定

Approval Grant 至少绑定：

- Session ID
- Operation ID
- Intent ID
- Plan Hash
- Proposal Hash
- 目标 Resource ID
- 目标 Resource Version
- Effect Scope
- Policy Version
- 签发时间与过期时间
- 单次使用状态

任何绑定项变化，旧 Grant 都必须失效。

## 4.3 真实写入采用两阶段控制

~~~text
Review Preview
  → Human Grant
  → Re-resolve Namespace
  → Recheck Resource Version
  → Dynamic Interlock
  → Re-evaluate Gate
  → Durable Checkpoint
  → EffectPrepared Event
  → Workspace Write
  → EffectCommitted Event
~~~

如果进程在 EffectPrepared 和 EffectCommitted 之间崩溃，恢复逻辑必须依据 pre/post Hash 判断是否补记完成或执行回滚，不能假设文件系统写入与 SQLite 事务天然原子。

## 4.4 External Effect 分类

M0 不用“内部操作”作为绕过 Operation 的理由：

| Effect | 分类 | M0 控制方式 |
|---|---|---|
| Scratch 写入 | 受限、可丢弃 Effect | 需要 Scratch Capability；不能解析到真实工作区 |
| 启动测试进程 | 外部进程 Effect | 固定可执行文件、固定参数、资源限额和输出捕获 |
| 创建 Checkpoint | 持久化 Effect | 类型化 Operation；记录 Manifest 和 Hash |
| 真实工作区写入 | 真实外部 Effect | Human Grant + Interlock + Permit + Checkpoint |
| Rollback | 补偿性外部 Effect | 新 Operation；保留原历史并追加回滚事件 |

## 4.5 核心实体与状态机

| 实体 | 作用 |
|---|---|
| Principal | 提出、批准或执行动作的主体 |
| Session | Namespace、Intent、Plan 和 Event 的隔离边界 |
| Namespace / Mount | 决定 Session 可以解析和看见哪些 Resource |
| Typed Resource | 暴露版本化 Operation，而不是任意宿主对象 |
| Capability Grant | 约束 Principal 可以对哪个 Resource 调用什么 Operation |
| Intent / Form / Plan Lock | 从用户目标到规范化、可复现的执行计划 |
| Proposal | 不带真实执行权的候选改变 |
| Verification Report | 针对原子 Claim 的验证结果和 Grade |
| Evidence Reference | 将风险变量绑定到有效 MeasurementRecord |
| Approval Grant | 对一项具体 Effect 的精确、一次性人工授权 |
| Execution Permit | Kernel 在所有前置条件成立后签发的不可伪造执行凭证 |
| Checkpoint | Effect 前的持久化恢复点 |
| Event / Current State | 不可变历史与 Fold 生成的当前投影 |

Operation 最小状态迁移：

~~~text
Draft
  → Compiled
  → Inspected
  → Proposed
  → Verified
  → AwaitingApproval
  → Approved
  → Interlocked
  → Checkpointed
  → Prepared
  → Committed
  → VerifiedAfterCommit
~~~

任意非终态都可以进入结构化 Denied、Failed 或 Cancelled。Committed 之后可以追加 RolledBack，但不得删除原 Committed 历史。

---

# 5. 技术与仓库方案

## 5.1 语言选择

| 层 | 技术 | 职责 |
|---|---|---|
| Control Kernel | Rust 2024 | 类型、Namespace、Capability、Inspector、Interlock、Gate、Fold |
| Application / Runtime | Rust 2024 | DAG 调度、命令状态机、端口调用、恢复 |
| Adapters | Rust 为主 | SQLite、本地文件、Git Checkpoint、Verifier、模型适配 |
| Desktop Shell | Tauri 2 | 进程、窗口、Rust/Frontend IPC、最小 Capability |
| Desktop UI | React + TypeScript strict | Task、DAG、Diff、Evidence、Approval、Timeline |
| Persistence | SQLite | Append-only Event、Evidence、Session Projection |
| Protocol | JSON | Rust 与 TypeScript 的版本化通信 |
| Form | YAML | M0 手写、版本化的任务定义 |

Rust 是主语言，负责可信计算基和所有真实 Effect。TypeScript 只负责界面与状态投影，不直接访问文件、Shell 或数据库。

## 5.2 依赖方向

~~~text
constellation-kernel
        ↑
constellation-application
        ↑
ports / adapters
        ↑
CLI / Tauri Desktop
~~~

Kernel 必须尽量保持纯函数和无 I/O。Adapter 不得自行决定授权；Orchestrator 不得绕过 Gate 直接调用 Commit Adapter。

## 5.3 建议仓库结构

Day 1 不立即创建十几个物理 crate。先让领域边界作为 Kernel 内部模块稳定，再按明确依赖关系拆分。

~~~text
Constellation/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── README.md
│
├── crates/
│   ├── constellation-kernel/
│   │   └── src/
│   │       ├── ids.rs
│   │       ├── namespace.rs
│   │       ├── resource.rs
│   │       ├── capability.rs
│   │       ├── form.rs
│   │       ├── inspector.rs
│   │       ├── verifier.rs
│   │       ├── evidence.rs
│   │       ├── policy.rs
│   │       ├── event.rs
│   │       └── fold.rs
│   └── constellation-application/
│
├── adapters/
│   ├── constellation-sqlite/
│   ├── constellation-local-files/
│   ├── constellation-git/
│   ├── constellation-mock-model/
│   └── constellation-test-runner/
│
├── apps/
│   ├── constellation-cli/
│   └── desktop/
│
├── examples/
│   └── auth-001/
│
├── experiments/
│   └── fault-injection/
│
└── docs/
    ├── constitution.md
    ├── workbench-v0.md
    ├── responsibility-map.md
    └── decisions/
~~~

拆分 crate 的条件：

- 出现明确、稳定的依赖边界；
- 模块需要独立测试或独立发布；
- I/O Adapter 需要与纯 Kernel 隔离；
- 编译时间或 feature 管理出现真实需求。

---

# 6. 六周、30 个工作日执行表

## 第 1 周：不可绕过的语义内核

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 1 | 初始化独立仓库、宪法、ADR 和 CI | Cargo Workspace、README、constitution、CI | fmt、clippy、test 可运行；无外部源码依赖 |
| Day 2 | 核心 ID、状态和控制类型 | SessionId、ResourceId、OperationId、VerificationGrade、Decision | 默认 Grade 为 U；外部不能伪造 Permit |
| Day 3 | 虚拟 ResourcePath 与 Namespace | Session、Mount、路径解析、ResourceRef | 两个 Session 同路径解析到不同 Resource；Traversal 被拒绝 |
| Day 4 | Typed Resource、Operation、Capability | OperationSchema、CapabilityGrant、拒绝原因 | 未声明 Operation 和缺 Capability 均 fail closed |
| Day 5 | Event 模型与确定性 Fold | 内存 append-only Event Store、Current State | 相同 Event Log 得到相同状态；乱序/重复序号拒绝 |

第 1 周退出条件：

- Agent 只能看到 Session Namespace 中的 Resource。
- 默认状态是未知和拒绝。
- 所有外部行为开始被表达为类型化 Operation。
- Event Log 可以重建当前状态。

## 第 2 周：只提案、不写真实工作区

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 6 | 定义 AUTH-001 Form 和 SkillScript | 版本化 Form Schema、固定演示 Form | Form 可解析；未知版本拒绝 |
| Day 7 | 编译规范化 DAG | 拓扑排序、稳定 Node ID、Plan Hash | 相同 Form 始终得到相同 Hash |
| Day 8 | Static Inspector | 环、Resource、Capability、Verifier、Checkpoint、Compensation 检查 | 六类非法计划均在执行前停止 |
| Day 9 | Proposal Port 与 Context Projection | Mock Proposal Adapter、局部上下文 | Adapter 不得到真实路径写权限和 Shell |
| Day 10 | Scratch Overlay 与 Dry Run | Patch 结构、隔离工作区、CLI Dry Run | Patch 只改变 Scratch；真实工作区 Hash 不变 |

第 2 周退出条件：

- 输入 AUTH-001 Intent 可以产生确定性 Proposal。
- CLI 能展示 Plan、Patch 和拒绝原因。
- 即使 Proposal Adapter 失控，也不能修改真实工作区。

## 第 3 周：Verifier、Evidence 与 Gate

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 11 | Verifier 接口和原子断言 | VerificationReport、A0/A1/B/C/U | 未覆盖断言保持 U |
| Day 12 | AUTH-001 固定验证器 | Contract Verifier、Test Runner Adapter | 干净和缺陷 Fixture 得到可重复结果 |
| Day 13 | SQLite Event/Evidence Store | Append-only 表、触发器、Store Adapter | UPDATE/DELETE 在数据库层被拒绝 |
| Day 14 | MeasurementRecord 生命周期 | pending/active/example/expired/superseded/rejected Event | 无效 Evidence 立即退出当前决策状态 |
| Day 15 | Autonomy Gate v0 | 风险、授权、硬禁区、Inspector/Interlock 条件 | A0 不自动 Allow；缺 Evidence 为 DenyUnmeasured |

第 3 周退出条件：

- Verification 和 Autonomy 是两个独立结果。
- DenyUnmeasured 不能被人工授权覆盖。
- 有效 Evidence 齐全但禁止自主写入时返回 RequireHuman。

## 第 4 周：精确授权、真实写入与回滚

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 16 | Approval Grant | 精确绑定、过期、单次使用语义 | 任一绑定项变化，旧 Grant 失效 |
| Day 17 | Dynamic Interlock | 版本、Capability、Evidence、Policy 重查 | 审批后手工改文件，Commit 被拒绝 |
| Day 18 | Git/Hash Checkpoint | Checkpoint Manifest、文件 Hash、恢复点 | 任何真实写入前已有持久化 Checkpoint |
| Day 19 | Workspace Commit Adapter | 只接受 Execution Permit 的写入端口 | 无 Permit 无法调用真实写入 |
| Day 20 | Rollback 与崩溃恢复 | Prepared/Committed 状态机、恢复算法 | 回滚后 Hash 一致；半提交可恢复 |

第 4 周退出条件：

- 批准前真实工作区不可变。
- Grant 只对单项 Operation 有效。
- Checkpoint 一定先于真实 Effect。
- Rollback 不删除历史。

## 第 5 周：Codex-like 桌面工作台

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 21 | Tauri 2 + React/TypeScript 壳 | Desktop App、最小 Capability、协议桥 | 前端不能直接访问文件、Shell、SQLite |
| Day 22 | Task 与 Session 页面 | Intent 输入、Session 状态、运行控制 | 可启动 AUTH-001 并观看状态 |
| Day 23 | Namespace Explorer 与 Plan View | Resource 树、DAG、Context Projection | 切换 Session 显示不同资源树 |
| Day 24 | Proposal Review | Diff、Verifier、Evidence、Gate、Approval | Grade 和 Decision 分开显示 |
| Day 25 | Chronicle、Explain、Rollback | Timeline、责任地图、恢复入口 | UI 能回答责任地图八个问题 |

第 5 周退出条件：

- 用户可以输入任务、观看执行、审查 Diff、批准并回滚。
- UI 是 Projection 和 Command 入口，不是可信控制层。
- 重启后可以从 Event Log 恢复 Session 状态。

## 第 6 周：对抗测试、打包与 Alpha

| 日 | 工作内容 | 交付物 | 当天验收 |
|---|---|---|---|
| Day 26 | Namespace 与授权攻击测试 | Traversal、Symlink、未挂载、旧 Grant 测试 | 所有越权尝试结构化拒绝 |
| Day 27 | Fault Injection | 四类错误注入、d_eff、误报率 MeasurementRecord | 测量值只能由实验导入 |
| Day 28 | 恢复性与确定性测试 | 重启、乱序 Event、半提交、性质测试 | 重启前后 Fold 状态一致 |
| Day 29 | 安装、Quick Start、Demo Script | Alpha 包、示例仓库、演示脚本 | 新环境按文档可复现 Golden Path |
| Day 30 | 完整验收与发布 | 0.1.0-alpha.1、测试报告、演示录屏脚本 | 完成标准和对抗测试全部通过 |

---

# 7. 设计意图如何进入产品

设计原则不能只写在文档中。每条原则必须形成一个“意图落地四联单”：

1. 领域类型或协议；
2. Rust 强制执行点；
3. 用户可见的产品表达；
4. 自动化或对抗测试证据。

| 设计意图 | Kernel / Runtime 强制点 | 产品中的表达 | 验收证据 |
|---|---|---|---|
| 默认可信度为 U | VerificationGrade 默认 U | 未验证步骤显示 U，不显示成功 | 未运行 Verifier 时 Effect 被阻止 |
| Session Namespace 隔离 | 所有解析携带 SessionId | Namespace Explorer 只显示本 Session Resource | 两个 Session 同路径解析不同 Resource |
| Agent 不能直接写文件 | Proposal Adapter 无文件句柄；Commit Adapter 独占写权限 | Agent 只生成 Proposal 和 Diff | Proposal 阶段工作区 Hash 不变 |
| Effect 必须类型化 | 写入必须声明 Operation Schema | Review 显示“将修改什么” | 未声明 Effect 无法通过 Inspector |
| Static Inspector | 执行前检查完整计划 | Plan View 显示检查项和错误 | 缺 Verifier/Capability/Checkpoint 时停止 |
| Dynamic Interlock | Effect 前重新检查全部绑定条件 | 审批后变化时提示授权失效 | 审批后修改目标文件，Commit 被拒绝 |
| 验证不等于自主权 | Verifier 与 Gate 使用独立类型 | Verification 和 Execution 两个面板 | A0 + 无授权仍不能写 |
| 未测不放行 | 风险变量绑定有效 MeasurementRecord | Evidence 面板显示来源和状态 | pending/example/expired 为 DenyUnmeasured |
| Evidence 不可修改 | 只允许追加 Event | Chronicle 显示过期和替代链 | UPDATE/DELETE 被数据库拒绝 |
| Context 是局部投影 | Context 由 Form/State/Event Log 投影 | 可查看“模型实际看到的内容” | 未挂载内容不出现在模型输入 |
| Checkpoint 先于 Effect | Commit 要求 Checkpoint 和 Permit | Approval 卡片展示恢复点 | CheckpointCreated 先于 EffectPrepared |
| 动作可解释 | Operation 绑定 Intent、Principal、Evidence、Policy | Explain 页面输出责任地图 | 自动回答责任地图八个问题 |
| 机制与策略分离 | Namespace/Resource 与 Policy 模块隔离 | UI 显示本次 Policy Version | 修改阈值不修改 Namespace 代码 |

---

# 8. 产品交互表达规范

1. A0/A1/B/C/U 使用中性的验证视觉，不直接使用代表执行许可的绿色。
2. Allow、RequireHuman、Deny 使用独立的执行决策状态。
3. Approval 按钮必须写明具体 Operation，不能只写“信任 Agent”。
4. 拒绝必须展示结构化原因，不能只显示“执行失败”。
5. Approval 页面必须展示 Plan Hash、Proposal Hash、Resource Version 和 Effect Scope。
6. DenyUnmeasured 页面不得提供“仍然执行”入口。
7. Event Timeline 不允许编辑或删除历史。
8. Rollback 是一个新的可审计 Operation，而不是抹除历史。
9. 用户必须能查看当前 Session 中模型实际收到的 Context Projection。
10. Checkpoint、Verifier、Evidence 和 Authorization 必须在真实写入前可见。

---

# 9. M0 验收标准

## 9.1 核心闭环

- [ ] 两个 Session 拥有不同 Namespace。
- [ ] 未挂载 Resource 不可见、不可访问。
- [ ] Agent/Model Adapter 没有真实写能力。
- [ ] 所有真实 Effect 都是类型化 Operation。
- [ ] Form 能稳定编译为规范化 DAG。
- [ ] 相同 Form 产生相同 Plan Hash。
- [ ] Static Inspector 能拒绝非法计划。
- [ ] Proposal 和验证阶段不改变真实工作区。
- [ ] Verifier 输出原子断言和 Grade。
- [ ] 未覆盖断言保持 U。
- [ ] Grade 与 Autonomy Decision 严格分离。
- [ ] Pending、Example、Expired Evidence 不能进入 Gate。
- [ ] Proof Status 与 Assumption Status 分开检查。
- [ ] 缺风险 Evidence 返回 DenyUnmeasured。
- [ ] DenyUnmeasured 不能人工覆盖。
- [ ] Approval Grant 精确绑定且不可复用。
- [ ] Dynamic Interlock 在 Effect 前重新检查。
- [ ] CheckpointCreated 先于任何真实写入。
- [ ] Workspace Commit Adapter 只接受 Execution Permit。
- [ ] Rollback 后逐文件 Hash 与提交前一致。
- [ ] Event Log 严格 append-only。
- [ ] Current State 可以通过 Fold 重建。
- [ ] 系统能够输出逐动作 Responsibility Map。

## 9.2 对抗验收

- [ ] ../、绝对路径和 Symlink Escape 无法越过 Mount。
- [ ] 未挂载 Resource 和缺 Capability 均 fail closed。
- [ ] 审查后手工修改目标文件，原授权失效。
- [ ] 修改 Plan、Proposal 或 Resource Version 后，旧 Grant 失效。
- [ ] Grant 过期或二次使用被拒绝。
- [ ] Pending/Example/Expired Evidence 即使人工批准也不能执行。
- [ ] Proof 合格但 Assumption 为 Pending 时仍拒绝。
- [ ] 移除 Verifier、Compensation 或 Checkpoint 声明时 Inspector 失败。
- [ ] EffectPrepared 后强制终止，重启后没有无法解释的半提交状态。
- [ ] MeasurementRecord 和 Event 没有 Update/Delete 路径。

---

# 10. M0 明确不做

- 不实现开放式通用 Chat 和长记忆。
- 不实现自然语言到 Form/DAG 的高可靠编译器。
- 不接真实通用 LLM；首切片使用确定性 Fixture 或 Mock Adapter。
- 不暴露通用 Terminal、Shell 和任意命令执行。
- 不支持任意仓库；首切片只支持内置 AUTH-001 Fixture。
- 不处理 Dirty Tree、Submodule、LFS、二进制 Patch 和复杂合并冲突。
- 不自动创建 Git Commit；M0 的 Commit 指将已验证 Overlay 提升到真实工作区。
- 不允许真实工作区自主写入。
- 不做多 Agent、并行 DAG 和后台长任务。
- 不做远程 Resource、跨设备、支付、生产部署和生产数据删除。
- 不做复杂概率推断平台或自动估计期望损失。
- 不实现完整 A0/A1/B/C/U 能力；未实现等级保持 U。
- 不同时建设完整 CLI 和完整桌面产品；CLI 仅用于测试和诊断。
- 不做插件市场、安装生态和多模型供应商支持。

---

# 11. 相对 v0.1 的关键调整

1. **从横向建设改为纵向切片优先**：第 2 周得到 Dry Run，第 4 周跑通受控真实写入。
2. **先合并 Kernel，后按边界拆 crate**：避免单人项目过早产生十几个空包和依赖管理成本。
3. **明确 Proposal-only 模型边界**：任何模型或外部 CLI 都不得持有真实写权限。
4. **明确 DenyUnmeasured 不可人工覆盖**：人工授权只处理有效 Evidence 下的 RequireHuman。
5. **真实写入采用两阶段控制**：Grant、Interlock、Checkpoint、Prepared、Commit 顺序固定。
6. **M0 关闭真实工作区自主执行**：自主 Allow 只可能用于受限 Scratch。
7. **桌面工作台是唯一主用户流程**：CLI 只承担诊断、测试和恢复入口。
8. **采用 clean-room 开发**：Codex-like 只描述体验目标，不复制其他产品源码或内部架构。

---

# 12. 风险与缓解措施

| 风险 | 影响 | 缓解措施 |
|---|---|---|
| 单人六周范围过大 | 桌面 UI 或控制闭环延期 | 优先闭环，延后真实 LLM、任意仓库和视觉完善 |
| Rust/Tauri 学习和打包成本 | Day 21 后进度受阻 | 前四周保持纯 Rust；提前检查平台构建依赖 |
| 模型或外部 CLI 绕过内核 | 破坏最核心安全目标 | Proposal-only Adapter；不授予文件、Shell、宿主路径写能力 |
| Event 与文件写入非原子 | 崩溃后产生半提交 | Prepared/Committed Event、Hash Manifest、启动恢复流程 |
| Evidence 缺乏真实校准 | Gate 无法合法放行 | 使用固定 Fixture 和可重复 Fault Injection，限制适用域 |
| 类型和 crate 过度设计 | 首周没有可演示成果 | 单 Kernel crate 起步，以测试证明边界后再拆分 |
| UI 混淆 Grade 与 Decision | 用户误把验证当授权 | 独立面板、独立状态、独立视觉语义 |
| Clean-room 边界不清 | 法律与维护风险 | 新仓库、新 Git 历史、不复制非公开源码，只依赖公开协议 |

---

# 13. 完成定义与汇报机制

## 13.1 Feature Definition of Done

任何功能只有同时满足以下条件才算完成：

- 有版本化领域类型或协议；
- 有 Kernel/Runtime 强制执行点；
- 有用户可见的 Projection 或结构化 CLI 输出；
- 有正常路径测试；
- 有至少一个对抗或失败路径测试；
- 有 Event/Evidence 可以证明结果；
- 有必要的文档和 ADR；
- fmt、clippy、test 全部通过。

## 13.2 每日汇报格式

每天结束时按四项汇报：

1. **完成**：落地的功能、文件和接口。
2. **验证**：执行的测试、测试结果和可复现实验。
3. **设计意图**：今天把哪条原则转化成了类型、执行点、UI 或测试。
4. **风险与下一步**：仍未证明的内容、阻塞项和下一工作日目标。

## 13.3 每周 Gate

每周结束只汇报：

- 本周 Exit Criteria 是否全部通过；
- Golden Path 已运行到哪一个节点；
- 是否出现新的绕过路径；
- 哪些 Evidence 仍为 Pending；
- 是否允许进入下一周。

若退出条件未满足，不用新增 UI 或横向功能掩盖控制闭环缺口。

---

# 14. M0 后续阶段

M1 才开始把工作台从固定 Fixture 扩展为更接近通用 Codex-like 产品：

- 真实模型 Adapter，但仍保持 Proposal-only；
- 任意干净 Git 仓库；
- 更完整的 Session Chat 与 Context 管理；
- 通用 Form 模板；
- 多种 Verifier；
- MCP/Skill Resource Mount；
- 受控 Shell Operation；
- 多 Session 与后台任务；
- Remote Resource Mount 与 Capability Delegation。

M0 必须先证明控制链成立，M1 才扩展能力面。

---

# 15. 立即执行的第一批工作

1. 在独立的 Constellation 目录初始化全新 Git 仓库。
2. 添加 README、constitution 和两份首批 ADR。
3. 创建 Rust Workspace。
4. 创建 constellation-kernel 和 constellation-cli。
5. 固定 Verification、Evidence、Autonomy 和 ID 类型。
6. 添加默认 U、Permit 不可伪造、非法风险值拒绝等首批测试。
7. 配置 fmt、clippy、test CI。
8. 以以下提交结束 Day 1：

~~~text
chore: bootstrap Constellation clean-room kernel
~~~
