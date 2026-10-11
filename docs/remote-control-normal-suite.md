# 正常远控测试批次（2026-10-11）

用户要求继续所有测试；保留“不做恶意测试”和隐藏运行的决定。只用唯一隔离
目录、合成身份与 loopback provider。此清单不是已通过证据。

## 编码前失败方式

- 旧入口继承上级 Job：独立 preflight 失败立即停止，不运行业务进程。
- 批次运行中源码/二进制/页面/驱动改变：失败，不混用不同产物的结果。
- 某轮清理不完整或期限到达：停止后续轮，不扫描、不补认领、不扩大终止范围。
- 普通业务失败但独立清理完整：保留失败轮，允许继续其它隔离情景。
- 忙任务归档只改标记、未停止实际任务，或等待项恢复后被执行：失败。
  实际 interrupt/终态必须来自合成 provider；归档响应不能冒充停止确认。
- 已启动任务删除只从页面消失：必须由隔离 daemon 回查任务集合，并观察真实
  provider 删除请求。取消删除确认不能改变集合；其它任务 ID 不变。
- 模型目录只显示空列表或超时：不能计为目录验证；只显示目录不等于选项应用。
- 发布更新没有真实 waiting worker、依靠测试全局函数模拟：不能计为更新验收。
  输入须在实际更新等待期间保留；只有用户点击 Reload 后才更换控制 worker。
- 测试工具和安装版服务共用配置、固定端口或浏览器 profile：拒绝。
- 缺失情景、被跳过情景与只有编译的情景不得累计为行为通过。
- 失败子进程只有通用 Child failed 时，不猜测源码缺陷。追加固定错误类别与
  退出码，不保存原始输出。浏览器连接失败应记录为浏览器启动阶段，不能沿用
  已通过的 daemon seed 阶段；复测使用新目录，原失败轮保留。
- 后续历史情景：先用手机完成三轮，再从首轮回复分支；分支只能保留第一轮，
  源任务三轮不变。回退源任务第二轮用户消息后只留第一轮，任务 ID 不变，
  分支不变。原生服务须实际复制/删除消息，刷新保持一致，不重发 prompt。
  路由与请求字段依据 OpenCode V2 当前 API；合成服务不代表真实 OpenCode
  文件快照恢复，此项仍单独保留未验收。不能让通用 200 空响应冒充历史语义。
  原生历史读取会生成临时 turn UUID；不能直接作为手机历史目标。须按真实
  provider 输入 ID 绑定本机持久轮次；无匹配或匹配不唯一时不提供动作，不能
  按消息位置或文本猜测。首次真实三轮情景已观察到旧目标被 host 正确拒绝。
  远程 daemon 还须保存实际 TurnStarted 和已校验的 native window；不能只保存
  提交回执和终态，否则已完成轮仍会被当作从未启动。旧 runtime 事件不能写入
  新 runtime；续跑只能替换本次续跑后缀，未匹配原生输入不得覆盖历史。
  列表必须使用本机 display_title（包含分支自动标题），不能只用 raw title，
  否则分支与源任务同名而无法明确选择；Inspector 同样读取正式显示标题。
  分支的 native_session_id、provider_cursor 都必须指向新原生会话，不能克隆源
  native_session_id；事件游标同样不继承源 runtime。否则读取仍返回源会话。
- 后续关系矩阵：手机 B 初始仅明确授权电脑 1；电脑 2 不能隐式出现。经独立
  校验码和本机确认后 B 才授权电脑 2；两枚设备各自切换两台电脑，读取独特
  合成内容，授权和身份不覆盖。撤销 A→1 后 B→1/A→2/B→2 仍有效；随后
  撤销 B→2 后 A→2 与 B→1 仍有效。全程不伪造身份或发送越权报文。
- WASM 正常测量：使用页面实际加载的冻结 bridge 与独立原生 Noise peer，核对
  XX 校验码、KK 指纹及双向字节。Noise 单帧上限不能容纳 1 MiB；以 32 个
  合法分片累计 1 MiB，重复三轮。加密/解密调用单独计时，不把浏览器驱动 IPC
  计入密码计算。缺失分片、字节不同、桥/源码漂移、未核验退出均失败。只输出
  时长、字节计数和哈希，不落私钥/密文/正文；桌面 Chromium 测量不冒充实机
  手机或非调试生产性能。旧攻击/慢连接配额压力测试继续不运行。
