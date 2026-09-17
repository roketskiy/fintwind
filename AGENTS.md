# Fintwind 开发指南

## 语言
- 使用中文交流

## 开发运行时

- 假定 `bun ./scripts/dev.ts` 已经在运行,并且持有当前的 `fintwind.exe`
  进程。源码变更会被自动重新编译并重启。只有在它尚未启动时才需要你自己运行。
- 除非被要求,否则不做视觉测试。
- 禁止写不必要的的注释，例如用户说过的话，会话内容等。
- 非必要不写小的测试。
- 开始写代码前对用户的命令有不理解的地方和要补充的及时提问

## 代码审查
- 单论超过三十行的代码修改时，check完成后调用调用一个子代理审查代码，并修复功能性bug和重大漏洞,然后提问用户是否编译release，以及如何编译release。

## Windows release 构建

dev watcher 占用的是 `target/debug/fintwind.exe`，release 写到 `target/release`，两者不冲突，不必停 watcher。两个可执行文件必须放在同一目录：应用从自身旁边启动 `fintwind-daemon.exe`。

### 不带安装包

只产出两个 release 可执行文件，不打包、不调用 Inno Setup：

```sh
cargo build --locked --release --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon
```

产物：`target/release/fintwind.exe`、`target/release/fintwind-daemon.exe`。

### 带安装包

编译 release，再打便携 zip 和 per-user 安装程序（需本机已装 Inno Setup 6.3+，`ISCC.exe`）：

```sh
bun scripts/bundle-windows.ts
```

等价于 `bun run bundle`。产物都在 `target/release`：

- `fintwind-<version>-<target-triple>.zip`（目录内两个 exe 并排）
- `fintwind-<version>-<arch>-Setup.exe`

未设置 `WINDOWS_CERTIFICATE`（base64 `.pfx`）和 `WINDOWS_CERTIFICATE_PASSWORD` 时打出的是未签名包，脚本会打印说明。细节见 [CONTRIBUTING.md](CONTRIBUTING.md) 的 Windows bundle 与 [RELEASING.md](RELEASING.md)。

## 性能

- 把性能当作产品需求,而不是事后补充。Fintwind 是一个与 Web 客户端竞争的
  原生应用,在高刷新率屏幕上面对长对话记录仍保持流畅,正是"原生"的意义所在。
  当更快的方案不牺牲清晰性时,优先选择它;并在假设开销可以接受之前先测量。
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
- 流式渲染的 CPU 由两个节奏决定——流提交 ≤ ~8.3 Hz、pulse clock 滴答
  ≤ ~30 Hz——以及一帧能看到的内容。在改动事件泵、pulse clock
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
- opencode文档地址（https://opencode.ai/v2/docs)
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
- 按关注点划分这两个参考:T3 Code 回答"coding-agent 客户端该做什么",Zed
  回答"一个打磨过的 GPUI 应用该怎么实现"。对两者都要同样克制——局部修复
  或用户已明确指定的变更,不要去翻参考源码。
- 把参考当作行为与设计的证据,而不是照抄 Web 特有交互模式或已知 bug 的
  指令。Fintwind 应当保持原生 Windows 惯例。
- 用户明确的截图与反馈,优先于此前的处理方式或仅仅"保持一致"的处理方式。
- 对于 provider 原生内容,如引用、推理和工具事件,验证真实的 provider 载荷
  并保持其顺序。绝不在对话记录中暴露 provider 的私有控制标记。
- 在 dev watcher 管理的刚重新编译、已签名的应用中,针对确切的 provider 交互
  验证可见变更;仅凭一次成功的 Rust 构建是不够的。
