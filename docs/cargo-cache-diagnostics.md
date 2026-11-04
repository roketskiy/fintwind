# Cargo 缓存诊断

## 范围

Tests 工作流记录 debug 测试与稳定版编译检查的 Cargo 指纹决策，并在运行时输出
真实进度。共享 setup 动作对完整的已审计工作区构建输入集使用内容校验时间戳；
不修改 Cargo 指纹，不关闭新鲜度检查。只有 CI 把工作区 dev/test 优化等级设为 0，
清单中的依赖覆盖仍保留优化后的 GPUI。本地开发与发版 profile 不变。

`scripts/cargo-cache-diagnostics.ps1` 执行既有 Cargo 命令并保留退出码。工作流在
成功与失败时都上传诊断，保留 7 天：

- `cargo-cache-diagnostics-test`
- `cargo-cache-diagnostics-release-check`

## 产物内容

| 文件 | 内容 |
| --- | --- |
| `cargo.stderr.log` | 完整 Cargo 进度、编译器诊断与指纹追踪 |
| `cargo.stdout.log` | 测试输出 |
| `fingerprint-summary.log` | 脏/过期原因、指纹错误、完成情况与编译错误 |
| `diagnostics.json` | Cargo/rustc 版本、命令、profile、所选编译器环境、计时、退出码与构建前时间戳 |
| `phases.json` | 编译/测试计时、超时、退出码与子进程退出前看到的进度 |
| `source-inputs-prepare.json` | 内容与构建上下文哈希、文件/字节数 |
| `source-inputs-restore.json` | 恢复的文件/目录数与变更输入 |
| `source-inputs-capture.json` | 构建前快照状态与扫描时间 |
| `source-inputs-verify.json` | 构建后输入是否仍与构建前快照及缓存键一致 |

元数据含所选项目与 vendored GPUI 输入的哈希与时间戳，以及恢复的 GPUI 指纹
时间戳；不转储完整进程环境。指纹过滤覆盖 Cargo 新旧模块名，不开启 HTTP 或
仓库认证追踪。

编译进度、警告/错误与测试输出随文件增长实时可见；原始指纹追踪留在产物中，
不刷屏。每 30 秒心跳上报当前阶段与已用秒数；两路流按有界块读取，保留各自流内
顺序；收尾给出编译与编译后耗时摘要，`GITHUB_STEP_SUMMARY` 收到简明时间表。

PowerShell 入口委托 `cargo-cache-live.py`，在新建的独占 Windows Job Object 中
启动仅做重定向的 `cargo-cache-command.ps1`。默认期限 3000 秒（`-TimeoutSeconds`
可改，上限 5400）；超时返回 124，只关闭该作业及其后代，不扫描 PID、端口或
进程名。Runner 作业超时仍是外层上限。本地执行请用隔离数据目录与受控环境。

## 首次失效定位

1. 先在 setup 动作日志确认作业恢复了编译缓存；冷缓存天然缺指纹，不是缺陷证据。
2. 读 `fingerprint-summary.log` 找第一次直接 GPUI 失效。
3. 沿 `StaleDepFingerprint` 回到其依赖；这通常是下游后果，不是原始原因。
4. 对 `ChangedFile`，比较源码 `stale_mtime` 与缓存指纹 `reference_mtime`，用完整
   追踪定位文件。
5. 改缓存策略前，先排查环境/profile、构建脚本输入、缺失产物等其他原因。

缓存下载命中不等于 Cargo 复用了全部产物；不要关闭新鲜度检查，不要回拨整个检出
目录的时间戳，两者都会掩盖真实源码变更。

## 本地复现

需要 Windows、Python 3、PowerShell 7 与已安装的 MSVC Rust 工具链。用
`rustc --print sysroot` 取得工具链目录并显式传入：

```powershell
python scripts/cargo-cache-diagnostics-blackbox.py --sysroot '<rustc sysroot>' --output 'C:/Users/Public/Temp/opencode/cargo-cache-diagnosis/result.json'
```

每套离线检查允许 5 分钟；每次编译调用期限 90 秒，跑在独占 Windows Job Object
中。运行器使用 UUID 隔离夹、私有 Cargo 家目录与 target 目录、环境白名单；
不构建 Fintwind，不启动供应商，不用项目 target 目录，不下载依赖。日志与 JSON
报告可复查。

夹具是名为 `gpui-pre` 的最小路径依赖，不是真实 GPUI 实现。检查覆盖：冷构建与
同目录新鲜构建、仅恢复 `deps`/`build`/`.fingerprint` 后改时间戳不改字节、恢复原
时间戳对照、真实源码编辑必须重建、编译器错误保留原始非零退出码、冷/热稳定版
编译检查、双 vendored 根快照捕获与恢复、各类必须重建与必须不重建的变更、缺失/
畸形/画像不匹配/越界快照与指向包外的 junction。

独立 Windows `Offline cache and logging verification` 作业并行运行该夹具与
`cargo-cache-workspace-blackbox.py`，步骤期限 5 分钟，失败也上传
`cargo-cache-verification-reports`。其夹具数据、Cargo 家目录、target 与独占进程树
与真实编译隔离。

```powershell
python scripts/cargo-cache-workspace-blackbox.py --sysroot '<rustc sysroot>' --output 'C:/Users/Public/Temp/opencode/cargo-workspace-check/result.json'
python scripts/cargo-cache-retention-blackbox.py --output 'C:/Users/Public/Temp/opencode/cargo-retention-check/result.json'
```