- 选项应用：不点 Apply 不改变会话；空选择表示保持原值，不假称当前配置。
  只提交 host 实际目录中的模型、agent 及该模型的变体。不得提交权限模式。
  忙任务不换选项，重复点击不得提交两次；回执不等于 provider 已采用。独立
  daemon 读回配置，并在下一次实际 prompt 核对 provider 的模型与 agent；刷新
  不重发。若 live driver 不能原位更新，空闲时退役该 runtime，下一轮使用已
  保存选项；不得保留旧 driver 又声称新配置会生效。agent 目录须来自 V2
  /api/agent，隐藏和 subagent 不作为会话选择；原生 agent 的权限不能替代本机
  权限规则。至少两模型、两可见 agent 的正常切换须有真实请求证据。
  ProviderModel/ProviderAgentPreset 的内部序列化字段使用 snake_case；外部
  ModelOptionEntry 使用 camelCase。目录转换不得静默丢掉变体或默认标志。
  列表投影不含实际权限或变体；应用选项前须 HydrateSession 读取本机详情，
  不得把列表的默认 FullAccess 当作已保存权限，也不得放宽 daemon 权限校验。
  Apply 是用户操作，须在结果列表显示真实保存回执；不能被过滤成仅后台状态。
  空闲 driver 更换后，合成 provider 的新 SSE 只等待后续轮次，旧连接不得继续
  执行时间线，已完成轮不得再次追加原生消息。三次 prompt 必须恰有三次成功
  终态和九条原生 user/assistant/idle 消息；重复 SSE 不能靠放宽计数通过。

## 顺序运行的正常链路

1. 已完成任务归档/恢复与指定草稿删除。
2. 忙任务归档：停止实际 provider、清掉等待项，恢复不重发。
3. 已启动任务删除：先取消确认，再确认删除；其它任务保留。
4. 普通 Stop 与精确队列取消查询。
5. 手机刷新时恢复等待项与审批。
6. host 安全重启、不刷新重连。
7. host 创建句柄 crash、不刷新重连。
8. daemon 单独 crash、host 存活、取消结果跨再次重启。
9. host/daemon 同时 crash、取消结果跨再次重启。
10. 双设备审批竞争及撤销隔离。
11. 双 host 切换、双设备、手机队列刷新与撤销隔离。
12. 空闲存活检测与隐藏/前台恢复。
13. Service Worker 更新等待期间输入保留、显式 Reload。
14. 三轮历史分支与回退。
15. 完整双 host × 双设备关系矩阵。
16. 真实 WASM bridge 与独立原生 peer 的正常测量。
17. 模型、Agent 与变体的显式应用及下一轮实际使用。

每轮重新创建独立目录，启动前检查冻结哈希，结束独立验证整树退出。批次通过
只表示列表中实际执行的情景通过，不升级为 fullPlanVerified/publicChainVerified。

## 待补齐的项目（恢复轮更新）

- 手机选项实际应用：`normal-remote-suite-MY5t8s` 单项通过，独立复核
  `normal-suite-evidence-RZ6d1A` 通过；当前统一输入仍须完整重跑。
- 历史分支/回退：`normal-remote-suite-fA0vx0` 单项通过并复核，当前统一输入仍须重跑。
- 在飞取消、查询失败保留重试、原运行任务未知结算：缺少受控真实故障时序。
- 完整两 host × 两设备关系：单项曾通过，当前统一输入仍须完整重跑。
- 旧 relay/stress、UI 模块运行器：须先移入已验证容纳路径，不运行旧
  direct-spawn、Kill(true)、固定 profile 或原始输出落盘入口。
- 正常 WASM 测量已在受控运行器内单项通过；真实手机与非调试生产性能尚未验证。
- 公网跨域：页面需由用户发布，复用审查后的冻结产物，不在开发测试中部署。
- 恶意 wire、越权/异常报文等恶意测试：按用户决定不运行，保留未验收。

## 静态检查

workspace、现有 tests、remote examples、WASM 目标编译，PWA 类型与协议漂移
检查；全部存命令、退出码、时长和输入哈希。编译现有测试不执行单元测试。

## 旧冻结版本结果（不代表当前代码）

2026-10-11：上述 13 项正常链路在同一输入批次 **13/13 通过**，另有独立
preflight 通过；静态检查 **10/10 通过**。不是全量计划或公网验收。

