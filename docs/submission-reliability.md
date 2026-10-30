# 提交可靠性：第一批改造

## 范围与边界

保留现有 GUI / daemon / 私有 OpenCode 服务分层、全局配置和原生数据库。
第一批只处理普通发送（包括出队后的发送）的持久提交身份、准备阶段竞争、
发送接收回执、脱敏诊断和请求目录一致性。排队/steer、历史编辑回退的完整
事务化、全套执行状态机和自动续跑属于后续阶段。

GUI 可以立即展示输入，但后台保存确认之前不向 OpenCode 发送。提交记录与
本地会话/消息在一个 SQLite 事务中保存；数据库中的原生输入 ID 在发送前
确定。记录接收事实不等于记录执行完成，不以 user 行或 assistant 步骤推断
执行成功。

不启动应用、daemon 或 OpenCode，不发送模型请求，不做视觉测试。当前验收
限于格式、编译、静态检查及只读代码审查；下面的 E2E 场景待另行授权执行。

## 先列出失败方式

1. 本地事务失败或保存回执丢失：不得进入 Git / driver / provider 提交；输入
   必须可恢复。服务器可能已保存本地记录，但这不代表模型接收。
2. 保存期间或 Git 准备期间退出：已确认的输入与稳定 ID 必须仍在数据库中；
   再打开只对账，不自动重发。
3. 旧目录快照与提交交错：不能移除 preparing 会话、覆盖状态或移除所属项目。
4. 旧 runtime attach 回来：不能替换 preparing 会话，不能把它误判为孤儿执行。
5. preparation 结果过期：匹配提交/回合身份，而非只看 Connecting；不得静默
   丢弃输入，也不得把旧结果发送到新回合。
6. preparation / driver 启动失败：记录明确的未发送状态，并保留输入。
7. 重复提交同一个 ID：只允许一次 preparing -> dispatching 的原子认领；
   进程重启或 RPC 重复也不能自动再次 POST。
8. OpenCode 接收成功但 HTTP 回执丢失：记录 unknown，保持原生输入 ID；不
   将未知当拒绝，不自动重发。
9. 接收成功后保存回执失败：先前 dispatching 记录仍表示接收结果未确认；
   不伪造 accepted 或 rejected。
10. 原生输入 ID 冲突：不能将任意 409 解释为安全拒绝后自动重发；需要对账。
11. 跨客户端陈旧 SaveTaskState：不得覆盖仍在准备的提交回合。
12. 同一 Windows 目录大小写/前缀不同：比较键可折叠，请求路径必须统一解析；
    解析不得从 render 路径触发。
13. 诊断包含隐私：日志只能记录时间、固定阶段、UUID/原生 ID、端口和 generation，
    不能记录 prompt、附件内容、URL、认证头或原始错误载荷。

## 后续 E2E 验收产物

在隔离数据目录、固定工作区/模型下，逐一制造保存失败、准备期间退出、
目录同步交错、重复命令、丢失接收回执和 provider 重启。每次保存脱敏提交
时间线、本地提交记录及原生输入 ID 对账结果；证明输入可找回、未重复发送、
状态不误报。仅通过 cargo check 不等于这些行为已验证。

## 实现边界

- `AcceptSubmission` 将提交记录、输入消息与会话一起提交；成功回执之后才
  做工作区/Git 准备。`DispatchSubmission` 原子认领，非 preparing 记录只返回
  现有事实，不重复 POST。
- preparing 的所有权关联到接受它的连接。连接失效后，下一次 hydration /
  dispatch 校验撤销未发送准备；不将永久的内存 UUID 集合当作活跃证明。
- driver 接收回执独立保存。HTTP 409 和不明确的传输失败保留 unknown；当前
  live 对账和冷历史读取都可按原生输入 ID 确认接收，无自动重发。
- 冷历史按原生输入 ID 合并：已接收输入保留本地 turn ID、输入呈现与
  checkpoint，原生缺失的本地输入仍保留。其他客户端的新增历史仍能刷新。
  owned 回合不采用历史转换器默认的 Completed；需要原生输入及 idle 终点
  的对账证据才能结算。仅附非图片文件的 owned 输入在呈现过滤前保留其 ID。
