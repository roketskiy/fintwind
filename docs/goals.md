# OpenCode Goal

Fintwind 使用 OpenCode 自带的 Goal，不再加载 Fintwind 自带的 Goal 插件。设置中没有 Goal 开关，支持该功能的 OpenCode 版本可以直接使用。此适配依据 OpenCode 2.2.1 的 Goal 命令与 RPC 接口；旧版本如果没有官方 Goal，不能使用此功能。

## 使用

- 输入 `/goal <目标>` 开始目标。目标可以附带文件。
- 输入 `/goal pause` 暂停，`/goal resume` 恢复，`/goal clear` 清除。目标卡片也提供这些操作。
- 输入框上方的目标入口显示当前状态；展开后查看目标，并根据状态暂停、恢复或清除。
- 单独输入 `/goal` 查看状态、最近原因及命令用法。回复进行中也可以设置或恢复目标。停止执行会同时暂停活动目标。

目标状态和续跑由 OpenCode 管理，并按会话隔离。OpenCode 会在执行过程中评估目标；达到目标、遇到阻碍或触及其限制时，更新目标状态。Fintwind 只同步并展示官方提供的数据，不提供额外的完成判定、耗时或工具计数。服务重启后，原先活动的目标由 OpenCode 暂停，需要手动恢复。

## 接口

开始、暂停、恢复和清除通过 OpenCode 的 `/api/session/{sessionID}/command` 执行 `goal` 命令。Fintwind 通过 `opencode.goal/get` 读取目标，在收到 `rpc.opencode.goal.changed` 或事件连接恢复时刷新。目标和插件存储属于 OpenCode；本次迁移不修改用户或项目的 OpenCode 配置。

旧版 Fintwind Goal 与官方 Goal 使用不同的存储。已有旧目标不会自动迁移，请在需要继续时重新设置。

## 旧检查脚本

`scripts/goal-opencode-e2e.py` 是已移除的 Fintwind 插件的旧检查脚本，不适用于官方 Goal。其他检查脚本仍导入其中的共享进程与模型辅助代码，因此暂时保留该文件；不要直接运行它来验证当前 Goal。
