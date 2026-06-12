# Runner 设计

> 状态:runner 已通过 task-133 落地实现;本文现在同时作为已落地部分的设计记录,以及未实现项的实现蓝本。
> 来源:多轮 brainstorm(paseo 会话 `1d2bf1e` 起)。单用户场景,不考虑向后兼容/迁移。

## 1. 背景与目标

当前 executor 靠「外部 skill + `task_added` hook 粘合」运行,问题集中在:

- 幂等去重逻辑散落在 skill 的 prompt 里,不可靠(`outstanding` 任务计数依赖 prompt 工程,容易偏差)。
- 执行者没有状态可见性:`agira task list` 看不到"谁在跑哪个任务、是否还活着"。
- 用户装好 agira 还要再装 skill 才能跑起来。

目标:把执行者收编为 agira 的**一等公民 `agira runner`**——有身份、持租约、可被 `start/stop/status` 管理,把可靠性逻辑从 prompt 工程下沉为 Rust 代码。

## 2. 命名:`runner`

在 `worker` / `executor` / `runner` 三者中选定 **`runner`**,取 GitHub Actions / GitLab Runner 的语义:一个**注册的、有身份、会认领并租用任务**的执行者,后端可插拔。相比 `worker`(匿名队列消费者)它自带 identity + lease 语义,正好用来根治 audit 文档里的对账脆弱点——每次 phase 推进都可归属到某个 `runner-id`,不再需要靠计数猜测。

## 3. 核心约束:订阅计费

**写代码要走 Claude 订阅计划(Pro/Max),而 `claude -p` 无头模式不在订阅覆盖内——只有交互式 Claude Code 会话吃订阅额度。**

这条约束决定了整个架构:

- Claude 侧的工作**必须**跑在交互式会话里 → 用 `tmux` 常驻一个交互式 `claude`,通过 `tmux send-keys` 喂 prompt。
- 因此**编排者只能是这个 Claude 交互式会话本身**,不能是一个对 `claude -p` 起一次性命令的 Rust supervisor。
- 交互式会话里用 Task 工具派生的 background sub-agent 跑在同一份订阅鉴权下,**同样吃订阅**——所以"每 phase 后台 delegate 给子 agent"是订阅友好的,予以保留。

## 4. 架构总览

```
┌─────────────────────────────────────────────────────────────┐
│ agira runner  (Rust)                                         │
│  - 生命周期管理:start/stop/status/attach/logs               │
│  - runner 注册表 + lease/heartbeat                           │
│  - 编排 prompt 渲染(内置模板 + config)                      │
└───────────────┬─────────────────────────────────────────────┘
                │ tmux new-session -d / send-keys / kill-session
                ▼
┌─────────────────────────────────────────────────────────────┐
│ tmux session: 交互式 Claude(订阅计费)= 薄编排者            │
│  - 调 `agira task todo --runner <id>` 认领下一个任务         │
│  - 按 phase 的 model 字段路由后端:                          │
│      · claude phase → 后台 delegate 给 sub-agent             │
│      · command backend → Bash 起一次性 backend 命令          │
│  - 经 `agira task todo --artifact` / `task fail` 推进状态机  │
│  - 自己不动手干 phase 的活(保持 thin)                      │
└─────────────────────────────────────────────────────────────┘
```

职责切分:

- **agira(Rust)拥有 runner 的生命周期、身份、租约、prompt 渲染**;不拥有编排循环。
- **tmux 里的 Claude 拥有编排循环**;但保持**薄**——只做调度与分发,把每个 phase 的实际工作甩给 sub-agent 或 backend column 携带的命令。
- **非 Claude backend 命令是编排者手里的 per-phase 工具**,不是独立 runner。不存在驱动任意具体工具的独立 Rust supervisor daemon。

## 5. runner 概念模型:identity + lease

- runner 启动时注册,获得 `runner-id`(临时注册:`start` 注册,`stop` 注销)。
- 认领任务 = 持有一个 **lease**(带 TTL + 心跳),取代旧的匿名 advisory `task lock`。
- 崩溃 → lease TTL 到期 → 任务自动回到可认领状态,**无需人工 `unlock`**。
- `runner-id` 穿过 `agira task todo --runner <id>`(或 `$AGIRA_RUNNER_ID`),使认领可归属。

