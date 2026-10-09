# process-ownership-plan.md — synthetic Windows process-ownership black box

本文件是 **纯合成** 的 Windows 进程归属（process ownership）整体黑盒设计与失败矩阵，
对应 `scripts/fixtures/remote-control/README.md` 第 107–125 行的
"Synthetic process-ownership black box (for main, after review)" 一节。

**红线（本任务只到代码 + 编译，不运行任何产物）：**

- 不运行任何产物，不运行 supervisor / daemon / provider / browser，不运行 `--list`；
- 不 kill、不按 PID `OpenProcess` 补认领、不做全机扫描；
- 不触碰生产 DB、不引用真实服务；
- **不解除 HOLD**。gate 在**同级仓库**
  `E:/work/Opencode-Desktop/fintwind-pwa/scripts/provider-chain-blackbox.mjs`
  第 62 行（`throw new Error("Provider acceptance is blocked pending owned-process
  isolation verification.")`），本任务未打开、未修改该仓库任何文件；
- `fullPlanVerified` 恒为 `false`。

**归属（2026-10-10 更新）：** 本黑盒的四个实现文件——`acceptance_supervisor.rs`、
`support/acceptance_observation.rs`、`acceptance_ownership_blackbox.rs`、
`acceptance_ownership_fixture.rs`——自 GLM 收尾批起由实施代理直接拥有并修改；
PWA 侧 runner（`fintwind-pwa/scripts/provider-chain-blackbox.mjs`）仍归 main，
本文所有批次都只读未改它。早期"supervisor 为 main 所有、代理只读"的约束已被
用户后续授权取代，保留历史段落时以本节为准。

## 文件清单

| 文件 | 作用 |
| --- | --- |
| `crates/fintwind-remote-host/examples/support/acceptance_observation.rs` | **共享观测 IPC 模块**（1043 行）。放在 `examples/support/` 使 Cargo 自动发现（`examples/*.rs`、`examples/*/main.rs`）不会把它当成可运行 example——已核实 `target/debug/examples/` 下没有 `acceptance_observation.exe` |
| `crates/fintwind-remote-host/examples/acceptance_ownership_blackbox.rs` | 黑盒 runner + 默认独立 observer（同二进制 `--role observer` 模式） |
| `crates/fintwind-remote-host/examples/acceptance_ownership_fixture.rs` | 可信合成子进程，按参数表演脚本角色 |
| `scripts/fixtures/remote-control/process-ownership-plan.md` | 本设计文档 |

## main 已裁定 / 已落地事项

1. **证据标准**：精确 job 查询句柄 + 每个冻结后代的真实进程句柄有界等待。
   **不接受** `IsProcessInJob(handle, NULL)` 弱变体。
2. **空 steps 已裁定为拒绝，且 main 已在 supervisor 第 667 行实现**
   （`if plan.steps.is_empty() { bail!("an acceptance plan must contain at least one step"); }`），
   本任务不再称其"未落地"。
3. **不为测试加旁路**：不增加命名 job、不加环境故障开关。观测接口只暴露句柄给
   指定 observer，不改 containment 行为；接口缺席时 supervisor 行为与今天逐字节一致。

---

## 1. 观测 IPC 模块（已实现）

### 1.1 pub API

