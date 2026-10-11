# 浏览器进程句柄验收方案

状态：句柄模块及合成双进程黑盒已实现；正常路径重复两次通过。
后续独立 observer + 调试树正常/非零/超时/调试器崩溃已通过；测试版
Chromium 空白页通过。旧 direct-spawn 链路入口仍阻断。

## 不改变的边界

- 不按 PID 调用 `OpenProcess`，不扫描或认领已有进程。
- 不添加外层 Job、命名 Job、breakaway 或自动批准配对开关。
- 不使用 `--no-sandbox`、`--single-process` 改造浏览器来减少待验收后代。
- supervisor 仍是唯一 Job 所有者，根进程仍先 suspended、按创建句柄
  AssignJob、再 Resume。环境、profile、身份和数据目录必须独立。
- 真实链路仍要求独立 observer 持有真实后代等待句柄；模块编译成功不解除门禁。

## 编码前失败清单

1. 创建调试树的线程与事件处理线程不同：拒绝；Windows 只允许创建线程等待事件。
2. CREATE_PROCESS 事件没有进程句柄、句柄 PID 不符或不属于确切的本轮 Job：失败。
3. 复制句柄失败、独立 observer 拒绝交接：不继续该事件，终止本轮 Job。
4. 将调试器托管的句柄主动关闭，再继续 EXIT 事件造成二次关闭：禁止。
   仅自有复制句柄由 RAII 关闭；image/DLL 文件句柄则及时关闭。
5. 吞掉业务异常：禁止。仅每个进程的首次系统启动断点返回 DBG_CONTINUE；
   其他异常返回 DBG_EXCEPTION_NOT_HANDLED，不读取异常正文或进程内存。
6. 调试等待无限阻塞、事件过载或首次/退出事件序列异常：失败，保留不完整证据。
7. Chrome 自建新调试树或 sandbox 后代不产生可交接事件：冻结拓扑与 Job
   名单不匹配则失败；不能用 PID 重新打开补齐，也不能关闭 sandbox 后重试。
8. root 退出但后代仍运行：不能冻结；必须继续事件处理，或进入期限/Job 清理。
9. 清理时停止事件泵，导致调试暂停的进程永远不能退出：清理期也要处理事件，
   按复制句柄等待，并核验 Job 为零；无法收尾报告失败。
10. 多个生命周期复用相同 PID：句柄是身份，活动 PID 只用于路由当前调试事件；
    已退出的复制句柄仍保留用于最终等待，不能以 PID 覆盖旧句柄。
11. 调试改变时序：这轮只证明受控浏览器行为，不宣称非调试生产性能通过。
12. 合成 fixture/Node 树未先通过正常、失败、超时和 debugger crash：不运行 Chrome。

## 最小接线

创建句柄崩溃注入编码前失败清单：不得按 PID、名称、端口或路径打开进程；
仅允许测试包装器自己 `Command::spawn` 返回的 `Child`，使用其 Windows
创建句柄 `Child::kill`。包装器必须已在受控 Job 内，目标仅为带独立数据目录
标记的冻结 host/daemon；无包装器逃逸、无产品故障开关。stdin 一次性随机
nonce 不匹配、未知命令、目标已退出须失败关闭，不扩大清理范围。
确认必须在 wait 后输出实际非零退出码；包装器正常退出不等于目标正常退出。
整个链路仍由独立 observer 核验后代创建句柄、精确 Job 和最终退出。
host crash 后 daemon/provider 必须存活；恢复须新 Noise 周期、原队列身份、
原审批与 prompt 计数不增。daemon crash 单独验收，不以 host crash 代替。
双方 crash 的本轮边界仅为：恢复原 queued 项并取消，失去旧 runtime 的
审批不能沿用；不得重新发 prompt 或制造原任务成功终态。原任务未知结算
和精确 C 查询失败重试仍要专门验收，不能因这个队列场景通过就关闭门禁。