- 冷读使用每个原生会话的 generation 屏障；同一回合的 receipt、结算状态
  和 checkpoint 在写回前从最新投影合并。终态及对应历史窗口一起返回，
  不给旧的部分回答标记 Completed；GUI 也检查冷读期间的投影变化。
- 所有未结算的 owned 回合都参与发送屏障；外部新增回合不能因为排在末尾
  就解除未知/忙碌状态。未结算的冷读不推进“历史已同步”的标记。
- daemon 在转发已经验证的 TurnFinished 前更新其执行投影，避免下一条输入
  被尚未处理的 GUI SaveTaskState 误判为 busy，包括明确拒绝后的失败终点。
  陈旧投影不能重新打开该回合。
- GUI 在 dispatch 确认前保持准备屏障，防止 Cancel 越过后台发送命令。
- 提交状态仅用于内部持久化和对账，不在用户消息、处理中行或回合折叠标签
  中展示保存、发送或接收状态文字；保留原有处理中显示和真实错误处理。
- 新增协议语法将版本升为 10；GUI 与 daemon 必须成对升级。迁移为
  `0004_icy_tusk.sql`，仅新增 submissions 表；补齐既有手写 0003 的生成器
  元数据，未重写已有 SQL 迁移。

仍不做 provider 进程重启后的自动续跑、全库接管、完整执行所有权协调或
旧历史状态机重构。对账找不到输入或查询不可用时继续保持未知；不能仅凭
当前进程没有 active 条目断言其他客户端也没有执行。

## 2026-10-30 会话恢复方案 B/C/D 落地记录

对应 `docs/session-recovery-plan.md` 的批次 B（阻止再次损坏）、C（统一恢复
和旧数据）、D（停止和交互）。线协议无变更（`PROTOCOL_VERSION` 保持 20），
GUI 与 daemon 仍成对升级。

- 类型化传输结果：`crates/fintwind-core/src/opencode_session.rs` 新增
  `SubmitTransportOutcome`、`http_status_from_error`、
  `is_connection_establishment_failure`、`classify_submit_error`。连接建立
  失败判 NotSent 并同步结束本地轮次；409 与不明确错误判 Unknown 进入对账，
  不再把任意 `HTTP 4` 视为拒绝。Prompt / Steer / Goal 共用该分类。
- 失效实例回收：`crates/fintwind-core/src/driver/opencode.rs` 在 POST 前检查
  `is_alive`，死实例直接结算 NotSent；`crates/fintwind-core/src/driver/mod.rs`
  新增 `is_transport_live`。daemon 的 `AttachSession` 与 `DispatchSubmission`
  均校验实例存活并按 generation 移除旧句柄（`crates/fintwind-core/src/daemon.rs`）。
- 派生投影拆分：`crates/fintwind-protocol/src/submission.rs` 新增
  `has_pending_confirmation` 与 `is_session_working`。孤立 Unknown 不再显示
  为 Working，但仍阻止后续 POST；陈旧 Working 投影在 hydrate、冷历史与
  SaveTaskState 合并时纠正，旧快照不能回滚修复结果。
- 停止：无活动轮次但残留忙碌投影时 `cancel_turn` 纠正为 Idle
 （`src/app/sessions.rs`）；队列仍由未结算提交阻塞，不自动出队。

验证产物（隔离环境，固定 fixture 模型，无真实供应商）：

- 批次 A 真实隔离 OpenCode v2.2.1 契约探针 11/11：
  `temp/session-recovery-contract/e9f500ead47341aab85fc78ca2660552/result.json`
  （精确 ID 可查、重复 ID 去重无 409、中断终态为 `outcome=interrupted` +
  `time.idle`、缺失会话 404、SSE 含 inbox 投递事件）。
- 快速故障矩阵 6 场景 + R1 steer + 2 投影守卫通过：
  `temp/recovery-e2e/644b344855c54ce2bab7f31c1890da31/result.json`。
- Long 套件（idle 对账、附件、主动续跑、模板展开）通过：
  `temp/recovery-e2e/*/result-long.json`、`result-p1.json`、
  `result-r2.json`、`result-p2.json`。