- 汇总：`C:/Users/Public/Temp/opencode/normal-remote-acceptance-W0w7PV/summary.json`
- 正常批次：`C:/Users/Public/Temp/opencode/normal-remote-suite-oQ0cKY/summary.json`
- 逐轮复核：`C:/Users/Public/Temp/opencode/normal-suite-evidence-wSGA2s/summary.json`
- 静态检查：`C:/Users/Public/Temp/opencode/normal-remote-static-9zsWn1/summary.json`
- 静态复核：`C:/Users/Public/Temp/opencode/normal-static-evidence-jP4v3c/summary.json`

首次批次 `normal-remote-suite-Vp1s7p` 有两项启动/前置失败，原记录保留；两项
独立复测 `normal-remote-suite-7q4iiX` 通过后，再完整执行本轮。没有自动重试
业务步骤、延长超时或放宽判据。首次瞬态失败根因尚未完全确认。

只读重复复核（不启动服务）：

```sh
node C:/Users/Public/Temp/opencode/verify-normal-static-evidence-20261011.mjs C:/Users/Public/Temp/opencode/normal-remote-static-9zsWn1/summary.json
node C:/Users/Public/Temp/opencode/verify-normal-suite-evidence-20261011.mjs C:/Users/Public/Temp/opencode/normal-remote-suite-oQ0cKY/summary.json
```

以上命令同时核对当前输入；后续源码变化后，不得再声称旧产物代表当前版本。

## 当前统一批次结果（2026-10-11，最新输入）

- 完整定义的 **17/17 正常业务场景通过**，另有独立 preflight 通过：
  `C:/Users/Public/Temp/opencode/normal-remote-suite-fyeRfz/summary.json`。
  `batchPlanVerified=true`、`omittedScenarios=[]`、`inputsUnchanged=true`，
  业务场景无失败或跳过项；恶意测试仍为禁用状态。
- 独立只读证据复核通过：
  `C:/Users/Public/Temp/opencode/normal-suite-evidence-rIyc78/summary.json`。
  `evidenceVerified=true`、`batchPassed=true`，并确认完整页面资产、运行器、
  每轮输入哈希、独立清理和报告哈希一致。
- 当前源码静态检查 **10/10 通过**：
  `C:/Users/Public/Temp/opencode/normal-remote-static-lJkHjI/summary.json`；
  只读复核为 `C:/Users/Public/Temp/opencode/normal-static-evidence-3aaQHN/summary.json`，
  `passed=true`、3 项证据检查通过。静态检查只编译/检查，不执行单元测试或业务服务。
- 本轮使用新的私有页面 candidate：
  `C:/Users/Public/Temp/opencode/remote-candidate-hcpc7h/summary.json`。
  旧 candidate 未覆盖；每轮均使用当前输入重新执行，不拼接旧轮结果。
- 本轮覆盖：任务管理、忙任务归档、删除、Stop、队列刷新、host/daemon
  重启与崩溃恢复、双设备、双 host、存活检测、Service Worker 更新、历史
  分支/回退、WASM 正常测量、模型/Agent/变体应用。
- 仍未覆盖：真实 OpenCode 工作区快照恢复、在飞取消与查询失败重试、原运行
  任务未知结算、旧压力测试、真实手机性能、公网跨域链路。恶意 wire 与越权
  测试按用户决定不运行。该结果不升级 `fullPlanVerified` 或
  `publicChainVerified`。
上节未具备条件的项目与恶意测试不因这次正常批次通过而解除阻断。

## 恢复轮补充判据（2026-10-11）

- 历史分支须读回独立原生身份：分支一轮、源任务三轮，两个 nativeSessionId
  非空且不同。源任务回退后保留一轮，其原生身份不变；分支身份与一轮历史不变。
- 选项独立复核须读取合成 daemon Inspector 与 provider 请求证据：不点 Apply
  状态不变，模型 B/自定义 Agent/变体持久化并实际使用；模型 A/build 清除变体，
  总计恰好三次 prompt、两次模型应用、三次 Agent 应用，本机权限始终 Ask。
- 原生 snapshot 失败必须保留过滤后的诊断与退出码；失败不生成成功 snapshot，
  不启动业务服务，不用 cargo check 或旧二进制替代。编译环境与业务隔离环境分离。
- 正常批次的精确取消 Inspector 只重复查询真实配对的原 C/P，不更换 host、
  device、grant 或目标；身份负面查询保留为未执行，不将其计入正常通过数。
  误用旧负面 helper、缺少 normalOnly 标记或不足两次正确查询均失败。