| 项 | 说明 |
| --- | --- |
| `PROTOCOL` / `MAX_MESSAGE_BYTES`(2048) / `MAX_QUEUED_LINES`(64) / `MAX_QUEUED_MESSAGES`(4) / `NONCE_BYTES`(16) | 协议标签与各类上限 |
| `Source` | `Supervisor` / `Fixture`——**结构性**写者身份 |
| `WriterId{source, fixture}` | 一个写者的结构身份；`WriterId::supervisor()` / `WriterId::fixture(id)` |
| `Kind` | `RootChild` / `Descendant` / `JobQuery` / `FixtureChannel` / `TopologyFrozen` |
| `Handoff` | `protocol`/`nonce`/`source`/`fixture`/`transfer_id`/`kind`/`claimed_pid`/`process_handle`/`job_handle`/`granted_access`，`deny_unknown_fields` |
| `Ack` | `protocol`/`nonce`/`transfer_id`/`accepted`/`reason` |
| `Channel` | 四条管道端的所有者；`create()`、`mark_subject_ends_inheritable()`、`mark_observer_ends_inheritable()`、`request_write()`/`response_read()`/`request_read()`/`response_write()` |
| `SubjectChannel` | 写者侧：`from_raw(WriterId, request_write, response_read)`、`handoff(&Handoff, deadline) -> Result<Ack>`、`finish(deadline)` |
| `ObserverHub` | observer 侧：`new()`、`register(WriterId, request_read, response_write)`、`next(deadline) -> Result<Option<(WriterId, Handoff)>>`、`keep_job_copy(handle)`、`close_job_copies()`、`ack(&WriterId, &Handoff, accepted, reason, deadline)`、`finish(deadline)` |
| `duplicate_for_observer(target, source, access)` | **唯一**移交点；访问掩码只在这里强制 |
| `dup_handle_with_access(source, access)` | 本进程内收窄（runner 把 observer 创建句柄收窄成 `PROCESS_DUP_HANDLE`） |
| `nonce()` | 每轮随机；不是句柄、不进 `HANDLE_LIST`、从不落日志 |
| `verify_handoff(&Handoff, nonce, &mut BTreeMap<WriterId,u64>)` | 来源证明；强制 `GetProcessId(handle)==claimed_pid` |
| `ack(&Handoff, accepted, reason)` | 构造 ACK |
| `OBSERVER_PROCESS_ACCESS` / `OBSERVER_TARGET_ACCESS` / `SYNCHRONIZE` | 显式访问掩码常量 |

### 1.2 草案纠错逐条落地

| 草案纠错 | 落地 |
| --- | --- |
| 单条管道无法 ACK | **每写者两条匿名管道、四个端点**（`Channel::create`），request + response |
| 多写者消息原子/串行或各独立 channel | **每写者独立 channel pair**（supervisor 一条、每个观测 fixture 一条），单写者单管道，物理上不可能交错 |
| nonce 不是 HANDLE | 只走命令行（fixture 走该步骤 env），从不进 `HANDLE_LIST`，从不写日志 |
| 仅允许显式最小继承 | 仅 observation 实例继承：supervisor 继承 `[observer句柄, request_write, response_read]`；observer 继承 `[request_read, response_write]` |
| observer 进程真句柄由 runner duplicate | runner 先建 observer，`dup_handle_with_access(observer, PROCESS_DUP_HANDLE)` 收窄 + 置可继承后交给 supervisor |
| ACK + topology-frozen bounded | 所有读写受 `deadline: Instant` 约束；observer 自身总寿命 bounded，只由 runner 创建句柄结束 |
| 副本 Job 查询使 kill-on-close 失效 | observer 在 `TopologyFrozen` 后 `close_job_copies()` 并 ACK，只保留进程等待句柄 |
| observer 死亡/超时 ⇒ supervisor 主动 `TerminateJobObject` | 列入 main 接入要求 2.3 |
| supervisor 先 crash 而 observer 还 alive | request 管道 EOF 即 subject 消失；此时 job 副本未关则记 `observation.killOnClosePhase` = notCovered——**该次 crash 不具 kill-on-close 证明** |
| runner+observer overall deadline 不死锁 | 双方 worker 都有界收口；`finish` 超限会关句柄强制取消并报 `workerNotReaped` |
| 不得用额外 outer job 容纳 subject | runner 不把 supervisor 放进自己的任何 job |
| 协议 version independent / deny unknown / bounded | `PROTOCOL` 标签不符即拒；`deny_unknown_fields`；超限消息在读线程里直接判违例 |
| read/write 线程受 deadline | 读：硬 cap + 有界队列 + 违例 fail closed；写：有界 inbox + 每次写完成 rendezvous |
| GrantedAccess 自述不能证明权限 | 掩码在 `duplicate_for_observer` 内强制；消息里的 `granted_access` 只作标签记录 |
| ownership proof + 唯一 id，不能仅 nonce | 结构 `WriterId`（含 fixture id）+ 按 `WriterId` 单调 `transfer_id` + `GetProcessId(handle)==claimed_pid` |
| 冻结 expected root PID 由真实 handle 验证 | topology 检查用 `{GetProcessId(h)}` 与 job 自身 PID 名单逐一比对 |
| 无终止权限 observer 只能等待 | 移交掩码 `PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE`，不含终止权限 |
| observer 只能由 runner 创建句柄终止 | `Observation::finish` 只用 `child.wait(...)` |
| partial 失败 RAII / 无 PID 补认领 / 无 secret log / 无环境继承 | `End`/`SubjectChannel`/`ObserverHub`/`Channel` 全 RAII；无 `OpenProcess`；nonce 只记录字节宽度 |