### 注册表 `~/.agira/<slug>/runner/runners.json`

```json
{
  "id": "claude-tmux-a1b2c3",
  "type": "claude-tmux",
  "tmux_session": "agira-<slug>",
  "status": "running",
  "current_task": "task-003",
  "lease_expires_at": "2026-06-11T12:34:56Z",
  "last_heartbeat": "2026-06-11T12:34:44Z",
  "registered_at": "2026-06-11T12:00:00Z"
}
```

> v1 单 runner/项目;schema 已为多 runner 预留 `id` 维度,但暂不实现(同 repo 多 runner 会撞 git 工作区,届时需串行或 per-runner worktree)。

### lease 取代 `task lock`

- `task lock` / `task unlock` 的语义并入 runner claim/release,**隐藏进 `task todo` 的认领逻辑**,默认不暴露手动命令。
- `agira task list` 顶部可显示:`task-003 由 runner claude-tmux-a1b2c3 执行,心跳 12s 前`,取代现有的匿名 stale-failure warning。

## 6. 编排者保持 thin(已拍板)

顶层编排者(tmux 里的 Claude)**只做调度,绝不自己动手干 phase 的活**:

- 取下一个 actionable task → 看 phase 的 `model` 字段 → delegate。
- claude phase:后台派生 sub-agent 执行,idle-wait 其完成通知。
- non-Claude backend phase:Bash 起一次性 backend column 携带的命令,wait 退出码。
- 子任务完成后,由执行者(sub-agent / backend 命令内部)经 CLI `agira task todo --artifact` 推进 phase;非 0 退出走 escalating retry / `on_retry_exhausted`。

好处:顶层 context 不膨胀、不跨 phase 污染、订阅 token 花在实际工作而非编排者自身的长上下文上。

## 7. orchestrator prompt 归属

编排者是 Claude,所以编排 prompt 必须存在,但从松散文件升级为「内置模板 + config 渲染、启动时注入」:

- **静态部分**(idle-wait 协议、怎么调 agira CLI、带 `runner-id` claim、advance with artifact、non-Claude backend 走 Bash one-shot 的约定)→ 作为 **claude-tmux runner 定义里的内置模板**,编进 agira 二进制。
- **动态部分**(phase 列表 + 每个 phase 的 `duty` / `model`)→ 从 `config.json` 的 `phases` 渲染。
- `agira runner start --type claude-tmux` 把 [内置模板 + config 渲染的 phase 表] 拼好,在会话启动时注入(`--append-system-prompt` 或等价机制)。
- **删除**松散的 `~/.agira/orchestrator-prompt.md`;需要定制时在 runner 配置里给覆盖路径。

净效果:编排行为单一真相源 = 内置模板 + `config.json`,消除 prompt 与 config 漂移。

## 8. 命令面

```sh
agira runner start [--type claude-tmux|<custom>]  # 创建 tmux session + 注入编排 prompt + 注册
agira runner stop                                 # tmux kill-session + 注销 + 释放 lease
agira runner status                               # 存活检查 + 类型 + 当前任务 + 心跳
agira runner attach                               # tmux attach(仅 session 型)
agira runner logs [-f]                            # tail pipe-pane 落盘的日志
agira runner heartbeat                            # (内部)编排者更新心跳用
```

可选配置(全局 `~/.agira/config.toml`):

```toml
[runner]
type = "claude-tmux"
auto_start = false    # 默认不自启;开启后 task_added 会先内部 ensure-runner(幂等),再派发用户通知类 hooks
lease_ttl = "5m"
orchestrator_template_path = ""   # 可选:覆盖内置编排 prompt 模板
```

### claude 模式配置 `[runner.claude]`(2026-06-13 收敛)

claude-tmux 模式的启动方式不再写死,新增专属配置段:

```toml
[runner.claude]
command = "claude"           # 二进制路径或 wrapper(tmux 非登录 shell 的 PATH 常与终端不一致)
model = "sonnet"             # 编排会话自己的模型;干活模型由 phase 的 model 字段路由,二者解耦
permission_mode = "auto"     # auto | acceptEdits | dontAsk | bypassPermissions | default
settings_path = ""           # 可选:专用 --settings 文件(为无人值守调好的 allowlist),留空继承
extra_args = []              # 逃生舱:透传任意 CLI flag(--add-dir、--mcp-config 等),agira 不校验

[runner.claude.env]          # 注入启动环境(代理、ANTHROPIC_BASE_URL 中转等)
```

各旋钮解决的真实痛点:

- **`permission_mode`** — runner 无人值守,claude 一弹权限确认整个会话就静默卡死。默认 **`auto`**(已拍板):后台分类器对未列入 allowlist 的操作自动裁决,安全放行、危险静默拒绝。已知退化路径:**连续 3 次或累计 20 次拒绝后退回人工确认提示** → runner 卡住。该路径有兜底:runner 是 tmux 交互会话(非 headless `-p`,后者反复拒绝会直接终止),卡住后心跳过期 → lease 释放 → 任务回到可认领(§9 第三层防线),用户随时 `runner attach` 解开。严格替代:`dontAsk`(纯 allowlist、绝不弹提示,代价是自己养 allowlist)、`bypassPermissions`(仅限隔离环境)。注意 `acceptEdits` 并不适合无人值守——它只放行文件编辑和常见文件系统命令,未列出的 shell 命令仍会弹提示。
- **`command`** — tmux 起的是非登录 shell,`claude` 找不到是这类工具最常见的开箱即坏;顺带支持版本锁定与 wrapper。
- **`model`** — 编排者是 thin 的(§6),长驻会话烧 opus 不划算,sonnet 足够。
- **`settings_path`** — 与 `permission_mode` 互补:allowlist 配得越全,分类器要裁决的越少,触发"反复拒绝退回提示"的概率越低。也把 runner 设置与用户交互会话的设置隔离。
- **`env` / `extra_args`** — 长尾需求的逃生舱,避免每个 flag 都结构化成旋钮。

连带想法(记录,不阻塞实现):`runner status` 可复用 pane 内容检测基建(`pane_is_claude` / TUI 就绪检查)识别权限确认提示,输出 `blocked on permission prompt, attach to resolve`,把"静默卡住"变成可观测状态。

`auto_start` 默认保持 `false`(2026-06-13 收敛,task-135):可发现性由 `task add` 在没有存活 runner 时向 stderr 打印提示承担,而不是默认自启。`auto_start = true` 是显式开启后的行为:此时 `task_added` 事件在派发用户 hooks **之前**先内部 ensure-runner。这取代旧的外部 executor skill 职责,并把"避免重复启动"从 prompt 工程下沉为 Rust 锁逻辑。

ensure-runner 的语义比"未注册才启动"更宽(2026-06-13 收敛,task-133 / task-135):它是幂等的"确保有一个活跃且被唤醒的 runner"入口。若 runner 不存在或 pane 已僵死,ensure 会重建;若 runner 空闲但仍存活,ensure 仍会重新 kick 一个 kickoff,让新任务能唤醒编排者。

递归防护(2026-06-13 收敛,task-134):当环境变量 `AGIRA_RUNNER_ID` 已设置时跳过 ensure。这样编排者自己在 pane 内 `task add` 时,不会向自己的 pane 注入 kickoff。

> runner 是独立的一等机制,**不是** `task_added` hook 的语法糖——`task_added` hook 保留给通知类集成(如 Telegram),避免用户删 hook 时连带删掉 runner。

## 9. daemon 化与孤儿清理

**tmux server 本身就是 daemon**(会话脱离终端常驻),无需 Rust double-fork/setsid。"存活令牌"是 **tmux session 名**,`tmux has-session` 即 check。

命令到 tmux 动词的映射:

| 命令 | tmux 操作 |
|------|-----------|
| `runner start` | `tmux new-session -d -s agira-<slug>` + 注入 prompt + pipe-pane 落日志 |
| `runner stop` | `tmux kill-session -t agira-<slug>` |
| `runner status` | `tmux has-session` + 读 runners.json |
| `runner attach` | `tmux attach -t agira-<slug>` |
| `runner logs -f` | tail pipe-pane 日志文件 |

孤儿清理三层防线:

1. **`kill-session` 自动拆子树**:Claude 在 pane 里起的 backend 命令 / sub-agent 子进程属于该 pane 的进程树,`kill-session` 一并收掉,不留孤儿。
2. **stale-session 检测**:`runner start` / `status` 时,若 session 存在但其中 Claude 进程已死,判定为僵会话 → 杀掉重建。
3. **lease / heartbeat 兜底**:编排者持 lease 并定期心跳;心跳 stale → 下次 `runner start` 接管(杀僵会话 + 释放 lease,任务回到可认领)。这层保证"崩溃后任务卡死"不可能发生,且不依赖任何清理代码成功执行。

> 真正要写的核心:tmux 生命周期封装 + stale-session/lease 回收。无独立自研 daemon。

## 10. phase → 后端路由

沿用 `config.json` 现有的 per-phase `model` 字段作为后端选择器,由编排者解释:

- `opus` / `sonnet` / `haiku` → 后台 delegate 给 Claude sub-agent(订阅计费)。
- 任何其他值 → 视为 phase 的 `model` 字段携带的 shell command;编排者不内置任何具体工具知识,只把 backend column 里的命令作为 Bash one-shot 运行。

编排者把 `agira task todo` 渲染出的 prompt 落到 `$AGIRA_PROMPT_FILE` 临时文件,backend column 里的命令引用文件而非塞超长字符串。环境契约沿用现有 `AGIRA_TASK_*` / `AGIRA_PROJECT_*` / `AGIRA_*_PHASE`,新增 `AGIRA_RUNNER_ID`、`AGIRA_PROMPT_FILE`。

## 11. 暂不实现 / 砍掉

- **多 runner / pool**:schema 预留,不实现。
- **独立 command-mode runner**:仅纯命令后端、不开 Claude 会话的项目才需要;当前混合 workflow 下非 Claude 命令由 Claude 编排者驱动,故砍掉或留 v2。
- **自定义 runner 协议**(声明式 TOML:`mode` / `start` / `check` / `stop` / `run` + `AGIRA_*` env 契约;可执行文件契约 `agira-runner-<name>`)→ 留 v2;v1 只内置 `claude-tmux`。
- **迁移/向后兼容**:单用户,无需考虑。旧的松散 `orchestrator-prompt.md` 直接删除。
- **时序/健康度旋钮**(`ready_timeout`、`heartbeat_staleness`、僵会话重建时 `resume = fresh|continue`)→ **hold**:常量先留在代码里(TUI 就绪 60×500ms、心跳过期 10m),有人撞到再暴露。
- **单一 `launch_command` 模板字符串**(占位符替换)→ 砍掉:灵活性最高,但 agira 从此无法对启动方式做推理(`pane_is_claude` 检测、system prompt 注入方式),结构化旋钮 + `extra_args` 逃生舱是更好的平衡。
- **会话回收策略**(`recycle_after` 防长驻上下文腐烂)→ 砍掉:过早优化,claude 自带 auto-compact,观察到编排质量随会话寿命衰减再说。
- **`[runner.claude]` 每项目覆盖** → 暂缓:v1 保持全局 config.toml;真出现"不同项目要不同 permission_mode"再加 override 层。段名选 `[runner.claude]` 而非 `[runner.types.claude-tmux]`(v1 单 type,前者更顺手)。
- **`task unblock` / `retry` re-open 触发 ensure** → 未实现(2026-06-13 收敛,task-135):未来这些重新打开任务的路径应复用 §8 的同一个幂等 ensure-runner 入口。

## 12. 实现顺序建议

1. runner 注册表 + lease/heartbeat 数据结构与原子读写(复用 `tasks.rs` 的 atomic rename)。
2. `task todo --runner` 认领逻辑 = lease 取代 `task lock`。
3. tmux 生命周期封装:`runner start/stop/status/attach/logs`。
4. 内置 claude-tmux 编排 prompt 模板 + 从 config 渲染 + 启动注入;删除 `orchestrator-prompt.md`。
5. stale-session / lease 回收。
6. `auto_start` 接入 `task_added`。
7. `task list` 顶部显示 runner 状态。

每步遵循 TDD(测试先行,见 CLAUDE.md)。
