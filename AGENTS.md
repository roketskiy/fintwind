# Fintwind 开发指南

## 语言
- 使用中文交流

## 开发运行时

- 除非用户要求，不然不使用 dev watcher。
- 除非被要求,否则不做视觉测试。
- 开始写代码前对用户的命令有不理解的地方和要补充的及时提问
- 
## development:  
- Tautological tests considered harmful.  
- Change-detector tests considered harmful.  
- Do not create regression tests for bug fixes without a genuine gap in behavior testing.

## UI
- 新 UI 优先复用下述 Go 组件库。

### Go 界面组件库（必用）
- Go 界面新造控件一律先用成品组件库，禁止手搓基础控件（按钮、菜单、输入框、列表、开关、标签、弹层、提示）。
- 选用顺序：lobe-mygo（`github.com/lobehub/lobe-mygo`，MIT，与 Lobe UI 同设计语言）优先；lobe-mygo 没有的才用 CrychicUI（`github.com/ELY-Open-Source/CrychicUI`，版本固定在 `go.mod`）；两者都没有才手写，且只允许组装已有原语。
- 动手前先读对应组件的 skills 页（`https://mygo.lobehub.com/skills/components/<name>.md`），只用文档记载的 props、variant 和 import。
- 禁止拿 CrychicUI `button.TextButton` 当次级文字按钮：它是 accent 色，只适合主操作。次级动作用 lobe `action.Button` 的 Text/Default 变体或同等 muted 样式。
- 不调用 lobe `theme.Apply`：它整帧生效且跟随系统外观，会扰乱同帧其他视图。lobe 组件在不调用时自动跟随窗口 Dark 标志渲染；与自有 palette 混用时接受视觉差，只保证功能与状态一致。
- 引入或升级组件库导致 MyGo 版本变动时（大版本，或 0.x 的小版本变动），先读新版 `docs/ui/migration.md`，跑 `migrate-ui` 预览，构建与 vet 全绿才继续。新增开源依赖按文档规范在 `docs/licenses/` 补充许可声明。