### 1.3 IO worker 生命周期（与代码一致的现状，2026-10-10 修订）

- `WriteWorker` / `LineReader` 的 worker 通过 `std::fs::File` 的 `write_all` /
  `read` 使用句柄，**worker 线程自己拥有管道端**（`File` 在线程内由裸句柄
  构造、随线程退出释放），结构体不持有、不重复关闭。`LineReader` 用
  `PeekNamedPipe` 轮询，读永远不会阻塞在对端不写的调用里。
- `windows-sys` 已启用官方 `Win32_System_IO` feature（`CancelSynchronousIo`
  所在模块，2026-10-10 按最小改动加入 manifest）——早期"模块内内联
  `unsafe extern "system"` 声明"的过渡方案已删除。
- `finish(deadline)` 三段式，全部有界：置 stop → 有界等 finished 信号 →
  未退则 `CancelSynchronousIo`（持 `CreateThread` 返回的**真实线程句柄**，
  绝不 `OpenThread` 按 id 补认领）打断卡住的同步写，再给一次有界机会 →
  内核 `WaitForSingleObject` 确认线程退出后才 close 线程句柄 → 仍失败报错。
  **任何路径都不会在线程仍在读写时关闭它正在使用的 I/O 句柄**：I/O 句柄
  由 worker 自己的 `File` 持有，随线程退出释放，句柄值不存在被复用窗口。
- `WorkerThread` 实现 `Drop`：spawn 后早退路径（如 `?` 传播）只 close 线程
  句柄并请求停止，不触碰线程正在用的 I/O 端。
- `ObserverHub::finish` 依次收口所有 reader 与 response writer，最后
  `close_job_copies()` 作为最后兜底（正常路径是 `close_job_copies` + ACK，
  发生在 crash 之前）。
- **移交失败但 observer 已收到副本**：observer 的 `ObserverHub::finish` 会关闭
  它注册的全部 reader/response 与 job 副本，所以副本不会成为 kill-on-close 的
  阻碍；subject 侧则按"观察失败阻止 Resume"用创建句柄终止该 child、显式
  `TerminateJobObject` 自己的 job 并有界等待——**不依赖 kill-on-close 兜底**。

---

## 2. 接入说明（**supervisor subject 侧与 FixtureChannel 均已实现**）

supervisor 侧已实现（`acceptance_supervisor.rs`，本节描述与代码一致）：

- 启动时从命令行重建 subject 端：`--observer-handle <值> --observer-nonce <hex>
  --observation-request <值> --observation-response <值>`。四个参数**全或无**：
  任一出现而集合不全即拒绝启动；全部缺席时 supervisor 行为与无观测时完全一致。
- `Observation::start` 从继承值重建 `SubjectChannel`（`WriterId::supervisor()`）。
  通道打不开 = 该轮以 `failureClass: "observation"` 失败退出，**不静默降级**为
  无观测运行。
- **每个 root child**：`AssignProcessToJobObject` 成功之后、`ResumeThread` 之前，
  `duplicate_for_observer`（在模块内强制 `OBSERVER_PROCESS_ACCESS`，消息里的
  `granted_access` 只是标签）+ `Kind::RootChild` handoff，`transfer_id` 单调递增。
  ACK `accepted=false`、写失败、EOF、超时——任何非 `Ok(true)` 结果都：kill
  该 child（创建句柄 + 有界等待）→ **主动 `TerminateJobObject` 自己的 job**
  （observer 可能仍持 job 副本，kill-on-close 会被延迟，不能依赖）→ 按启动
  失败上报 `class: "observation"` → break。child 绝不在未验证的 handoff 之后运行。
