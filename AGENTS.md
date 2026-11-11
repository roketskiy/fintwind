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
- 对于新 UI 首先复用 Ely 组件

### Go 界面组件库（必用）
- Go 界面新造控件一律先用成品组件库，禁止手搓基础控件（按钮、菜单、输入框、列表、开关、标签、弹层、提示）。
- 选用顺序：lobe-mygo（`github.com/lobehub/lobe-mygo`，MIT，与 Lobe UI 同设计语言）优先；lobe-mygo 没有的才用 CrychicUI（`github.com/ELY-Open-Source/CrychicUI`，版本固定在 `fintwind-go/go.mod`）；两者都没有才手写，且只允许组装已有原语。
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

## Windows release 构建

dev watcher 占用的是 `target/debug/fintwind.exe`，release 写到 `target/release`，两者不冲突，不必停 watcher。所有渠道只需要应用和 `fintwind-daemon.exe`。应用从自身旁边启动 `fintwind-daemon.exe`。

三渠道规则：GitHub CI 只构建和发布 stable（release.yml 显式 `--channel=stable`，无 beta job/输入/tag/制品，不要新增）；beta 只在本地构建，与 stable 共用生产数据目录（不可同开两版，新 beta 迁移过的数据不保证旧 stable 可读）；dev 是 debug 构建，数据目录 `Fintwind Debug` 隔离。非 stable 构建不执行自动更新检查，beta 靠重新构建更新。stable 与 beta 的 release 产物写同一个 `target/release`，先后构建会互相覆盖，属预期。

### 不带安装包

稳定版只产出两个 release 可执行文件，不打包、不调用 Inno Setup：

```sh
cargo build --locked --release --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon
```

产物：`target/release/fintwind.exe`、`target/release/fintwind-daemon.exe`。

beta 版使用相同的生产数据目录，但不执行自动更新检查：

```sh
cargo build --locked --release --features beta --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon
```

### 带安装包

编译 release，再打便携 zip 和 per-user 安装程序（需本机已装 Inno Setup 6.3+，`ISCC.exe`）：

```sh
bun scripts/bundle-windows.ts
```

默认构建稳定版；个人 beta 包使用：

```sh
bun scripts/bundle-windows.ts --channel=beta
```

等价于 `bun run bundle`。产物都在 `target/release`：

- 稳定版：`fintwind-<version>-<target-triple>.zip`（目录内两个 exe 并排）和
  `fintwind-<version>-<arch>-Setup.exe`
- beta 版：文件名带 `-beta`，目录内仍只有应用和 daemon 两个 exe

未设置 `WINDOWS_CERTIFICATE`（base64 `.pfx`）和 `WINDOWS_CERTIFICATE_PASSWORD` 时打出的是未签名包，脚本会打印说明。细节见 [CONTRIBUTING.md](CONTRIBUTING.md) 的 Windows bundle 与 [RELEASING.md](RELEASING.md)。

## 性能

- 把性能当作产品需求,而不是事后补充。界面在正常交互和渲染时以至少 60 FPS
  为目标。研发渲染功能时,要考虑长文本、多文件等高压力场景,保证加载和交互
  流畅,避免明显卡顿。当更快的方案不牺牲清晰性时,优先选择它;并在假设开销
  可以接受之前先测量。
- 绝不用重活阻塞 UI 线程。渲染独占它,因此一帧能触达的任何数据都必须已在
  内存中:不允许生成子进程、不允许遍历文件系统、不允许网络请求、不允许
  阻塞锁、不允许同步 IPC。
- 行构建器和测量路径在每一帧都会对每个可见条目执行。把从 `render` 可达的
  I/O 视为缺陷——即使它看起来便宜、即使首次命中后有缓存、即使只对部分行
  触发——一次 `git` 调用就已经耗掉好几帧的预算。
- 把工作交给 `cx.background_executor().spawn`,把结果存到 entity 上,完成时
  `cx.notify()`。渲染只读这个存储,未命中表示"尚不可知",必须优雅降级。
- 用一次后台遍历解决整个会话或集合,而不是逐条探测,并用 generation 计数器
  加以保护,使被取代的那一轮的结果不能覆盖更新的状态。