### 图标
- 新增图标优先从 [Lucide Icons](https://lucide.dev/icons/) 获取对应 SVG，不添加 Lucide 依赖或运行时图标包。
- 将所需 SVG 纳入 `assets/icons/`，沿用项目现有的嵌入式资源、主题着色和图标渲染机制，并保留所需许可证与来源声明。
- 已有合适图标时优先复用，不为统一来源替换现有图标。

## network
- 当发现有网络连接问题时，走7897端口代理重试


## 网页移动端UI设计
- 禁止做大圆角药丸按钮
- 布局紧凑、平衡，从用户角度思考布局设计是否合理
- 优先考虑性能和速度
- 禁止使用渐变紫做背景色
- 延续桌面端的设计风格
- 不要做全平铺设计，通过明暗、间隔、图形、颜色等方式做出层次区分。
- 风格克制，质感和体验优先

## worktree
- 新建worktree放在项目的同一个父文件夹下
-
## 文档
### 存放
- Git 只放正式介绍/说明文档（`docs/` 根，另有 `licenses/`、`media/`）。
- 过程稿（bug 记录、迁移评估、plan、acceptance、failure-matrix、阶段验证）一律写 `docs/dev-notes/`，该目录已 ignore，不提交。
- `.zcode/plans/` 本地保留，不提交。
- `docs/` 根新增/删除/移动正式文档需先确认；移动或删除时同步修复 README、CHANGELOG、RELEASING 与源码注释中的引用，不留死链。
### 写作
- 中文说明体，句子通顺；面向用户/贡献者讲行为、用法、边界。
- 实现写到模块级（crate、模块、脚本、协议）为止，不展开到函数签名级。
- 用到的开源项目要点名并注明许可，许可文件放 `docs/licenses/`。
- 不写过程内容：验证记录、失败用例表、日期验收、“本次”口吻。
- 只引用 git 跟踪中的文件，不引用 `docs/dev-notes/`。
## 测试
- 禁止写单元测试，只做e2e测试
- 测试不得终止用户或其他会话的进程。禁止按名称、监听端口、命令行子串或目录匹配杀进程；端口冲突应另选随机端口或失败退出。
- 清理只允许本轮创建并持有句柄的进程或本轮独占的受控作业。PID、父 PID、名称或路径单独不能证明所有权；不得通过全机扫描补认领孤儿进程。
- 运行前须确认独立数据目录、环境白名单和子孙进程容纳边界。无法证明安全时停止并报告，不扩大终止范围。安装版 Fintwind、daemon、remote-host 和 OpenCode 服务不得作为清理目标。

## UI文案
- 禁止禁止禁止在UI界面能看到的地方加上无意义的说明文字，包括但不限于对简单易辩的UI做说明、标签。每一个字都要思考是否简洁正式。
- 不要给含义明确的按钮和控件添加重复的悬停提示；仅在确有必要补充信息时使用 tooltip。无障碍名称使用 `Label`/`Description`，注意 Lobe `ActionIcon` 的 `Title` 会自动显示 tooltip。

## Windows release 构建

正式版本使用 Go 应用和 Go daemon，Rust 源码保留作为参考，不再构建和维护。
版本号保存在根目录 `VERSION`。发布包包含并排的 `fintwind.exe` 和
`fintwind-daemon.exe`；应用启动同目录的 daemon。

在仓库根目录构建两个可执行文件：

```powershell
go build -mod=readonly -ldflags=-H=windowsgui -o target/release/fintwind.exe ./cmd/app
go build -mod=readonly -o target/release/fintwind-daemon.exe ./cmd/daemon
```

在仓库根目录运行 `bun scripts/bundle-windows.ts` 构建 stable 安装包和便携包；
使用 `--channel=beta` 构建本地 beta 包。GitHub CI 只发布 stable。两个渠道
共用生产数据目录，不得同时运行。打包需要 Inno Setup 6.3 或更新版本；未配置
签名证书时产物不签名。具体发布步骤见 [RELEASING.md](RELEASING.md)。

## 性能

- 正常交互以至少 60 FPS 为目标；流式渲染同时控制刷新延迟和 CPU 开销。
- MyGo view、行构建和测量路径只读内存，不执行文件、网络、子进程或同步 IPC。
- 耗时工作交给 goroutine，完成后通过 `Window.Update` 发布；用取消与 generation
  拒绝过期结果。工作线程不保存或访问 `ui.Context`、`ui.Element`。
- 长列表使用 `ui.List` 虚拟化；缓存集合级派生状态，避免逐行重复遍历。
- 性能相关改动先读 [docs/performance.md](docs/performance.md)，实际测量后再判断开销。

## 无障碍

- 鼠标能操作的控件也必须支持键盘，保留可见焦点、约定按键及足够的命中区域。
- 使用 MyGo 组件的焦点和快捷键机制；持久引用使用 `ui.Handle`，不保存帧内元素。
- 遵循系统减弱动态效果设置，不只用颜色、悬停或动画表达状态。
- 深浅主题中的文字与图标都应清晰可读。

## 产品参考
- 有关opencode服务器的api协议在opencode2文档，地址（https://opencode.ai/v2/docs)
- opencode v2的仓库地址是（https://github.com/anomalyco/opencode/tree/v2）
- opencode v2的更新日志（https://opencode.ai/changelog）
- 当任务涉及 coding-agent 工作流、信息层级、控件、工具活动或对话记录的
  呈现,且这种对比能切实澄清一个模糊的产品决策时,以 GitHub 上的
  [T3 Code](https://github.com/pingdotgg/t3code) 源码作为参考;或者当用户明确
  要求对比时。
- 局部 bug 修复、直接的视觉修正、原生平台行为、或用户已经明确指定的变更,
  不要去翻 T3 Code。当 T3 Code 确实相关时,检查它当前的应用或源码,而不是
  依赖旧的截图或记忆。
- MyGo 的 API 与生命周期以 `go.mod` 固定版本的文档和模块缓存源码为准。
  组件使用 Lobe MyGo、CrychicUI 的对应版本文档；不要从 Rust/GPUI 归档复制 API。
- 把参考当作行为与设计的证据,而不是照抄 Web 特有交互模式或已知 bug 的
  指令。Fintwind 应当保持原生 Windows 惯例。
- 用户明确的截图与反馈,优先于此前的处理方式或仅仅"保持一致"的处理方式。
- 对于 provider 原生内容,如引用、推理和工具事件,验证真实的 provider 载荷
  并保持其顺序。绝不在对话记录中暴露 provider 的私有控制标记。
- 涉及 provider 可见行为时，使用隔离启动器和确切交互验证；构建成功不能替代行为验证。

## mygo 前端与组件库

仓库根目录是正式主线，包含 Go 应用和 Go daemon。源码日用入口为根目录 `MyGo.cmd`，
直接使用真实 Fintwind 数据；未获用户明确许可，agent 不得运行该入口（`--plan` 除外）。
用户已授权（2026-11-11）：每轮 mygo 改动完成后，agent 用
`pwsh -File scripts/mygo-restart.ps1` 重启日用实例——脚本只停止 `--profile daily`
且根在本仓库的 launcher（其监督进程以 kill-on-close Job 拆掉 app 与 daemon），
再拉起新实例；launcher 每次启动都会重建 app，重启即生效。不得绕过该脚本直接
杀进程，也不得停止 `--profile agent` 或其他会话的实例。
Agent 开发使用 `MyGo-Agent.cmd --name <task>`；只构建加 `--build-only`，
只查看路径加 `--plan`，无窗口启动验证加 `--verify --daemon <absolute-path>`。
不得通过直接运行 app 或修改环境变量绕过启动器的隔离、凭据白名单与数据锁。
独立环境不是沙箱，自动测试只能添加测试项目，不导入真实凭据或发起真实付费请求。
启动器只清理自身持有 Job 的进程，不关闭已安装 Fintwind 或其他会话的进程。
用法和边界见 [CONTRIBUTING.md](CONTRIBUTING.md) 的 MyGo 章节。

MyGo、lobe-mygo 与 CrychicUI 均由 `go.mod` 固定版本，使用 Go 模块缓存，
不依赖源码仓库中的 vendor 副本。查阅组件源码时先按该版本定位模块缓存，不使用旧原型中的 API。

## 仓库边界

- `cmd/` 只放 app、daemon、mygo；验证和维护命令放 `tools/`。
- `legacy/rust/` 只归档旧源码和必要许可，不增加 Cargo 清单、锁文件或构建入口。
- 根 Go 模块名仍为 `fintwind-go`，它是 import 前缀，不是磁盘目录。
- `db/migrations/` 是数据库迁移的权威输入；修改后运行生成脚本及其 `--check`。