- **冻结序列，只发一次**：step 循环正常结束后、teardown 之前，
  `Kind::JobQuery`（dup 真实 job 句柄）→ `Kind::TopologyFrozen`。冻结失败同样
  主动 terminate job 并按 observation 失败上报。deadline 一律用共享
  `total_deadline`，不附加超时。
- teardown 完成后 channel 自身 worker 在同一 cleanup deadline 内收割；
  收割失败使该轮不算绿。final 事件携带 `observation`:
  `integrated|absent|failed|unreaped`。

**观测 fixture 契约（2026-10-10 已实现，两侧与 runner 同批落地）：**

- **plan 字段**：`Step.observeDescendants: bool`（`#[serde(default)]`，缺省
  `false`；`deny_unknown_fields` 继续拒绝一切其它未知字段）。`false` 的 step
  行为与无观测时代逐字节一致：只有三个 stdio 句柄进 `HANDLE_LIST`，env 不含
  任何观测键。prevalidation 拒绝 `observeDescendants` 与 `mode != "run"` 的
  组合——**run step 的 verified exit 就是"后代 hand-off 已完成"的同步点**，
  不靠 sleep 猜，也不需要额外 ready/frozen 握手。
- **授权顺序**：观测 step 的通道在 fixture 进程存在之前建管并注册——
  `Channel::create` → `mark_subject_ends_inheritable` → observer 两端经
  `duplicate_pipe_end_for_observer`（唯一移交点）→ `Kind::FixtureChannel`
  handoff（pipe 端走 **显式类型字段** `pipeRequestRead`/`pipeResponseWrite`，
  绝不冒充 `process_handle`/`job_handle`——管道没有 pid，`GetProcessId` 来源
  认证只属于真实进程句柄）→ observer `ObserverHub::register` 并 ACK →
  supervisor 才创建 fixture 进程（assign 进 job）→ `Kind::RootChild` 真实
  创建句柄 handoff → ACK → `ResumeThread`。
- **fixture id**：supervisor 内单调计数器（从 1 起），`WriterId::fixture(id)`
  结构唯一，两个 fixture 永不共享写者身份。
- **继承与环境**：观测 step 的 `HANDLE_LIST` 是 3 stdio + observer 句柄
  （`PROCESS_DUP_HANDLE`，runner 创建时已标 inheritable）+ 该 fixture 的
  request_write/response_read；**值**经本 step 环境的
  `FINTWIND_OBSERVATION_{HANDLE,NONCE,REQUEST,RESPONSE,FIXTURE_ID}` 五键
  下发——句柄值走 env，句柄本体只走 `HANDLE_LIST`；nonce 是关联令牌，从不是
  句柄、从不进 `HANDLE_LIST`。fixture 收到**部分**观测键即退出码 2（broken
  grant fail-closed），五键齐全才启用；无 grant 的角色照常运行。
  非观测 step 的 env 与继承列表不含任何观测内容。
- **诚实失败**：fixture 的 `Descendant` handoff 未被接受 ⇒ 该 step 以非零
  退出（5）失败，supervisor 按普通 step 失败处理——**已验证的 exit 0 永远
  不替没有发生的 hand-off 背书**。supervisor 侧任何 grant/handoff/ACK 失败
  ⇒ 不 Resume、显式 `TerminateJobObject`、有界清理（见上文 RootChild 规则）。
- **冻结时机**：`Kind::JobQuery` + `Kind::TopologyFrozen` 仍在全部 step 结束
  后、teardown 前各发一次；因为观测 step 是 run 模式，此时它已退出，其后代
  hand-off 已完成，拓扑里不会再有新成员。