端到端存活检测编码前失败清单：relay ping/ack 不得证明 host 在线；明文
控制消息、其它设备/host、旧 Noise 周期与不匹配 probe 不得延长期限。
同一通道只留一个 probe，回复不访问 daemon/文件系统，也不读取整份快照。
期限到达只关闭该手机通道并重建 Noise，不重放操作密文、不重复提交提示。
关闭页面/通道须收掉计时器；后台页不持续发送，恢复前台须核验通道。
验收须先观察旧 host 退出后控件 disabled，再启动新 host，不刷新页面，
恢复 queue/offer 并精确取消，provider prompt 计数不增。该检测只证明 host
传输循环响应，不证明 daemon 或任务执行成功。

实施前补充收尾约束：调试树只允许作为计划的最后一个 `run` 步骤，必须
有独立 observer，不能与 fixture 自报后代模式混用。root 退出或期限到达后，
先终止本轮 Job，并继续处理退出事件和交接新发现的句柄；整个调试树消失
后再发送冻结标记。因此不以仍能创建后代的 Chrome 树作为冻结证据。
正常退出、非零退出、持续运行触发期限三个合成场景必须分别验证 observer
的确切 Job 成员检查、拓扑检查、每个句柄等待与旁观者存活。

`support/acceptance_debug_tree.rs` 只负责处理创建线程上的调试事件：

- 由 `CREATE_PROCESS_DEBUG_EVENT` 获取内核提供的进程句柄；复制为只读查询+
  等待句柄，在对应事件仍暂停时核验其属于调用者提供的**确切 Job**。
- 返回待交接的新进程；调用者先交给独立 observer，ACK 后才继续事件。
- 活动 PID 表仅路由事件，不作为打开/终止进程的参数。全部自有句柄保留到
  本轮清理核验结束；不把程序、路径、命令行或输出正文写入日志。
- 未接入时不改变既有 supervisor 的创建标志、等待方式和验收哈希。

后续接线须在同一个创建线程上使用 `DEBUG_PROCESS`（不是
`DEBUG_ONLY_THIS_PROCESS`），替换 run 步骤的阻塞等待，并在 teardown 保持
事件泵。独立 observer 须接受 supervisor 来源的 Descendant 句柄；其冻结
检查仍按精确 Job 查询句柄逐生命周期核验。

## 官方 API 依据

- https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
- https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-waitfordebugevent

`DEBUG_PROCESS` 覆盖子进程调试链，但后代另建调试链时会中断传播；所以不能
只凭这个标志外推 Chrome 覆盖，必须保留冻结名单的完整性核验。

## 已执行证据（2026-10-10）

后续接线与证据（取代下文早期“未接 observer”状态）：

- supervisor 新增 `debugDescendants`，仅允许有独立 observer 的最后一个
  run 步骤，不能混用 fixture 的 `observeDescendants`。事件泵在创建线程，
  descendant 交接 ACK 前不继续事件；清理时先终止自己的 Job，再泵退出
  事件、等全部真实句柄，最后发送冻结标记。无调试标志的原路径不变。
- observer 在核验失败时返回拒绝 ACK；只用仍存活句柄核对冻结 Job 名单，
  保留已退出生命周期的句柄用于最终等待，避免 PID 复用误作身份。
- 最新完整合成矩阵 19/19 pass、0 failed/blocked，包含原 15 项以及
  debug-normal、debug-exit-3、debug-deadline、debug-supervisor-crash：
  `C:/Users/Public/Temp/opencode/ownership-debug-frozen-17a29d9b0dba4a5abe2680e19f6f9214/matrix-report.json`。
- debug crash 仅使用冻结 fixture 的固定双进程树：observer 检查两个真实
  句柄与确切 Job 名单，关闭全部 Job 副本并暂扣后代交接 ACK，runner 用
  自身创建句柄结束 debugger，随后逐句柄有界等待。不是 Chrome 的冻结
  协议，也不外推任意活跃树完整性。
