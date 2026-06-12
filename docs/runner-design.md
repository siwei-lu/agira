# Runner 设计

> 状态:设计已收敛,**尚未实现**。本文是实现蓝本。
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
│      · codex phase  → Bash 起一次性 `codex exec`             │
│  - 经 `agira task todo --artifact` / `task fail` 推进状态机  │
│  - 自己不动手干 phase 的活(保持 thin)                      │
└─────────────────────────────────────────────────────────────┘
```

职责切分:

- **agira(Rust)拥有 runner 的生命周期、身份、租约、prompt 渲染**;不拥有编排循环。
- **tmux 里的 Claude 拥有编排循环**;但保持**薄**——只做调度与分发,把每个 phase 的实际工作甩给 sub-agent 或 `codex exec`。
- **codex 是编排者手里的 per-phase 工具**,不是独立 runner。不存在驱动 codex 的独立 Rust supervisor daemon。

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
- codex phase:Bash 起一次性 `codex exec`,wait 退出码。
- 子任务完成后,由执行者(sub-agent / codex 命令内部)经 CLI `agira task todo --artifact` 推进 phase;非 0 退出走 escalating retry / `on_retry_exhausted`。

好处:顶层 context 不膨胀、不跨 phase 污染、订阅 token 花在实际工作而非编排者自身的长上下文上。

## 7. orchestrator prompt 归属

编排者是 Claude,所以编排 prompt 必须存在,但从松散文件升级为「内置模板 + config 渲染、启动时注入」:

- **静态部分**(idle-wait 协议、怎么调 agira CLI、带 `runner-id` claim、advance with artifact、codex phase 走 Bash one-shot 的约定)→ 作为 **claude-tmux runner 定义里的内置模板**,编进 agira 二进制。
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

可选配置(`config.json` 内新增段或同级 `runner.toml`):

```toml
[runner]
type = "claude-tmux"
auto_start = true     # task_added 时先内部 ensure-runner(幂等),再派发用户通知类 hooks
lease_ttl = "5m"
```

`auto_start = true` 时,`task_added` 事件在派发用户 hooks **之前**先内部 ensure-runner(幂等:检查 tmux session 名 / lease / 心跳)。这取代旧的外部 executor skill 职责,并把"避免重复启动"从 prompt 工程下沉为 Rust 锁逻辑。

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

1. **`kill-session` 自动拆子树**:Claude 在 pane 里起的 codex / sub-agent 子进程属于该 pane 的进程树,`kill-session` 一并收掉,不留孤儿。
2. **stale-session 检测**:`runner start` / `status` 时,若 session 存在但其中 Claude 进程已死,判定为僵会话 → 杀掉重建。
3. **lease / heartbeat 兜底**:编排者持 lease 并定期心跳;心跳 stale → 下次 `runner start` 接管(杀僵会话 + 释放 lease,任务回到可认领)。这层保证"崩溃后任务卡死"不可能发生,且不依赖任何清理代码成功执行。

> 真正要写的核心:tmux 生命周期封装 + stale-session/lease 回收。无独立自研 daemon。

## 10. phase → 后端路由

沿用 `config.json` 现有的 per-phase `model` 字段作为后端选择器,由编排者解释:

- `opus` / `sonnet` / `haiku` → 后台 delegate 给 Claude sub-agent(订阅计费)。
- `dispatch exec -a codex`(或等价标记)→ Bash 起一次性 `codex exec`。

编排者把 `agira task todo` 渲染出的 prompt 落到 `$AGIRA_PROMPT_FILE` 临时文件,后端命令引用文件而非塞超长字符串。环境契约沿用现有 `AGIRA_TASK_*` / `AGIRA_PROJECT_*` / `AGIRA_*_PHASE`,新增 `AGIRA_RUNNER_ID`、`AGIRA_PROMPT_FILE`。

## 11. 暂不实现 / 砍掉

- **多 runner / pool**:schema 预留,不实现。
- **独立 codex-only command-mode runner**:仅纯 codex、不开 Claude 会话的项目才需要;当前混合 workflow 下 codex 由 Claude 编排者驱动,故砍掉或留 v2。
- **自定义 runner 协议**(声明式 TOML:`mode` / `start` / `check` / `stop` / `run` + `AGIRA_*` env 契约;可执行文件契约 `agira-runner-<name>`)→ 留 v2;v1 只内置 `claude-tmux`。
- **迁移/向后兼容**:单用户,无需考虑。旧的松散 `orchestrator-prompt.md` 直接删除。

## 12. 实现顺序建议

1. runner 注册表 + lease/heartbeat 数据结构与原子读写(复用 `tasks.rs` 的 atomic rename)。
2. `task todo --runner` 认领逻辑 = lease 取代 `task lock`。
3. tmux 生命周期封装:`runner start/stop/status/attach/logs`。
4. 内置 claude-tmux 编排 prompt 模板 + 从 config 渲染 + 启动注入;删除 `orchestrator-prompt.md`。
5. stale-session / lease 回收。
6. `auto_start` 接入 `task_added`。
7. `task list` 顶部显示 runner 状态。

每步遵循 TDD(测试先行,见 CLAUDE.md)。