- **observer 侧**（`acceptance_ownership_blackbox.rs`）：收到 `FixtureChannel`
  时校验"pipe 字段之外不得夹带 process/job 句柄"，用显式 pipe 字段注册该
  fixture 的 reader/response；`Descendant` 到达后按既有判据
  `IsProcessInJob(handle, exactJob)` + job 自身 PID 名单与 `GetProcessId`
  真实句柄集合逐一比对；crash 前关闭全部 job 副本并 ACK，只保留等待用进程
  句柄。runner 终止只用本轮 supervisor 的创建句柄，逐后代真实句柄有界等待。
- **normal 与 supervisor-crash 场景的 plan 均已声明** `observeDescendants:
  true` 的可信 fixture step（`--grandchild`），crash 场景的后代退出证明
  走 observer 持有的真实句柄有界等待。

原始设计稿（已按此形状落地为上文 `Observation` 实现，保留作对照）。

supervisor 侧最小接入点，全部在 `acceptance_supervisor.rs` 既有
`create::create_suspended` 前后，不改 containment 逻辑：

```rust
// 启动时一次：从命令行取值重建 subject 端
let observer: HANDLE = /* --observer-handle 的值，已继承，PROCESS_DUP_HANDLE */;
let nonce: String = /* --observer-nonce 的值 */;
let channel = observation::SubjectChannel::from_raw(
    observation::WriterId::supervisor(), request_write, response_read)?;

// 每个 child：AssignProcessToJobObject 成功之后、ResumeThread 之前
let dup = observation::duplicate_for_observer(
    observer, created.guard.process, observation::OBSERVER_PROCESS_ACCESS)?;
let ack = channel.handoff(&observation::Handoff {
    protocol: observation::PROTOCOL.to_owned(),
    nonce: nonce.clone(),
    source: observation::Source::Supervisor,
    fixture: None,
    transfer_id: next_id(),                 // 单调
    kind: observation::Kind::RootChild,
    claimed_pid: created.guard.pid,
    process_handle: Some(dup as u64),
    job_handle: None,
    granted_access: "PROCESS_QUERY_LIMITED_INFORMATION|SYNCHRONIZE".to_owned(),
}, deadline)?;
if !ack.accepted {
    // 观察失败阻止 Resume：创建句柄终止 + 有界等待 + 按启动失败上报
    created.guard.kill_and_wait(5_000)?;
    bail!("the observation channel refused the hand-off");
}
// ResumeThread 只在 ack.accepted 之后
```

job 查询句柄在**冻结前**移交一次（`Kind::JobQuery`，`job_handle: Some(dup as u64)`）；
观测 fixture 的 channel 由 supervisor 建管（`Kind::FixtureChannel`，
`fixture: Some(id)`、`process_handle`/`job_handle` 复用为该 fixture 的
request_read / response_write 值），并把 nonce 与该 fixture id 经该步骤的 env 下发。

runner 传给 supervisor 的命令行（已由 runner 实现并验证可编译）：
`--observer-handle <值> --observer-nonce <hex> --observation-request <值> --observation-response <值>`。

**观测 fixture 侧**（已实现，见 `acceptance_ownership_fixture.rs`）：
自建孙进程 → `duplicate_for_observer(observer, grandchild, OBSERVER_PROCESS_ACCESS)`
→ `SubjectChannel::handoff(Kind::Descendant, fixture: Some(id))` → 收到 ACK 后
**本角色不再调用任何 `CreateProcess`**（拓扑冻结）。

### 2.3 subject 侧必须满足的行为（否则观测无效）——已逐条落地

1. **观察失败阻止 Resume**：复制失败 / 写失败 / ACK 超时 / ACK `accepted=false`
   ⇒ 不 `ResumeThread`，创建句柄终止 + 有界等待后按启动失败上报。
   **已落地**（RootChild handoff 分支，非 `Ok(true)` 即 kill + terminate job）。
2. **observer 死亡或超时 ⇒ supervisor 主动 `TerminateJobObject` 自己的 job**：
   observer 持有 job 查询副本会使 kill-on-close 失效，不能靠它收尾。
   **已落地**（观测失败路径与冻结失败路径都显式 `job.terminate()`）。
