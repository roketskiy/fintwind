# Agent 创建定时任务

Fintwind 在自己的 OpenCode 进程中提供 `fintwind_scheduled_tasks` MCP。需要
OpenCode 2.1.4 或更新版本，使用其仍标为 experimental 的客户端托管 MCP
WebSocket 接口。无需安装插件或修改用户的 MCP 配置。

在「设置 → 通用 → 功能」中开启「定时任务 MCP」。默认关闭；开启后自动建立
实验性 MCP 连接，当前会话无需重启。关闭时移除工具，已有定时任务继续调度。
OpenCode 2.1.4 的这个接口没有额外的 `experimental` 配置开关，Fintwind 直接
管理连接的启停，不修改其他实验性选项。

Agent 通过 Code Mode 调用 `tools.fintwind_scheduled_tasks.create`。调用前，
应向用户确认尚未明确的任务内容、执行位置、运行时间和时区。工具不提供默认
执行位置或默认时间，参数缺失时拒绝创建。

```javascript
return await tools.fintwind_scheduled_tasks.create({
  title: "检查构建状态",
  prompt: "检查此项目的最新构建，报告失败原因。",
  enabled: true,
  target: "currentSession",
  schedule: { kind: "every", intervalSeconds: 3600 },
});
```

所有顶层参数均必填。`target` 可以是 `currentSession` 或 `newSession`。
新会话沿用当前会话的工作区、模型和权限配置，不接受其他项目或会话的 ID。
任务进入现有定时任务模块，可在应用中编辑、暂停、删除或立即运行。

定时规则支持两种形式：

- `every`：`intervalSeconds` 在 60 秒到 365 天之间，首次运行在一个间隔之后。
- `atTime`：例如 `{kind: "atTime", time: "09:15", weekdays: ["monday", "friday"]}`。
  时刻使用 daemon 所在机器的本地时区，工具说明包含当前时区偏移。

`enabled: false` 创建暂停的任务。返回值包含任务 ID、已保存的规则和目标，
以及 `nextDueAt` 和带时区偏移的 `nextRunLocal`；暂停任务的下次运行时间为
`null`。daemon 运行时才会调度任务。创建响应丢失时，不应自动重试，以免重复创建。

每个 OpenCode 进程的每个工作区共享一条 MCP 连接。OpenCode 的 Basic 认证
保护该连接，调用中的 `_meta["ai.opencode/sessionID"]` 用于查找仍然有效的
Fintwind 会话绑定。缺少绑定、运行时已结束或输入校验失败时不保存任务。
连接随最后一个会话绑定释放，断线后按退避间隔重连。旧版 OpenCode 缺少该接口
时记录不可用原因，普通对话仍可继续。

黑盒验证沿用隔离数据目录、环境白名单和 Windows Job Object，运行真实 daemon、
OpenCode 和本地脚本化模型服务：

```powershell
python scripts/scheduled-tasks-e2e.py --mcp-only
```

该验证包含真实模型工具调用、目标和规则保存、变更通知、任务执行及非法输入拒绝，
去掉 `--mcp-only` 可继续运行原有调度器验证。源码协议依据：
[OpenCode 客户端 MCP 实现](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/mcp.ts)
及[服务端接口](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/server/src/handlers/mcp.ts)。