工作区套件检查全本地/examples 新鲜度、纯文档提交同一性、改字节留旧 mtime、
locale/SQL/资源/嵌入式 changelog 变更、文件增删改名、构建后编辑、已声明环境
输入、损坏/越界/上下文/profile/时间戳快照、junction、原始编译错误、实时进度、
输出压力与独占超时。保留套件用假 `gh` 可执行文件在私有夹具中调用，绝不登录或
删除用户 GitHub 账户。

CI 用 `cargo-cache-msvc.py` 经 Visual Studio `vswhere`/`VsDevCmd.bat` 激活原生
MSVC 环境，不依赖 Node 动作。按 x64/x64 或 arm64/arm64 选择宿主/目标一致组合，
只捕获 SDK/编译器元数据，校验链接器与库路径后导出到 `GITHUB_ENV`。x64 离线夹具
加 `--require-msvc-env`；运行器只保留 allowlist 的 SDK/工具链变量（`PATH`、
`LIB`、`INCLUDE`、`LIBPATH` 与安装元数据），用户数据与 Cargo 家目录保持私有。
预检确认 `link.exe` 来自 `VCToolsInstallDir` 且库路径已配置，否则在编译前失败，
避免误调 Git 自带的 GNU `link.exe`。

## V3 全工作区输入协议

`cargo-cache-workspace.py` 无扩展名过滤扫描完整目录树：`src`、`crates`、
`vendor`、`assets`、`resources`、`locales`、`db`、`tests`、`examples`、`.cargo`；
同时哈希根清单/lockfile/构建脚本、Rust 工具链文件，以及 `src/app/help.rs`
嵌入的 **CHANGELOG.md**。根文档与 `docs/` 排除在外；在这些已审计根之外新增
编译器输入，必须先纳入再依赖其缓存同一性。

每个文件按路径、SHA-256、大小与模式比较；只有完全未变的才恢复其缓存 mtime。
目录仅在其整个子树一致时恢复 mtime，目录监视器仍能看到增删改名。已变更输入
即使编辑器刻意保留旧 mtime 也赋新 mtime。不跟随链接与重解析点；畸形或上下文
不匹配的快照跳过；不支持的目录树禁用保存。时间戳应用中的 I/O 失败直接失败，
不掩盖部分恢复。这是给不可变 CI 检出用的，不是给正在被编辑的 live 工作区用的。

协议为 `prepare -> restore artifacts -> restore stamps -> capture ->
compile/test -> verify -> save`。快照位于
`target/<profile>/.fingerprint/fintwind-workspace-inputs.json`。校验比较完整输入
列表/字节/模式/mtime、原始输入哈希与兼容哈希；构建期间的源码编辑会阻止保存。

键为 `cargo-build-v3-<OS>-<arch>-<kind>-<context-hash>-<input-hash>`。上下文含
rustc 同一性、绝对工作区/target/Cargo 家目录、所选 profile/编译器/SDK 环境与
CI 缓存自动化。兼容前缀允许源码变更时复用，重建判定仍由 Cargo 决定。不同架构、
测试/发版模式与不兼容上下文不共享前缀。源码提交 SHA 不是同一性：纯文档提交可
复用精确键，但 Tests 仍会运行，发版门禁仍要求精确发版提交的 Tests 成功。

编译器缓存含 `deps`、`build`、`.fingerprint` 与 **examples**，不含增量状态、
安装器、归档与顶层应用二进制。依赖下载缓存另含 registry 源码与 Git 检出，避免
在 Cargo 指纹阶段前重复解压/检出。依赖缓存不加入凭据或 Cargo 配置文件。

只有成功且校验通过的 main 分支构建保存不可变编译器键；PR 与 tag 只恢复不保存。
main 上的手动 Release 可播种发版构建缓存。采用 v3 是一次冷迁移，不要据此推断
稳态性能。

## 保留与免费额度

GitHub 每仓库 10 GB 缓存（不含产物存储），运维软预算 7.5 GB，留出上传余量应对
GitHub 自动驱逐。

Tests 在 main 全绿（或手动 main Release 完成 Windows 构建与同提交检查）后，
有读写权限的维护作业调用 `cargo-cache-retention.py --apply`。它分页走真实 `gh`
API，每个架构/profile 保留最新两代 v3，同架构保留最新 v3 依赖下载缓存；退役
v2 缓存只在同组已有 v3 缓存后删除。未知键与非 main 引用永不删除。另有活动工作流
运行时推迟全部删除，清理中再次检查活跃度。新读者与检查竞态只导致缓存未命中，
不触碰已下载/live 构建目录；GitHub 缓存 API 做不到对未来每个读者硬保证。

不带 `--apply` 时维护只读；API 失败不成空清单。维护作业是非阻塞家务：失败保留
JSON 报告可见，不证伪产品检查。为凑 7.5 GB 不删除受保护的最新代；超预算报告
意味着下次需要实测调整。超大受保护缓存与既有无关缓存仍可能触发 GitHub 自身
10 GB 驱逐；初次迁移也可能在清理前驱逐旧缓存。

## 验收（宣称提速前）

冷跑一次，再用相同提交、相同输入与上下文重复一次。确认热日志中无本地、GPUI 或
example 编译，测试仍执行。再比较纯文档提交与单 crate 变更，确认真实资源/locale/
迁移变更重建其消费者。比较包含恢复/保存与并行校验的完整作业耗时，不只看 Cargo
段。忽略/外部认证 E2E 用例单独记录，不计入已执行测试。缓存命中不等于 E2E 全覆盖。
3-5 分钟热目标是待远程测量的目标，不是已观测结果，也不是对功能变更的承诺。
CI opt-level=0 必须按编译加测试总时间评估。