3. **job 查询副本的关闭必须发生在 crash 之前并 ACK**，否则该次 crash 不构成
   kill-on-close 证明（runner 的 observer 已按此标注阶段）。
   **supervisor 侧已可配合**：JobQuery + TopologyFrozen 在 teardown 前发出。
4. **`TopologyFrozen` 只发一次**：所有步骤起来之后、teardown 之前。不是每步之前。
   **已落地**（step 循环后的单个冻结序列）。
5. **不得用外层 job 容纳受测子树**，不得新增 kill 旁路。
   **未触碰**：无命名 job、无环境故障开关、无 outer job。

---

## 3. 失败矩阵：判据已实现 / 未运行 / 阻断

> 「判据已实现」仅表示 runner 里写了这条断言，**不等于已运行、不等于已通过**。
> 本任务未运行任何产物。runner 逐条结论只有三态：`verified` / `failed` / `notCovered`。

| # | 失败模式 | supervisor 应有行为 | 独立证据 | 状态 |
|---|---|---|---|---|
| 1 | normal | green，`containmentVerified=true` | 退出码、status JSONL 全字段、outsider 存活、fixture 内核事实、plan 冻结哈希前后、**observer 通道** | 判据已实现 + 观测通道已接线，未运行 |
| 2 | exit-3 | `exit-3` 自报名，teardown 仍 verified，后续副作用步骤不执行 | 退出码、status JSONL、marker 文件不存在 | 判据已实现，未运行 |
| 3 | no-ready（EOF） | `readiness-eof` | 退出码、status JSONL | 判据已实现，未运行 |
| 4 | no-ready（超时） | `readiness` + `totalDeadlineExceeded` | 退出码、status JSONL | 判据已实现，未运行 |
| 5 | malformed / wrong pid | 不接受 → `readiness-eof` | 退出码、status JSONL | 判据已实现，未运行 |
| 6 | wrong pid 后接正确行 | **接受后者 → green** | 退出码、status JSONL | 判据已实现，未运行 |
| 7 | protocol | 不接受 | 退出码、status JSONL | 判据已实现，未运行 |
| 8 | 非 loopback | 不接受 | 退出码、status JSONL | 判据已实现，未运行 |
| 9 | flood > 64 KiB | `readiness-cap` | 退出码、status JSONL | 判据已实现，未运行 |
| 10 | 不读 stdin（随即退出） | 不死锁，`ioWorkersExited=true` | 退出码、status JSONL | 判据已实现，未运行 |
| 11 | 不读 stdin（长驻） | teardown 断管后收敛 | 退出码、status JSONL | 判据已实现，未运行 |
| 12 | total deadline | `total-deadline`，final 仍存在 | 退出码、status JSONL 末行 | 判据已实现，未运行 |
| 13 | invalid plan（7 子项） | 前 6 个退出码 1 且无 status；**空 steps 按 main 裁定拒绝**（main 已在 supervisor 667 行实现 bail） | 退出码、status 文件存在性 | 判据已实现，未运行 |
| 14 | status 不可写 | 退出码 1，无 status，无 child | 退出码、文件存在性 | 判据已实现，未运行 |
| 15 | spawn fail | `spawn` | 退出码、status JSONL | 判据已实现，未运行 |
| 16 | output-substitute 缺失 | `output-substitute` | 退出码、status JSONL | 判据已实现，未运行 |
| 17 | supervisor crash | 内核经 KILL_ON_JOB_CLOSE 收尾；crash 路径不写 final | supervisor 退出（真实句柄）、status 无 final、outsider 存活、**observer 对每个移交后代的真实句柄有界等待** | **subject 侧与 Descendant 通道均已接线**（normal/crash plan 已声明观测 fixture step）；未运行 |
| 18 | 受控 outsider 仍 alive | teardown 不得触碰它 | 每场景后真实创建句柄查 `STILL_ACTIVE` | 判据已实现，未运行 |
| 19 | 同 job（supervisor 子树） | —— | `IsProcessInJob(handle, exactJobHandle)` 逐个 + job PID 名单与 `GetProcessId(handle)` 集合相同 | **判据链路已完整接线**（exact job 查询句柄 + RootChild/Descendant 真实句柄均由 supervisor/fixture 移交）；未运行 |
| 20 | assignment failure | `containment` | —— | **NotCovered**，且不提议加故障旁路 |