- 一次性用户操作(如点击或菜单命令)在"新鲜度比延迟更重要"时可以同步执行;
  帧处理则不行。
- 让每帧的工作量与屏幕上可见的内容成比例。长集合用 `list()` 做虚拟化,行
  构建器不得重建整个会话的状态;把它提升到每帧只刷新一次的缓存里。
- 流式渲染的刷新率要求高于 60 Hz;设计流提交、pulse clock 滴答和每帧工作量时,
  都要兼顾这一目标与 CPU 开销。在改动事件泵、pulse clock
  (`src/ui/motion.rs`)、veil、悬浮滚动条、pane 缓存或其他任何流式帧会触达的
  东西之前,先读 [docs/performance.md](docs/performance.md);它还记录了真正能
  发现性能回退的、基于计数器的测量方法。

## 无障碍

- 同样把无障碍当作产品需求。GPUI 尚未暴露屏幕阅读器树,所以这里的无障碍指
  键盘可操作性、遵循系统设置、以及可读性——这些都不依赖那个缺失的 API,
  而一旦放任不管,它们都会悄悄退化。
- 鼠标能触达的每个控件,键盘也必须能触达并操作。使用 `track_focus` 配合
  `tab_index`、`tab_group` 和 `tab_stop`,通过 `focus_visible` 给焦点一个可见
  的样式,并支持该控件约定俗成的按键(方向键、`home`/`end`、`enter`/`space`、
  `escape`)。
- 遵循系统的减弱动态效果(reduce-motion)设置。`with_animation` 已经尊重
  `App::reduce_motion`,但直接调用 `window.request_animation_frame` 做装饰性
  动画时,必须检查 `cx.reduce_motion()` 并跳过该请求。
- 绝不只用颜色、悬停或动画来传达含义。状态颜色要配上图标或文字,并且只在
  悬停时显示的内容,键盘聚焦时也必须能触达。
- 保证文字和图标在两种主题下相对其背景都清晰可读,并给可交互目标足够的
  命中区域——扩展命中区域,而不是缩小到字形本身。

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
- 当任务涉及 GPUI 实现——布局与样式惯用法、焦点与按键分发、虚拟化列表、
  菜单与弹层、窗口与平台行为——或当 `src/ui` 内部原语需要一个经过验证的
  原生先例时,以 [Zed](https://github.com/zed-industries/zed) 源码作为参考。
  Zed 是 GPUI 的权威代码库;读它的 crates 而不是 `gpui-component`,并且读
  `Cargo.toml` 中固定的 gpui 版本,使 API 与 Fintwind 构建所用的一致。
- 按关注点划分参考:T3 Code 回答"coding-agent 客户端该做什么",Zed
  回答"一个打磨过的 GPUI 应用该怎么实现",Ely 回答"具体组件可以怎样设计与呈现"。
  按任务需要选用,不要求每次查阅全部参考;局部修复或用户已明确指定的变更,
  不必为了参考而扩大调查范围。
- 把参考当作行为与设计的证据,而不是照抄 Web 特有交互模式或已知 bug 的
  指令。Fintwind 应当保持原生 Windows 惯例。
- 用户明确的截图与反馈,优先于此前的处理方式或仅仅"保持一致"的处理方式。
- 对于 provider 原生内容,如引用、推理和工具事件,验证真实的 provider 载荷
  并保持其顺序。绝不在对话记录中暴露 provider 的私有控制标记。
- 在 dev watcher 管理的刚重新编译、已签名的应用中,针对确切的 provider 交互
  验证可见变更;仅凭一次成功的 Rust 构建是不够的。

## mygo 前端与组件库

`fintwind-go/` 是正式迁移主线，连接 Rust daemon。日用入口为根目录 `MyGo.cmd`，
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

MyGo、lobe-mygo 与 CrychicUI 均由 `fintwind-go/go.mod` 固定版本，使用 Go 模块缓存，
不依赖 `vendor/` 子模块。查阅组件源码时先按该版本定位模块缓存，不使用旧原型中的 API。
