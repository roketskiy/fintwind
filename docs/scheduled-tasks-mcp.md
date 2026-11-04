# Agent 创建定时任务

Fintwind 在自己的 OpenCode 进程中提供 `fintwind_scheduled_tasks` MCP。需要
OpenCode 2.1.4 或更新版本，使用其仍标为 experimental 的客户端托管 MCP
WebSocket 接口。无需安装插件或修改用户的 MCP 配置。旧版 OpenCode 缺少该接口
时，普通对话仍可继续。

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

定时规则支持三种形式：

- `every`：`intervalSeconds` 在 60 秒到 365 天之间，首次运行在一个间隔之后。
- `atTime`：例如 `{kind: "atTime", time: "09:15", weekdays: ["monday", "friday"]}`。
  时刻使用 daemon 所在机器的本地时区，工具说明包含当前时区偏移。
- `once`：例如 `{kind: "once", at: "2026-11-02 09:15"}` 或 RFC3339 时间。
  在 daemon 本地时区执行一次，运行后自动停用并保留历史记录；启用时必须是未来时间。

`enabled: false` 创建暂停的任务。返回值包含任务 ID、已保存的规则和目标，
以及 `nextDueAt` 和带时区偏移的 `nextRunLocal`；暂停任务的下次运行时间为
`null`。daemon 运行时才会调度任务。创建响应丢失时，不应自动重试，以免重复创建。