`job-create` / `resume` / `wait` / `cleanup` 等失败类**无法从外部注入**，一律记为
NotCovered，不造假绿。

### `observation.subjectIntegrated` 完整判据（三条全真才算集成生效）

1. observer 在 supervisor channel 上收到 ≥1 个 hand-off，且 `verify_handoff` 通过
   （nonce 匹配 + 该 `WriterId` 的 `transfer_id` 严格递增 + `GetProcessId(handle)==claimed_pid`）；
2. observer 收到 `Kind::TopologyFrozen`（证明 subject 侧走完了冻结路径）；
3. observer 对每个 `RootChild`/`Descendant` 都持有一个通过
   `IsProcessInJob(handle, exactJobHandle)` 的进程句柄。

三条有任一不成立 ⇒ `notCovered`，不伪称集成。subject 侧已实现 2.3 的五条
行为要求与 FixtureChannel 契约（见第 2 节），因此三条判据的**产生链路**已
全部存在；判定仍由 observer 在真实运行中做出，未运行前整体仍是
`notCovered`，不因接线完成而预支任何绿色。

---

## 4. 句柄来源论证（原"为什么必须 blocked"一节）

要证明「子树同 job」和「crash 后每个后代退出」，必须有**指向每个后代的真实句柄**。
取得另一个进程所建子进程的句柄，Windows 上只有四条路：① 自己是创建者（runner 不是）；
② 从持有者 `DuplicateHandle`（supervisor 是唯一持有者）；③ `OpenProcess(pid)`（禁止）；
④ 全机句柄枚举（禁止且需特权）。**②是唯一合法路径，supervisor 必须参与。**

已否决的"替代证明"（都是推断）：命名 job 对象消失（只证明最后一个句柄关了，
**不证明每个后代退出**）；`jobActiveProcessesZero` 自述单独成立；「已知 root 干净
⟹ 全树干净」。最后者的唯一运行时交叉核验是：**冻结时刻 job 自身 PID 名单必须与
observer 持有的真实句柄集合（经 `GetProcessId`）逐一对应**，出现无对应句柄的成员即
报告「无法解释的成员」，不得宣称干净。

**结论（2026-10-10 修订）**：② 路径的 subject 侧已实现——supervisor 移交每个
root child 的创建句柄与 exact job 查询句柄，观测 fixture 移交其后代创建句柄
（第 2 节契约）。第 3 节第 17/19 行的判据链路因此**可以独立成立**；但本黑盒
仍未运行，任何逐条结论都只能在真实运行后由 observer 给出，`fullPlanVerified`
保持 `false`，编译与接线不作为 functional gate 的放行依据。

---

## 5. 安全与清理契约

- 一轮 = 一个独立目录 `<parent>/ownership-<utc>-<pid>-<nanos>/`；不写别处。
- 子进程环境按**变量名**白名单（`PATH` 收敛到 System32），只记录变量名。
- raw stdout/stderr 经管道进内存、计数后丢弃，**不落盘**；nonce 从不写任何日志或
  报告（只记录字节宽度）。报告中不出现地址、token、readiness 原文或任何真实 credential。
- 清理只有两条合法出口：本轮创建句柄上的 `TerminateProcess` + 有界等待；本轮独占
  job 上的 `TerminateJobObject`。任何有界等待失败即记 `cleanupUnverified` 并停止。
- observer 只由 runner 用自己的创建句柄结束；observer 持有的移交句柄不含终止权限，
  因此它只能等待，清理出口始终在 runner 或 subject 自己的 job 上。

## 6. 产物契约

`report.json` 含：三者的 source 与 binary SHA-256、每场景 plan 冻结哈希前后、
supervisor binary 哈希 round 首尾、逐步事实、退出事实、清理事实、每 check 三态判定
与理由、阻断清单、每场景 `rawOutput`（内存丢弃字节数 + `writtenToDisk: false`）。
`fullPlanVerified` 恒为 `false`。