- 测试版 `D:/playwright-browsers/chromium-1243/chrome-win64/chrome.exe`
  在独立 profile、环境白名单内，保留 sandbox，以创建管道连接 CDP，
  不监听或认领调试端口，不做视觉测试。14 个真实进程句柄交给 observer，
  各自通过确切 Job 检查与退出等待，Job 归零，旁观者未受影响。
  两次通过证据：
  - `C:/Users/Public/Temp/opencode/browser-ownership-frozen-a55387d3e2194a9087eb0cdb163ed1c0/matrix-report.json`
  - `C:/Users/Public/Temp/opencode/browser-ownership-frozen-f7ee93796f9d4c468eb982ef450de830/matrix-report.json`
- 重复命令：`pwsh -NoProfile -File C:/Users/Public/Temp/opencode/run-browser-ownership-20261010.ps1 -Chrome D:/playwright-browsers/chromium-1243/chrome-win64/chrome.exe`。
  不使用 Puppeteer launch/close 的潜在 PID 清理 fallback；Node 仅创建
  Chrome 并 connect，结束时 disconnect，所有终止由 supervisor 的 Job 完成。
- 本机安装版 Chrome 的多个探测在 CDP connect 超时：0 回复、未通过；
  33 个进程句柄及清理独立核验通过，但不能把清理通过写成页面通过。
  失败目录保留。PowerShell 准备阶段的 HOME 只读变量与旧 Puppeteer
  包目录路径错误均在任何测试进程启动前失败，修复后使用新目录运行。
- 19 项首轮唯一失败是旧断言要求 crash 前非空 status；debug crash 在
  第一个 run 完成前发生，空 status 合法。检查改为要求 status 存在且
  所有已完成行合法；原 supervisor-crash 仍要求非空。失败目录保留：
  `C:/Users/Public/Temp/opencode/ownership-debug-frozen-9ad2b64f733f49fd8ba9e41592f35e59/`。

- `cargo check --locked -p fintwind-remote-host --example acceptance_supervisor`
  通过；模块仅被编译，尚未改变 supervisor 创建标志或等待逻辑。
- `cargo build --locked -p fintwind-remote-host --example acceptance_debug_tree_blackbox`
  通过。黑盒自身不在外层 Job；它只创建一个 suspended 合成 fixture，
  按创建句柄分配独占 KillOnClose Job 后启动。fixture 创建一个合成后代，
  两个调试创建事件分别提供真实句柄，均通过确切 Job 成员核验。
- 首跑失败证据：
  `C:/Users/Public/Temp/opencode/debug-tree-frozen-3457ef2dec2742b48c805a2f8663388d/round/report.json`。
  两个 EXIT 事件与 Job 归零已观察，但零时限句柄检查未通过。
  改为继续 EXIT 事件后按同一清理期限有界等待，不丢弃失败目录。
- 修改后两次独立目录运行通过，均 `createdHandles=2`、`exitEvents=2`、
  `allHandlesExited=true`、`jobActiveProcessesZero=true`：
  - `C:/Users/Public/Temp/opencode/debug-tree-frozen-e6473225583c4527bffe6be170bb290b/round/report.json`
  - `C:/Users/Public/Temp/opencode/debug-tree-frozen-e8fb8eb6b4c74af0bbec28f0af1494c5/round/report.json`
- 运行器：`pwsh -NoProfile -File C:/Users/Public/Temp/opencode/run-debug-tree-20261010.ps1`。
  每轮保存 exe、源码、运行器、SHA-256 与报告；fixture SHA 对齐此前 15/15
  冻结矩阵的 fixture。未扫描或认领历史进程，未终止生产或其他会话的进程。
- 尚未覆盖：独立 observer 交接、Chrome、超时矩阵、debugger crash、
  引发异常/创建新调试链的后代。`browserVerified=false`、
  `observerVerified=false`、`fullPlanVerified=false` 保留。