- 历史规模矩阵（分页缩减、20 MiB 单消息、90 张截图）通过：
  `temp/protocol-e2e/c06626b61d074651a8f2d3a1d653f07e`。
- 真实隔离 OpenCode 连续三轮 + 陈旧终态否决通过：
  `temp/protocol-e2e/885f6198bf354510a53a17cb22273fb8`。

仍未验证：生产旧库 `Interrupted + Unknown + Working` 的真实重放（须隔离
脱敏副本）、重启恢复端到端、多会话部分失败与查询故障规模组、真实
provider、桌面视觉与键盘、性能帧路径。证据不足的旧记录保持待确认，
不宣称全部自动修复。

### 2026-10-30 验收补充（重启重放、部分失败与查询故障组）

- 重启种子与修复（同一 app.db，先后两个独立 daemon 进程，第二个使用从未
  见过该输入的全新 fake 服务）：`temp/recovery-restart-<uuid>/result-seed.json`
  与 `result-repair.json`。drop-ack 输入落为 Unknown 且仅 POST 一次；待确认
  期间第二次提交被 daemon 门控拒绝且无新增 POST；GUI 式停止保存保留
  Interrupted 收据、队列与输入身份；重放的 Working 旧记录在 hydrate 时纠正
  为 Idle，收据保持 Unknown（不翻为 NotSent），重启与重连后零 POST、无自动
  出队、重试仍被阻止；事后重放的陈旧 Working 快照不能回滚修复。
- 部分恢复与有界查询故障（单共享 serve）：`temp/recovery-e2e/*/result-partial.json`。
  会话 A/C 在会话 B 保持 Unknown 的同时正常结算（同进程单 serve 一次启动）；
  历史 503 的冷核对在预算内失败且收据保持 Unknown、无终态误报；7 个未解决
  提交下目录读取保持响应。
- 仍未验证：生产旧库真实重放（须隔离脱敏副本）、重启恢复端到端（GUI+daemon
  联合）、真实 provider、桌面视觉与键盘、性能帧路径。

### 2026-10-31 移除人工放行，保护自动恢复

- 用户明确不需要“主动放弃待确认”。移除按钮、翻译和客户端强制改写收据及
  自动出队的处理，不以用户承担重复执行风险作为恢复前提。
- `AbandonSubmission` 只保留内部准备清理语义：原子地将 `Preparing` 转为
  `NotSent`。它也是提交 RPC 回执不明时的自动清理入口，因此必须原样返回
  `Dispatching`、`Unknown` 和 `Accepted`，不得将它们伪装成未发送。
- 丢失确认后继续由现有自动历史核对确认精确输入及执行终态；核对完成后
  正常接收下一条显式输入，不需要点击任何修复按钮，不重复 provider POST。
- 黑盒入口：先构建 `opencode_recovery`，再运行
  `python scripts/opencode-protocol-e2e.py --submission-cleanup`。使用独占目录、
  环境白名单和 Windows Job，输出 `result-submission-cleanup.json`。
- 修复前同一黑盒失败：
  `temp/recovery-e2e/a430445a709a4aaa89dc82439561d5a6/result-submission-cleanup.json`，
  在响应屏障内调用自动清理后，Dispatching 被错误改成 NotSent，历史恢复无法确认收据。
- 修复后同一黑盒通过：
  `temp/recovery-e2e/b8d11d142c884d4999f20ae114169bfb/result-submission-cleanup.json`，
  Preparing 清理幂等；Dispatching/Unknown 保留；provider POST 一次；自动完成
  原输入的接收与终态核对；后续显式输入无需人工放行即可被接收。
- 六场景快速恢复矩阵通过：
  `temp/recovery-e2e/da8eba59e1304972bdb3881135824b4a/result.json`。应用和 daemon
  的 `cargo check --locked --offline` 通过；保留现有 `drag_anchor` 未使用警告。
- 本轮不宣称完整方案全部落地。旧记录在原生历史与 inbox 均缺失且没有充分
  接收证据时，仍需完善跨重启自动恢复；不能用人工放行或直接重发掩盖缺口。