## 7. 编译验证（静态状态，随批次更新至 2026-10-10）

最近一轮（GLM 收尾批，subject 侧 + FixtureChannel 全接线后）：

```
cargo check --locked -p fintwind-remote-host \
  --example acceptance_ownership_blackbox \
  --example acceptance_ownership_fixture \
  --example acceptance_supervisor
```

* 结果：通过；supervisor 仍有 4 个早期批次的未用 helper 警告（`WAIT_TIMEOUT`、
  `wait_handle`、`close`、`exit_code`，`create`/`job::wait` 模块内的预留
  helper），非本轮引入，未为消警告而删除或加 `#[allow]`。
* manifest 变更：`fintwind-remote-host/Cargo.toml` 的 `windows-sys` features
  按 main 批准**最小加入官方 `Win32_System_IO`**（`CancelSynchronousIo` 所在
  模块）。早期"模块内内联 `unsafe extern "system"` 声明、不改 manifest"的
  过渡方案已删除——不复存在，勿再引用。
* containment 契约未变：`KILL_ON_JOB_CLOSE`、按创建句柄 assign、assign 后
  （且观测 hand-off ACK 后）才 `ResumeThread`、空 steps bail。观测接口缺席
  （无观测参数）时 supervisor 行为与无观测时逐字节一致。
* `examples/support/acceptance_observation.rs` 仍是共享模块，不是可运行
  target（Cargo 不会为 `examples/support/` 生成 example）。
* **编译通过不等于已运行、不等于通过**：三侧接线已实现且可编译，但本批
  未运行任何产物，observer 的全部三态判定只能在真实运行中产生。
* **main 首次真实运行记录（2026-10-10）**：main 构建三 examples 通过（4 既有
  警告），冻结于
  `C:/Users/Public/Temp/opencode/ownership-frozen-ba11ec83e1124cc29941e966aaa6e840`
  （三个 exe + sha，冻结后未变；证据保留）。纯合成 normal 首跑失败：runner
  main 线程 panic `failed to join on thread: 句柄无效 (os error 6)`，exit 1，
  normal-report 未生成。main 命令立即停止全部 runtime。源码诊断修复（见
  `remote-control-progress.md` 同日小节）：确定的 `observer.join_io()` 过早
  join（observer 须存活收集 hand-offs 却被 join 到退出）已删除，drains 统一
  由 `Observation::finish` 收；`join_io` 以 `catch_unwind` 包住 std
  `JoinHandle::join`（部分 Rust 版本在内核等待失败时 panic 而非返回 Err）并
  把 reaped 失败记入证据链；runner 主体包 `catch_unwind`——panic 时显式
  finish_outsider（本轮独占 job 终止+有界等待）并写出 `outcome:"panic"`、
  `cleanup.unverified` 的**失败** report，exit 1，绝不伪装干净。
  **单行根因未唯一锁定**：`as_raw_handle` 误 close / WorkerThread 句柄与
  std join 混用两条提示路径经全文 grep 均未在现源码中出现（已逐一排除）；
  若修复后重跑再现 panic，panic report 的 `panicMessage` 原文即为下一步
  取证。重跑与否由 main 决定，本代理未运行。

## 8. 下一步

1. ~~main 按第 2 节接入 supervisor 的 subject 侧，并满足 2.3 的五条行为要求。~~
   **已实现**；~~FixtureChannel 契约缺失阻断~~ **也已实现**（见第 2 节契约，
   normal/crash plan 已声明观测 fixture step）。
2. 由 main **独立**运行；运行通过后才可讨论 functional gate。PWA runner 的
   incident gate（同级仓库 `fintwind-pwa` 顶层 `throw`）只有 main 可移除。
3. 运行顺序（main 执行，本代理未运行）：先纯合成 normal + supervisor-crash
   两场景（独立目录、环境白名单、受控 job 边界），全部 verified 后才谈
   provider 链路；`assignment-failure` 等外部不可注入类保持 NotCovered，
   不为绿而造故障旁路。
