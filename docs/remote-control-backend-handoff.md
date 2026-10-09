# 后端阶段交接（2026-10-08；暂停后恢复修订）

用于子代理上下文压缩后恢复工作。保留全部既有 WIP，不提交、不重置。

## 目标与约束

- 用户要求完成 `remote-control-plan.md` 的全量第一阶段，不是 MVP。
- 并发最多 4 个：Step 5 Preview 最多 2、GLM 5.3 Flash 最多 2；阶段边界压缩，
  目标约 300k，避免超过 400k。
- 禁止新增单元测试；整体外进程黑盒与 E2E 保存脱敏可重复产物。
- 不启 watcher、不做未要求的视觉测试，不用用户工作区、身份或真实模型密钥。
- 协议版本保持 2；未知副作用只能查询原 operationId，不能自动重发。
- 恢复时用户明确 Cloudflare 页面自行部署，替代此前 Pages 代发布授权。主代理
  准备冻结验收包；公网验收尚未完成，未因此回退独立来源方案。

## 文件归属

- 后端：remote-host 的 adapter/approvals/dedup/devices/events/main/outbound/pending/queue/relay_session/state；remote-protocol 的 message/frame。
- 后端追加：持久发送与整理所需的 core daemon/persistence/server、protocol
  model/protocol/submission、对应数据库迁移；保留暂停时已写入的部分实现。
- 主代理：workspace_reads/directory/diff、host lib.rs、schema、隔离 daemon 数据目录、部署与构建脚本、seed/probe examples。
- 桌面代理：identity/pair/grants、桌面 daemon/devices/locales、信任黑盒与 provider fixture。
- PWA 代理：PWA src、控制 UI 黑盒和 `isolated-real-chain.mjs`；主代理维护
  生成类型、WASM 构建与发布门禁，不再并行修改真实基础链路脚本。
- relay/WASM 代理：两个对应 crate 与其黑盒。不要越界修改其在途文件。

## 第二批交付事实

后端报告已实现每设备 Noise/序号/缓冲/分片隔离、审批请求 claim、16 项/64KiB 会话队列、
4096 事件窗口与 Resume/Resync、未启动会话更名、逐项删除、可信 probe 路径与选项目录、
只读接入和后台 host-status.json（状态切换及每 20 秒）。

后端报告 native/tests 编译通过，host 测试 23 项通过。其中 19 项是 lib 测试，不计新要求的
整体行为验收；已有 device_loop/pairing_loop/isolated_daemon 只覆盖单设备和隔离进程。

## 仍未完成

以下是第二批缺口，暂停前已经部分实现。恢复时以当前源码和实际外进程验收
核对，不要求将已完成代码改回旧路径，也不把文件存在或构建成功视为已闭合。

1. 发送仍绕开持久 submission，使用主机内存队列和 Command::Prompt；host 重启不能恢复
   队列，未运行会话不能接首条提示词。需把准备与派发放到可信 daemon 公共路径，而不是
   将 GUI 状态或客户端整对象作为远端输入。
2. 归档/恢复显式拒绝，不能算全量计划完成；应补 daemon 所有的持久整理状态及精确操作。
3. 两台 host/两设备、网络故障、撤销、审批竞争尚未实际验收。
4. `adapter.rs` 第 522 行含字面的 `\n` 且整段在注释内，agentPreset 没有实际应用路径；
   选项返回成功不能代替状态/真实 provider 载荷变化。代码行号以后续版本为准。
5. `delete_sessions` 先 RemoveSession 后再 session_coordinates 读已删除记录，provider
   删除坐标会丢失；先冻结可信坐标，并明确 stop、provider 删除和本地删除各项结果。
6. `queue.rs` terminal 项保留正文但不再计入上限，长期发送仍可能增长；需及时清除正文，
   不允许“有界 pending”掩盖无限 terminal 留存。
7. 已有运行权限遵循电脑设置，远端禁止修改权限；新会话需复用真实本机新任务默认路径，
   不能仅把 AgentSession::new 的结构体默认当作实际用户设置。

## 只读验收证据

- `pwsh -NoProfile -File scripts/check-workspace-reads.ps1` 已完成：27 项通过，
  credential-file symlink 情景因 Windows 创建权限跳过。不能标全情景完成。
- 产物：`C:/Users/Public/Temp/opencode/workspace-reads-blackbox-NbdNaX/summary.json`；
  包装构建输入哈希：`C:/Users/Public/Temp/opencode/workspace-read-check-a91fd26f-46da-46b8-9403-890fcd27a7f8/summary.json`。
- 不证明授权/密文/daemon 适配或公网；完整读取模块才是该项范围。

## 当前接口

`sendPrompt {prompt}`；`cancelQueuedPrompt {operationId}`；`createSession {projectId}`；
`deleteSessions {sessionIds}`；`loadOptionsCatalog {}` 后独立 OptionsCatalog；
`applySessionOptions {interactionMode?,model?,agentPreset?,reasoningEffort?,serviceTier?,contextWindow?}`；
`forkFromResponse/rewindToMessage {turnCount}`；`readTaskOutput {}`；
文件/目录/diff 的精确请求与分页返回见对应 workspace_* 类型。

首条消息、队列恢复与整理接入后重新交付精确 payload/返回；生成类型由主代理统一更新。

后续新增接口以 `remote-control-backend-interface.md`（后端待交付）及实际
message/frame/core command 为准；项目名称与稳定 turnId/turnCount 防陈旧确认、
RejectedFrame 结构引用和 QueueFull 多设备恢复仍需明确验收。
