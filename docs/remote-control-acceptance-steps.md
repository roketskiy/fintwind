# 远端控制后端验收步骤（第四批代码交付后执行）

> 2026-10-10 执行更新：本地真实 daemon/合成 provider 已完成创建、幂等受理、
> 精确取消查询、派发、审批/问题回复、成功结算与归档；三次独立 daemon 重启
> 保留队列与取消终态黑盒也已通过。最新冻结证据见 `remote-control-progress.md`。
> 下方“只交付代码”、GLM fixture 只读归属和 parent-pid 推荐为历史记录，
> 已由用户授权主亲自全自动推进及 CREATE_SUSPENDED → 独占 Job → Resume
> 容纳要求取代；parent-pid 不构成整树容纳证明。手机/host 恢复/公网尚未通过。

本批**只交付代码与文档**，未运行任何 daemon / provider / 验收 / 清理，未访问生产 API/DB，
未 kill 任何现存进程。编译证据：`cargo check --locked --workspace` 与
`cargo check --locked --workspace --tests` 均通过（仅有既有测试文件的既存 warning）。

执行前必须先过 AGENTS.md 的测试安全规则（2026-10-09 新增）：

- 测试**不得终止用户或其他会话的进程**；禁止按名称、监听端口、命令行子串或目录匹配杀进程；
  端口冲突另选随机端口或失败退出。
- 清理只允许**本轮创建并持有句柄**的进程；PID、父 PID、名称或路径单独不能证明所有权，
  不得全机扫描补认领孤儿进程。
- 运行前确认独立数据目录、环境白名单、子孙进程容纳边界；无法证明安全则停止并报告。
- 安装版 Fintwind、daemon、remote-host、OpenCode 服务**不得**作为清理目标。

推荐的进程归属做法：daemon 以 `--parent-pid <测试进程 PID>` 启动（daemon 自带父进程监护，
父退出即自行结束）；所有子进程句柄由测试进程持有，结束时只 `kill` 自己创建的句柄。

## 1. 构建

```sh
cargo build --locked -p fintwind-daemon -p fintwind-remote-host
cargo build --locked -p fintwind-remote-host --example start_remote_acceptance
```

## 2. 隔离 daemon 启动契约

- 可执行：`target/debug/fintwind-daemon.exe`
- 参数：`--bind 127.0.0.1:0`（随机端口）、`--data-dir <本轮独立目录>`（绝对路径）
- 环境：`FINTWIND_DAEMON_TOKEN=<本轮随机>`；`PATH` 前置一个**本轮新建**的 shim 目录
  （内含 `opencode.cmd` → `python launch_isolated.py synthetic_opencode.py`），
  使 daemon 的可信 probe（`find_executable("opencode")`）解析到合成 provider；
  另设独立 `HOME`/`USERPROFILE`/`APPDATA`/`LOCALAPPDATA` 指向本轮临时目录。
- 就绪：daemon 在 stdout 打印一行 `DaemonReady{address, protocol_version, pid}` 后开始服务；
  测试用该 address + token 建 `DaemonClient`。

fixture（**只读**，归 GLM 隔离修复，不得修改）：
`scripts/fixtures/remote-control/{synthetic_opencode.py, launch_isolated.py, acceptance.json,
README.md}`。环境变量 `FINTWIND_RC_BEHAVIOR/LOG/SPAWN_LOG`；时间线含
`permission.requested` 与 `form.created(kind=question)` 两个 `await_reply` 阻断点，
回复路由分别是 `POST /api/session/{sid}/permission/{id}/reply` 与
`POST /api/session/{sid}/form/{id}/reply`。

种子数据：`SaveTaskState { projects:[Project{id,name,path=<合成 workspace>}], live_session_ids:[],
sessions:[] }`，随后 `ProbeProvider{binary_override:None, directory:Some(workspace),
discover_models:false, probe_version:true}` 必须回 `installed:true` 且版本串含 `synthetic`
——这是"daemon 的可信 probe 确实用了隔离二进制"的证据。

## 3. 阶段矩阵（每阶段一个布尔/计数，写唯一脱敏 summary）

| # | 阶段 | 通过判据 |
| --- | --- | --- |
| P1 | `CreateRemoteTask{provenance, projectId}` | 回 `RemoteTaskCreated{sessionId}`；`Error` 即失败 |
| P2 | idle 首条 `AdmitRemotePrompt` | 回 `RemotePromptAdmitted`，`operationId` **回显原值**，`state=="queued"` |
| P3 | 同 operationId 重放 | 回 `RemotePromptState` 且 submissionId 与 P2 相同；`ListRemotePrompts` 仍 1 行；**不得**出现第二个 submission |
| P4 | 同 operationId 但异 summary | 被拒（operation id 冲突），不产生新行 |
| P5 | 第二条 `AdmitRemotePrompt`（另一 operationId） | 入队；会话仍 idle，未启动任何 runtime |
| P6 | `DispatchRemotePrompt{P2}` | `state=="dispatched"`；随后 `AttachSession` 得到 runtime_id；订阅事件流应见 `turnStarted` |
| P7 | 交互线 | `permission.requested` → `Respond`；`form.created(kind=question)` → `RespondUserInput`（single + multi + custom）；最后 `turnFinished{success:true}` |
| P8 | busy 普通发送 | P7 turn 运行期间 admit 第三条 → `queued`；事件流中**只有一个** execution；绝不被 steer |
| P9 | turn settle 后队列自动推进 | P8 的 prompt 在会话空闲后被派发并 settle；两轮各自一条 submission |
| P10 | 取消 | 对未派发行 `CancelRemotePrompt` → `cancelled`；daemon 行消失；设备收到 `cancelled` |
| P11 | 过期队首 | 造一个 `expiresAtMs` 已过的队首 → host 取消 daemon 行、上报原设备 `Expired`、**下一项被派发**（FIFO 不堵死） |
| P12 | host 重启恢复 | 重启 host（不动 daemon）→ `ListRemotePrompts` 恢复 queued 行；已 settle 的不恢复、不重跑 |
| P13 | 撤销/重新配对 | 撤销设备 → 该设备队全部 drop 且 **daemon 侧行被取消**；旧 provenance 再派发被拒 |
| P14 | 归档 | 忙任务归档：无运行时可停即拒且不写 `archived`；有运行时则取消全部等待行并归档；恢复只清标志 |
| P15 | 未知结局不重发 | 对已 dispatched 行再 `DispatchRemotePrompt` → 回记录状态，**不**产生第二次 driver 提交 |

P7 的双设备审批唯一 driver 决定、relay QueueFull 外进程、公网/真实 provider 仍未覆盖，
须在 summary 里如实记为未验，不得写入 `passed:true`。

## 4. 产物要求

- 唯一脱敏 summary JSON：阶段名 + 布尔 + 计数；**不含** prompt 正文、选项标签、权限 pattern、
  答案正文、token、路径以外的机密；fixture 的原生请求日志本身已满足该约束（只记别名/计数/请求 id）。
- 记录输入源与二进制的 SHA256（synthetic_opencode.py、launch_isolated.py、acceptance.json、
  fintwind-daemon.exe、fintwind-remote-host.exe）。
- 总期限：整轮一个 deadline（建议 10 分钟），任一步超时即记该阶段失败并带着已写产物退出，
  不得挂起。
- 结束时只 kill 本轮创建且持有句柄的进程；daemon 由 `--parent-pid` 监护自退。
