# Changelog

All notable changes to Fintwind. This file is the **source of truth for the
release notes published with each GitHub release**: the Release workflow
extracts the section whose heading matches the version being released and
publishes it as the release body.

Format follows [Keep a Changelog](https://keepachangelog.com). Add a new
`## [<version>]` section at the top for each release, matching the version in
`Cargo.toml`.

Fintwind versions are independent of [waku](https://github.com/egoist/waku).
Do not copy upstream version numbers into this file.

Write release notes for the final product users receive, not the development
history. When a feature is still unreleased, fold its fixes and refinements into
the original feature bullet instead of adding separate entries for them.

## [unreleased]

## [0.3.3]

```
+----------------- [ 0.3.3 ] ------------------+
| opencode 2  支持到 2.3.2                     |
|                                              |
| +  added    提示词脱敏、单提交更改查看       |
| ~  changed  补全卡片、回复尾标、会话卡片等   |
| *  fixed    子智能体问答、压缩分组等         |
| -  removed  内置浏览器                       |
|                                              |
+----------------------------------------------+
```

### 新功能

- 提示词脱敏：默认开启。检测请求内容中的凭据与邮箱，在发给模型前替换为引用；输入框与会话历史中的用户消息高亮检测到的内容，发送前可查看。模型回复、思考、工具结果、压缩摘要与恢复的历史按原值还原显示，引用可跨轮复用；工具执行时参数还原为原值（shell、Code Mode 与子智能体任务除外）。「设置 → 通用 → 上下文管理」可关闭。凭据检测规则改编自 Gitleaks 与 CredSweeper，许可见 [docs/licenses/gitleaks.txt](docs/licenses/gitleaks.txt) 与 [docs/licenses/credsweeper.txt](docs/licenses/credsweeper.txt)（[e3db19f6](https://github.com/roketskiy/fintwind/commit/e3db19f600cdd3ea59ccd4e21ac28bb88dd5b0df)、[d1adaae3](https://github.com/roketskiy/fintwind/commit/d1adaae3626b3b75148b949412593b3309c0dc49)、[095bbc82](https://github.com/roketskiy/fintwind/commit/095bbc821cdc045c63cf9a8b1ca37c999d11b2fc)、[4117dc16](https://github.com/roketskiy/fintwind/commit/4117dc16db5deae12022ec0de8cc66c6e3e97865)）。
- 提交历史支持查看单个提交的更改：提交详情面板新增查看更改入口，审阅页以只读 diff 展示该提交相对第一父提交的更改，根提交对空树、合并提交按第一父，与历史面板统计口径一致（[cf4055bc](https://github.com/roketskiy/fintwind/commit/cf4055bc6ef55120401fd8f68fed0f189cb84241)）。

### 改进

- 输入框 `/` 与 `@` 补全卡片分「最近 / 命令 / 技能 / 文件」分区浏览，空查询时可用 ←→ 切换分区、↑↓ 选择、Enter/Tab 添加、Esc 关闭，显示加载与空状态；输入邮箱不再误触发文件补全（[0ac0800a](https://github.com/roketskiy/fintwind/commit/0ac0800a4836cfdba2982f2de75f5022f8502000)、[b505ef00](https://github.com/roketskiy/fintwind/commit/b505ef0062d05748af10a5eefacde2b31bc06d44)、[a01c8e2b](https://github.com/roketskiy/fintwind/commit/a01c8e2b0885a13315d5b36f6db694c215f953c1)）。
- 回复尾标在模型名称后显示本次使用的思考程度（[1698b7b4](https://github.com/roketskiy/fintwind/commit/1698b7b479a6ad7fc8e832df97a114acf50a7a85)）。
- 侧边栏会话卡片去掉供应商标识头像；运行中的会话改在第二行行首显示强调色光环，后接时间与分支，无时间与分支的新会话也能显示运行中（[da00f188](https://github.com/roketskiy/fintwind/commit/da00f1888c9253bee92b450d9f40bf1df272f727)）。
- 活动卡片展开与收起加入 200 毫秒缓动动画（[a116262d](https://github.com/roketskiy/fintwind/commit/a116262d9cb8f009681bbe4185da110d5017afad)、[efe384ea](https://github.com/roketskiy/fintwind/commit/efe384ea2275ddabc8f281cdbe628e5f64fd22aa)）。
- 后台工作面板中未运行的子智能体折叠行改用次级文字色，不再比区块标题更醒目（[dd3eeafc](https://github.com/roketskiy/fintwind/commit/dd3eeafcc4a43ede7bb1fc4bb25174335a27b40c)）。

### 修复

- 子智能体的提问与权限请求现在转发到会话中处理，卡片显示发起的代理身份，多个请求按序排队（[09f53e9d](https://github.com/roketskiy/fintwind/commit/09f53e9df3a83d09b8a3b14e4d45b5cf7bb40a98)）。
- 修复压缩摘要与后台回复的分组：压缩摘要正确折叠进对应回合，后台任务完成后的自动续跑作为新回复展示，不再并入上一条回答（[3bf41a2f](https://github.com/roketskiy/fintwind/commit/3bf41a2f5b72e9e608c2a81b3fb681bdb4aba844)）。
- 修复会话胶囊打开的提交卡片；输入框的右键菜单不再被卡片遮住（[c1d693b4](https://github.com/roketskiy/fintwind/commit/c1d693b4e96a8e7f5de85700276f296140d31fbb)）。
- 修复审阅区提交按钮与 SSH 管理页搜索框的文字显示问题（[a4827b6a](https://github.com/roketskiy/fintwind/commit/a4827b6afb25ce761fc0e6658d60e9bf5b8d00fd)）。
- 去掉设置页下滑时出现的边框线（[f302ed6c](https://github.com/roketskiy/fintwind/commit/f302ed6c3c2d5a4917dabc1e8a4d0e88123d9c27)）。

### 移除

- 移除内置浏览器及其浏览器工具。网页链接与登录仍通过系统浏览器打开；「设置 → 通用 → 功能」新增「OpenCode 官方浏览器工具」入口，可访问官方 Browse 产品页——以 MCP 服务器加浏览器扩展的方式让 Agent 使用你真实的浏览器（[186c3230](https://github.com/roketskiy/fintwind/commit/186c323072e6ef1a0e0c1e7d1aa6fd2dd95c9396)、[97a5274f](https://github.com/roketskiy/fintwind/commit/97a5274f9ae527c17f1cea749276a5142981859d)）。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x，本版支持到 OpenCode 2.3.2。

## [0.3.2]

```
+----------------- [ 0.3.2 ] ------------------+
|                                              |
| opencode 2  支持到 2.3.2                     |
|                                              |
| +  added    SSH 远端工作区、重复输出保护     |
| ~  changed  原生压缩、用量流向、离线模型表等 |
| *  fixed    配置读取、粘贴与右键等           |
| -  removed  图像生成启用开关                 |
|                                              |
+----------------------------------------------+
```

### 新功能

- SSH 远端工作区：通过 SSH 私钥连接 Linux 服务器（x86_64/ARM64），像本机一样使用服务器上的项目和会话，本机与远端任务并行，断线重连或桌面升级后恢复远端会话。Fintwind 自动在服务器上安装和更新所需后端，所有通信走 SSH 加密连接，服务器无需开放额外端口。「设置 → SSH 连接」新建、编辑、检查与移除连接；首次连接记住主机指纹，指纹变化时阻止连接；移除连接不影响服务器上的项目（[3094bb67](https://github.com/roketskiy/fintwind/commit/3094bb67743747428a9b536180f2bea672ca9d7b)）。
- 侧边栏底部的主机切换器在本机与各服务器间切换。「添加项目」可先选择服务器，再按现有文件夹、GitHub 克隆或新建文件夹添加；克隆显示实时进度，失败后保留输入可重试，已存在的目录不会被覆盖（[790442b0](https://github.com/roketskiy/fintwind/commit/790442b01732a4f13e288f1459c84e4acc1f6dc5)、[7a4361bb](https://github.com/roketskiy/fintwind/commit/7a4361bb3791109fa835f0597d5fb5d30eb873bb)）。
- 设置侧栏底部的机器选择器切换设置归属：通用设置、提供商、MCP 与技能作用于所选服务器，可检查、安装、更新服务器上的 OpenCode，远端 MCP 登录在本机浏览器完成；断线时服务器设置只读。外观与 SSH 连接设置始终归本机（[27bdc9b2](https://github.com/roketskiy/fintwind/commit/27bdc9b2ae3710e5d67fc782f9b2ce150b14a0c2)、[8753249f](https://github.com/roketskiy/fintwind/commit/8753249ff6bdbe85a04c98fdbabce45f31b303ae)）。
- 新增重复输出保护：模型回复或工具调用及结果连续 10 次完全相同时自动停止任务，默认开启，可在通用设置关闭（[260c1246](https://github.com/roketskiy/fintwind/commit/260c1246bb60fdf193469ff7a9a7ecb9ff6e642d)）。

### 改进

- 上下文压缩改由 OpenCode 原生自动与后台压缩执行：阈值按模型输入窗口占用百分比设置，工作区或自定义 OpenCode 配置优先；后台压缩在接近阈值前提前生成摘要，需要 OpenCode 2.3.1 或更新版本；两者默认开启（[7bcac82b](https://github.com/roketskiy/fintwind/commit/7bcac82be739d753e1fcccb009a07f9683e993aa)）。
- 用量页重新设计：用量流向图将左侧模型份额与右侧全量构成（缓存读取、输入、输出、缓存写入、推理）汇入同一总量，点击可高亮对应流向；订阅额度以轮播卡片同页展示；单日统计提供按小时图表并可逐日切换，时间范围默认 7 天，另有单日、30 天与全部（[7a92f4c4](https://github.com/roketskiy/fintwind/commit/7a92f4c424d5f9b830442677429e67ca4c6e5d6f)）。
- 内置 models.dev 精简模型表：仅保留知名 AI 实验室官方条目与两年内发布的模型，字段精简为名称、上下文、输入输出模态与思考档位；运行时离线读取模型与供应商名单，不再联网拉取 models.dev，旧磁盘缓存仅供用量页读取历史价格（[57cc5f45](https://github.com/roketskiy/fintwind/commit/57cc5f45a97b1537bdc41e2b2b6951111758a8f2)）。
- 自定义模型支持思考程度多选（none/low/medium/high/xhigh/max），留空跟随目录档位或标准六档（[d26c2441](https://github.com/roketskiy/fintwind/commit/d26c24411ef5bac464aea9d2d5b2a780eddb6005)）。
- 移除图像生成工具启用开关，工具可用性随 OpenCode 版本与已连接图像模型；通用设置仅选择默认图像模型，「自动」由 OpenCode 按请求选择可用模型（[0ed05038](https://github.com/roketskiy/fintwind/commit/0ed050383da42cec48cf35571180ffd1a301683e)）。
- 后台工作面板列出后台进程与子智能体，显示子代理引导消息与运行时长；未运行的子智能体默认折叠；后台任务中间输出不再触发会话未读（[1e8ea49c](https://github.com/roketskiy/fintwind/commit/1e8ea49cc3462da81f68fc0d9ce790d88f5c4e7a)）。
- 会话错误改为持久化活动卡片；首字延迟改读 OpenCode 存储的轮末计时；活动组显示本组耗时与步数，时间戳不完整时保留计时；Build/Plan 模式固定蓝/橙配色（[c4f8eb0f](https://github.com/roketskiy/fintwind/commit/c4f8eb0fef32037e52c9114076770c8b447a4436)）。
- 会话与右面板标签栏改为 Chrome 造型；主工作区改为双层面板，卡片底部与右侧内缩并圆角；常规与外观设置改为分组卡片行式排版；统一加载延迟显示与操作旋转动效；选项变更与供应商连接测试改用 toast 提示；应用内更新日志改用时间线展示，更新提示调整样式与显示时机（[6658e538](https://github.com/roketskiy/fintwind/commit/6658e53819252c6d1f09a05c006c6930a286a9d6)、[3234d2e0](https://github.com/roketskiy/fintwind/commit/3234d2e09f396e5e6307f04d911775b05f2f16fd)、[ef8a258b](https://github.com/roketskiy/fintwind/commit/ef8a258b98ada9865f2b1943d5c946288157dfd9)、[6b36e348](https://github.com/roketskiy/fintwind/commit/6b36e3488fb701a461d85940dab12dadab9307c9)、[ebf9e040](https://github.com/roketskiy/fintwind/commit/ebf9e040d751f3736f057882b0f960a6be32d81e)、[0d801ddc](https://github.com/roketskiy/fintwind/commit/0d801ddc5ba3190bda0dced3bd53201069da1349)、[7c12dc03](https://github.com/roketskiy/fintwind/commit/7c12dc03fd7f61dbedc17ff4142e76bb3ade9643)）。
- 源码管理与分支历史体验改进：分支切换失败保留现场并支持重试，提交对话框与历史加载增加延迟显示，调整会话胶囊 Git 工具布局（[84751b74](https://github.com/roketskiy/fintwind/commit/84751b744233b87dcb1bda432ace1a38a49b66fd)）。
- 会话输入行布局、提交动作与附件加载改进，文件拖放归属跟随目标会话；粘贴消息保留换行与空白（[a38e72df](https://github.com/roketskiy/fintwind/commit/a38e72df4b7a2ffca7cea067f754797049c4b575)、[b74f3127](https://github.com/roketskiy/fintwind/commit/b74f3127275182b8f6525cb65b8f3472f6ba0cf5)）。
- 手机远控改善加载与上传反馈（[f3f9b530](https://github.com/roketskiy/fintwind/commit/f3f9b530234789d890aeb366331a34f87d80e216)）。
- 定时任务创建时记录 IANA 时区并按该时区执行；未带时区的旧任务继续按本地时间计算（[d008436f](https://github.com/roketskiy/fintwind/commit/d008436fa797150ecd5b9b583fbe6149315256e5)）。
- 提供商与用量设置改经 daemon 读写，连接测试、MCP 登录与列表、配额刷新等补齐加载状态与键盘操作支持（[5dc66c62](https://github.com/roketskiy/fintwind/commit/5dc66c620f3014472633bdec84d1f9055a742f79)）。

### 修复

- 修复 OpenCode 配置读取问题（[ea9dc658](https://github.com/roketskiy/fintwind/commit/ea9dc6585cec20b9d8a7fc747a5c51be8d6fa862)）。
- 修复终端粘贴焦点与末行显示，以及右键误触发文本链接（[efb18d2a](https://github.com/roketskiy/fintwind/commit/efb18d2a6ba0a6e52008eb8617fdacd61edb2bf4)、[4ed4d88a](https://github.com/roketskiy/fintwind/commit/4ed4d88ab89933dd647e7c548b9147fd03ea4a31)）。
- 修复订阅卡片边框裁切（[bbe1b677](https://github.com/roketskiy/fintwind/commit/bbe1b67765182e079a0eb87f84ad1e8a9a85f50b)）。
- 修复加载按钮文字对齐（[86ed08de](https://github.com/roketskiy/fintwind/commit/86ed08def73377ed49dc3b9c98dab238c3f4ad59)）。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x，本版支持到 OpenCode 2.3.2。

## [0.3.1]

### 新功能

- 通用设置的运行环境支持检查 OpenCode 版本，并通过 npm 或 Bun 安装、更新 OpenCode。应用内更新仅适用于当前 npm 或 Bun 管理的安装；便携版等其他安装方式仍需自行更新。更新不会重启正在运行的 OpenCode 进程（[d339f8cf](https://github.com/roketskiy/fintwind/commit/d339f8cfbfa4f2d8400ad7519609c1baeb7150e1)）。

### 改进

- 用量页分为「用量统计」和「订阅额度」：统计页展示 Tokens、趋势和模型、提供商、项目排行，不再展示费用估算；额度页集中显示已连接且受支持的供应商，包括本机 Claude Code、Codex 订阅及 OpenCode Console Go。不同页面独立刷新，额度不受统计日期影响，凭据失效或查询失败时显示对应状态。支持范围和数据来源见 [供应商额度说明](docs/provider-quota.md)（[aa2458b7](https://github.com/roketskiy/fintwind/commit/aa2458b708901325b22612db648614ef722d256d)）。
- 浏览器工具改用 OpenCode 实验性客户端托管 MCP，截图以 MCP 图像内容发送给模型；开关仍默认关闭。旧版 OpenCode 或未启用工具时，普通对话不受影响（[f490e5fd](https://github.com/roketskiy/fintwind/commit/f490e5fd1f70eb94d7d6c2230b05429763e5012e)）。
- 桌面端与手机端的「跟随系统」改为「自动」：按各设备本地时间，7:00 起使用浅色，19:00 起使用深色，不再跟随系统外观。原有「跟随系统」设置会沿用为「自动」，也可手动选择浅色或深色（[8bee5cf2](https://github.com/roketskiy/fintwind/commit/8bee5cf2c6f79a4fe29b27945cf13d288332110e)）。
- 统一卡片表面与顶部高光，调整定时任务条目和启停状态标签（[44f7c8be](https://github.com/roketskiy/fintwind/commit/44f7c8be78089557468b0a904733c2852a44c4e9)、[ee065bb2](https://github.com/roketskiy/fintwind/commit/ee065bb290e81aa6cb8a92a1bf58132a2d7e6c48)）。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。浏览器 MCP 与定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。图像生成需要 OpenCode 2.2.1 或更新版本及兼容的图像模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用。未验证完整的模型或版本兼容区间，详见 [Goal 说明](docs/goals.md)和[定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

## [0.3.0]

### 新功能

新增手机远程控制：从桌面侧边栏开启后，扫码或打开配对链接，即可在手机网页切换工作区、查看和新建会话、发送消息、停止任务、调整会话权限、处理权限审批与回答问题（[0531afbe](https://github.com/roketskiy/fintwind/commit/0531afbeb73f0ef9dd23c91c0775ce82798fa72f)、[df1449a9](https://github.com/roketskiy/fintwind/commit/df1449a9dd46d808b752b5ca28e5c4e77e2d68ca)、[74423b20](https://github.com/roketskiy/fintwind/commit/74423b204a52581808f6e34766e18e380300dc3e)）。输入框支持模型、Build/Plan 与思考强度选择、斜杠命令、图片和小文件附件，也可在模型运行时发送引导消息，收到接收确认后才清空草稿，失败时保留内容（[6423f6da](https://github.com/roketskiy/fintwind/commit/6423f6dafd43c4abce58840ae5c0e10a35f71c0d)、[854fbf5a](https://github.com/roketskiy/fintwind/commit/854fbf5afb72d0b25567b8a40406f23ea4b3cd71)、[ef218f6a](https://github.com/roketskiy/fintwind/commit/ef218f6a19805adad2014b46df057da7d76b2623)）。对话支持 Markdown、公式、Mermaid 图表与代码高亮，按顺序展示思考和工具活动，可展开查看思考及活动列表；提供 Todo 与 Goal 状态、消息时间和回合耗时，支持实时增量更新与滚动跟随（[fda2598a](https://github.com/roketskiy/fintwind/commit/fda2598ace87a1ad2a7717c38dcd8e0993d47abf)、[9054e02e](https://github.com/roketskiy/fintwind/commit/9054e02ef2571524ac515003cbb8e2eb8d8dedf2)、[b9b8c469](https://github.com/roketskiy/fintwind/commit/b9b8c46966fafc76761afc603fe19ee586d1370a)、[d24dc943](https://github.com/roketskiy/fintwind/commit/d24dc943becf4003a01d0682dfa16fab221e17ac)）。连接使用端到端加密，无需账号登录，同一浏览器可记住配对（[22e91c89](https://github.com/roketskiy/fintwind/commit/22e91c89f5e3e86346df643a27bb9de31c2d9a74)）。使用时桌面须保持运行，仅开放启用时已登记的工作区及最近 72 小时内更新的会话；关闭远控、退出桌面或重新配对后，旧授权失效。详情见 [手机远控说明](docs/remote-control.md)。

### 改进

- Git 历史改用多分支泳道图，展示本地分支、远程分支、标签、当前检出位置与合并节点。分支保持固定列位与连续连线，当前检出分支的主线固定在首列，配色保持稳定；排除 Fintwind 内部快照引用，避免历史图被无关记录挤满（[a10058cd](https://github.com/roketskiy/fintwind/commit/a10058cdef9900abb3fd1f73f2f1a2a813f29d39)、[4e80f58a](https://github.com/roketskiy/fintwind/commit/4e80f58a70619f7e1b97a5dc465e96fe976961a6)、[111048d0](https://github.com/roketskiy/fintwind/commit/111048d0b314634231b9233a7adc9eac5ea9a449)、[40fad9ba](https://github.com/roketskiy/fintwind/commit/40fad9ba0b5d3d9caabccdef76827f298b275c2b)）。
- 审阅页新增卡片式提交框，输入区带独立边框并随内容增高，支持 Ctrl+Enter 提交已暂存更改。主按钮按仓库状态显示提交、拉取或推送，右侧下拉菜单与主按钮连为一体，提供提交并推送、全部暂存并提交、拉取、推送与获取等操作，移除原底部操作栏（[a10058cd](https://github.com/roketskiy/fintwind/commit/a10058cdef9900abb3fd1f73f2f1a2a813f29d39)、[4e80f58a](https://github.com/roketskiy/fintwind/commit/4e80f58a70619f7e1b97a5dc465e96fe976961a6)、[7e4dc7cb](https://github.com/roketskiy/fintwind/commit/7e4dc7cb3e24b83766b661d228d57b4fbd0e45cc)）。
- 内置浏览器移除逐页授权和会话隔离：启用浏览器工具后，所有会话均可操作已打开的标签页，包括手动打开的页面，计划与构建模式不再区别处理。支持本机文件页面和密码框操作；页面快照可包含密码、隐藏字段及其他表单值（[beb28ae2](https://github.com/roketskiy/fintwind/commit/beb28ae2f439e07f7f3791e504846d4fe6bd4a79)）。
- 图像模型后台检查失败时延迟重试一次，不再因启动期间服务尚未就绪弹出错误；用户主动调整相关配置后，重试仍失败时才显示提示（[6534e8e4](https://github.com/roketskiy/fintwind/commit/6534e8e4acb583075cee15d0bd4246d6826bdfa4)）。
- 卡片保留原有背景色，增加顶部高光与分层阴影；新会话的 ASCII 背景延伸至工作区底部，减少边缘断层（[26679b71](https://github.com/roketskiy/fintwind/commit/26679b717ed81483a831c95fd65affd293e1ef54)、[34eaa052](https://github.com/roketskiy/fintwind/commit/34eaa05292f918a4474d5cad198bc852f05dff9c)、[d1f69304](https://github.com/roketskiy/fintwind/commit/d1f693048203218d0bf1007859df8a8876b07946)）。
- 图表图片缓存容量上限由 32 MiB 提高至 256 MiB，最多保留 64 项，减少较大图表被反复移出缓存的情况（[4496a5d7](https://github.com/roketskiy/fintwind/commit/4496a5d7dd5694f35d61dcb4a2786f0845fb48f6)）。

### 修复

- 修复图表全屏预览中缩放比例变化但图像大小不变的问题，并支持触控板双指缩放（[b437afaa](https://github.com/roketskiy/fintwind/commit/b437afaaba52b28692edbc8494629aab68751751)）。
- 修复多行独立公式被 Markdown 块解析拆分，导致公式显示异常的问题（[4fc01a97](https://github.com/roketskiy/fintwind/commit/4fc01a97415a506404f104366b6b407695cae3a3)）。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用；定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、图像模型配置与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

## [0.2.20]

### 改进

- 更新检查改走边缘网关：启动时一次缓存查询代替匿名分页轮询 GitHub API，不再受共享限流影响；网关不可用时自动回退直连，行为与之前一致。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用；定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、图像模型配置与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

## [0.2.19]

### 新功能

- 定时任务新增单次执行：可按具体日期时间执行一次，日期改用日历选择，运行后自动停用并保留历史。
- 图表支持全屏预览：点击对话中的图表（或聚焦后按 Enter/Space）进入全屏，可放大、缩小、重置 1:1，按 Esc 关闭。

### 改进

- 图表配色跟随主题：表面改用主题色填充，边框带主题色，多系列由灰阶改为彩色，并区分深浅主题。
- 排队消息移入输入框卡片内部，可折叠，默认展开；拖拽排序时拖起行跟随指针，其余行滑开，中线落点。
- 原生公式优化尺寸与溢出：行内 1.21 倍、独立 1.25 倍显示，超宽公式独占一行并在原位横向滚动，不打乱正文排版。

### 修复

- 修复会话中断后的状态恢复：中断后不再误显示“工作中”，失效的运行时在下次提交前重建。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用；定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、图像模型配置与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

## [0.2.18]

### 新功能

- 通用设置新增「图像生成」开关，启用 OpenCode 的实验性 `image_generate` 工具，并可从已连接提供商的图像模型中选择默认模型。
- 会话胶囊新增 Todo 列表，展示 Agent 的待办事项、当前执行项和完成进度。
- 通用设置新增保持唤醒选项，开启后在应用运行期间防止屏幕关闭和系统自动休眠。
- 新增 Claude 主题，并调整现有主题配色。

### 改进

- Goal 改用 OpenCode 官方功能，无需设置开关；支持设置目标、查看状态、暂停、恢复和清除。旧版 Fintwind Goal 不会自动迁移，需要继续的目标请重新设置。
- 自定义提供商的 API 密钥改由 OpenCode 凭据接口托管，支持选择凭据账号；已有明文密钥在凭据写入并校验成功后迁移，环境变量和文件引用保持不变。
- 自定义模型支持独立选择协议或继承提供商协议，补充 OpenAI Images 与 Google Images；默认图像模型列表以当前工作区实际可用的模型为准。
- 新会话增加 ASCII 动态背景，优化分界线拖动时的连续性；空输入框预留两行高度，新建标签页按钮增加跟随主题的底色。
- 会话胶囊的变更入口显示待处理增删行数，并调整宽度与活动光晕效果。

### 修复

- 修复拖选对话内容到视口边缘时不能自动滚动的问题，并改善自动滚动的平滑度。
- 修复第三方图像模型的协议配置与识别问题，保留提供商已有的端点、凭据和其他模型配置。
- 修复 Windows 路径栅格化后未恢复场景渲染目标导致的绘制异常。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用。定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、图像模型配置、[提供商凭据说明](docs/custom-provider-credentials.md)与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

## [0.2.17]

### 改进

- 审阅页将未跟踪和已忽略文件分别列出，与已暂存和未暂存变更分开显示。未跟踪与已忽略目录按需逐层加载，列表过长时提供继续加载或截断提示；未跟踪目录加载完整前不执行整组批量操作。
- 工作区中的子模块和嵌套 Git 仓库显示为独立节点，可展开查看各自的更改；仓库扫描不完整时显示提示。
- 查看工作区变更时按所选文件加载差异，默认显示少量上下文，并可按需展开更多内容。
- 差异视图增加行内修改高亮和增删标记；顶部显示当前审阅范围的总增删行数，统计不完整时给出提示。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口；验证范围见 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。未验证完整的模型或版本兼容区间。

## [0.2.16]

### 新功能

- 新增内置定时任务 MCP，Agent 可根据用户要求创建周期任务，继续当前会话或在同一工作区新建会话；未明确的执行位置、时间和任务内容由 Agent 向用户询问。通用设置提供独立开关，默认关闭，开启后自动连接 OpenCode 的实验性 MCP 接口，无需重启会话。
- 未关联项目的新会话共用固定的通用工作区，可在「设置 → 通用 → 数据管理」中选择文件夹或恢复默认；已有会话继续使用原目录。

### 改进

- 一键更新下载时显示进度条、百分比和已下载大小；总大小未知时显示已下载大小。
- 通用设置新增「运行时发送」，可选择 Agent 工作时将消息排队或立即插话；调整排队消息的显示与操作。
- 提问卡片支持跳过、收起和展开。
- 审阅页的变更文件按目录树展示，支持展开和收起目录。
- 用量页按「单日、7 天、30 天、全部」排列时间范围，每次打开默认显示单日，并优化切换动效。
- 精简连续活动卡片的默认显示，调整后台任务胶囊和应用内更新日志的呈现，减弱工作区阴影。

### 修复

- 修复包含中文等非 ASCII 字符的工作目录在 OpenCode 请求头中的编码问题。
- 修复上下文面板过窄时模型名称溢出的问题。

### 兼容性

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。定时任务 MCP 需要 OpenCode 2.1.4 或更新版本；该开关仅启停实验性 MCP 连接，不修改其他实验性设置。

## [0.2.15]

本版继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。上下文采集通过隔离 OpenCode 服务与本地模拟模型的 10 项端到端检查，并回放了 3 个真实会话副本；路径设置通过 6 项隔离黑盒检查，旧会话清理通过 14 项模块黑盒检查。上述检查不代表完整的模型或版本兼容区间，也未覆盖新增路径设置的实际会话启动或界面操作。验证说明见 [上下文构成](docs/context-composition.md)与 [可执行文件路径](docs/opencode-executable.md)。

### 新功能

- 通用设置新增 OpenCode 可执行文件选择，可指定便携版 `opencode-cli.exe`，无需配置 PATH；支持恢复自动检测。保存的路径用于之后启动的会话和后台操作，运行中的会话继续使用原进程。
- 新增旧会话清理，按最后活动时间提供 30／90／180 天选项，默认 90 天。预览后确认清理，仅处理 Fintwind 中可见且关联 OpenCode 的会话，通过 OpenCode 官方删除接口清理服务端数据，并删除对应的 Fintwind 记录。

### 改进

- 上下文面板改用面积分块图，按系统与规则、工具定义、对话正文、推理、工具调用、文件内容、其他工具结果和压缩摘要展示估算构成。实际占用与分类估算分开显示，加密推理等不可测内容单独标记；旧会话在下一次请求后生成新分类。
- 通用设置按功能、上下文管理、运行环境和数据管理分组。
- 会话输入框增加轻阴影，改善与背景的层次区分。

### 修复

- 修复重开会话后消息不断重复的问题：保存完整历史快照时，按消息 ID 清除已被替换的旧行，避免相同位置累积多个副本。
- 修复单个长回合积累大量截图后，回答完成仍持续显示「工作中」的问题。恢复消息前先将工具图片保存为内容寻址引用，避免图片内联数据挤满累计恢复预算；保留图片、工具顺序与 64 MiB 响应保护，并补充累计预算和分页时限的明确诊断。

## [2.0.14]

本版继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。已使用真实 OpenCode 2.1.3 与本地模拟模型完成客户端、daemon 和服务端的端到端验证，覆盖同一会话连续三回合正常结束；独立黑盒检查覆盖历史接口返回 503、同毫秒旧结束事件、断线恢复、追加输入、丢失结束事件、超过 64 MiB 的历史页和单条含 20 MiB 图片数据的消息，并重放真实会话的历史副本验证恢复。未验证完整的模型或版本兼容区间。验证入口见 [协议黑盒脚本](scripts/opencode-protocol-e2e.py)。

### 改进

- 应用内更新改为按正式 Release 的发布时间选择最新发布，不再比较版本号大小。后发布的版本即使版本号更小，也会提示更新；当前已是最新发布时不重复提示，草稿和预发布不参与检查。
- 模型选择器与提供商设置统一使用可识别厂商的品牌图标，适配深浅主题；未识别的提供商保留名称首字母图标。

### 修复

- 修复回答已完成却一直显示「工作中」的问题。完整实时流根据本轮输入送达与执行结束事件收尾；需要恢复时，历史响应上限提高到 64 MiB，超大的历史页自动缩页重读，累计预算只计算本轮保留的数据，避免旧图片或工具结果阻塞回合结束。保留断线补齐、旧事件过滤和追加输入的并发保护。
- 细分回合恢复失败的诊断，区分会话信息、消息历史和活动状态查询失败，并记录历史缩页及超大响应，便于定位后续问题。

## [2.0.13]

本版继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。已有专项验证覆盖 OpenCode 2.1.1 的 Mermaid 提示词分发（v1/v2/v3 指令字节、fork 与进程重启）与校验工具；启动日活上报不依赖 OpenCode，已通过本地身份与上报的黑盒验证。未验证完整的版本兼容区间。验证记录见 [Mermaid 文档](docs/mermaid-prompt.md) 与 [启动日活文档](docs/activity.md)。

### 新功能

- 新增匿名启动统计：正式版与 beta 每次启动在后台向官网发送一个本地随机生成的安装 UUID，用于统计每日活跃的桌面数据目录数。请求不包含会话、项目路径或账号信息，上报失败不影响启动。

### 改进

- Mermaid 图表中的中文、日文与韩文改用屏幕阅读更清晰的系统界面字体（如微软雅黑 UI），图表缩窄显示时笔画更清楚。
- Mermaid 可视化提示词升级到 v3，不再限定流程图或时序图，也不再要求短标签，模型可按内容选择更合适的图表类型；已有会话继续沿用创建时的提示词版本。
- 新建定时任务的星期选择改为多选组件，默认不选任何星期。

### 修复

- 修复性能开销窗口的原生标题栏不跟随暗色主题的问题。

## [2.0.12]

本版使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。已有专项验证覆盖 OpenCode 2.0.27 的 Goal 模式和 2.0.32 的 Mermaid 提示词与校验工具，未验证完整的版本兼容区间。验证记录见 [Goal 文档](docs/goals.md)与 [Mermaid 文档](docs/mermaid-prompt.md)。

### 新功能

- 新增「定时任务」工作区，从侧边栏时钟图标或命令面板进入。可按指定星期与时间，或按固定间隔自动执行指令，适合周期性检查、汇总与维护。任务可新建会话并指定项目、模型与权限，也可复用已有会话；支持暂停、恢复、立即运行与删除，可查看下次运行时间、上一次运行结果并打开对应会话。Fintwind 完全退出后任务不会执行，错过的运行不会补跑。
- 对话记录支持原生 Mermaid 图表，可直接查看流程图、时序图、类图、状态图、实体关系图、饼图与甘特图。图表在本地编译，跟随主题、字号与高 DPI 缩放，支持切换源码、键盘滚动与复制；编译失败时保留源码并显示具体原因与行列位置。渲染采用纯 Rust 的 [Ondine](https://github.com/wevm/ondine)，无需联网或运行 JavaScript，许可见 `docs/licenses/ondine-MIT.txt`。
- 新增「Mermaid 可视化提示词」设置，开启后模型会收到图表书写指引，并通过 `fintwind_mermaid_check` 自检与修正图表。默认关闭，仅影响新会话；每个会话保留创建时的偏好，重开、重启或派生会话时继续沿用。
- MCP 市场新增办公、设计、数据、自动化、运维、商务六个分类，收录 Notion、Zoom Docs、Atlassian、Linear、monday.com、Figma、Canva、Airtable、Supabase、Zapier、Sentry、Vercel、Stripe、ZoomInfo，均使用厂商官方 MCP 端点。
- 上下文面板新增「平均首字延迟」，查看模型从请求到首次响应的等待时间。优先使用服务端测量，缺失时回退到客户端轮次计时。

### 改进

- 审阅侧栏重新整理项目、分支与文件导航。文件名与目录分行显示，选中背景等宽，状态标签统一右对齐；远程仓库操作与全部丢弃操作收进菜单，保留常用的刷新、全部暂存与提交入口。
- 用量页顶部合并为一张总览卡片，突出总 Tokens、生成 Tokens、会话数与平均缓存命中率，精简次要说明，底部输入、输出与缓存数据等宽排列；折线图改为平滑曲线。
- 优化定时任务表单分组与宽度适配，星期按钮均分宽度，选中时使用主题色。
- Brave、Exa、Tavily、Keenable、Parallel、Linkup、Bright Data、Consensus、Hugging Face、GitHub、DeepWiki、Microsoft Learn、Gread 改用各自的官方 MCP 服务，详情页提供官方项目链接。重新添加已配置条目时直接更新旧端点，保留名称、请求头与 OAuth，避免重复。PubMed、arXiv、Paper Search、Semantic Scholar、Wikipedia 继续使用标明来源的托管服务。
- 过长的用户消息默认折叠，主会话与子代理界面均支持展开与收起。
- 模型厂商图标改用彩色品牌资产，并补充缺失图标。
- 提高界面对比度，减轻深色模式下侧栏与工作区交界处的阴影，调整圆角与标签页间距；上下文面板分区改为浮动卡片，用量、通用与外观设置卡片补充轻阴影。

### 修复

- 修复「重载 OpenCode 服务器」未出现在输入框 `/` 命令列表中的问题。

## [0.2.11]

- 内置供应商支持应用内 OAuth 登录：与 CLI 和官方桌面应用同一链路，通过 OpenCode 服务端 connect/oauth 完成授权；供应商页提供登录按钮、预启动表单（选项与条件字段、必填校验）、授权码输入与取消，需要重新授权的供应商提供重新登录入口
- 对话记录中的行内文件路径支持用默认应用打开，右键可为路径选择打开方式
- 命令面板新增「重载 OpenCode 服务器」：修改配置后无需重启服务进程即可生效，运行中的会话会在下一步边界用上新配置
- 用量页上下文明细改为方格图：按输入、缓存读取、缓存写入与输出分类展示上下文的 token 构成
- 侧边栏视觉打磨：项目卡片去边框、收紧间距，展开的会话带缩进与引导线，选中会话在引导线上标记；补充通用设置的说明文字
- 修复会话界面输入框焦点混乱丢失的问题
- 修复自动压缩触发不可靠的问题，并新增上下文采集工具
- 修复非 Goal 轮次误调用目标结算工具，以及 Goal 暂停恢复后累计计时错误的问题
- 修复轮次中的引导消息未被折叠的问题

## [0.2.10]

- 新增内置 Goal 目标驱动模式：在设置中开启后，通过 `/goal <目标>` 为会话设定执行目标；模型在轮末通过结算工具自主汇报状态，支持自动续跑与后台任务等待，最多连续推进 20 轮，并在输入框上方实时显示状态条与汇总卡片
- 输入框支持 `@` 补全并显式加载技能与工作区文件，补全项带有技能和文件图标，并在草稿与用户消息中渲染图标标签
- 设置页新增 OpenCode 终端 Shell（PowerShell 7、Windows PowerShell、Command Prompt、Git Bash、WSL 等）选择与配置
- Git 提交历史详情面板新增文件变更统计，展示涉及的文件总数及具体的增删行数
- 优化长会话用量持久化索引，保留增量用量并修复模型归属统计
- 优化设置通用页的排版与输入框配色
- 修复内置浏览器打开共享标签页时的权限授权交互逻辑
- 修复浅色模式下消息气泡背景对比度不足的问题
- 修复会话内容与审阅代码无法使用快捷键复制，以及特定键盘焦点导致快捷键失效的问题
- 修复 Windows 重叠 I/O 挂起（错误码 997）误判为长连接断开的问题

## [0.2.9]

- 内置供应商 DeepSeek、智谱（Z.ai）、Kimi、MiniMax 新增订阅使用情况显示，可查看订阅额度或 API 余额；密钥由 OpenCode 托管，不会写入日志
- 修复通过 HTTPS 代理时请求可能失败的问题
- 视觉打磨：工作区分层更清晰、选中色更中性；回复中的行内代码改为与正文同色，主题色更柔和
- 修复侧边栏搜索按钮错位的问题

## [0.2.8]

- 新增 Google（Gemini）API 格式，可用于自定义供应商
- 新增 HTTP 代理设置，应用与模型请求可使用代理
- 帮助菜单新增应用进程浏览功能
- 新增上下文提前自动压缩，可按 token 数设置触发阈值
- 模型思考档位改由 models.dev 动态解析，优先使用对应厂商数据；未收录的模型提供标准六档
- 用量估算支持阶梯价格和长上下文价格，按本次请求的上下文量计算费用
- 修复第三方插件命令和自定义斜杠命令不显示的问题
- 移除实验性手机远程控制与设备配对功能

## [0.2.7]

- 输入框支持引用：文件编辑器右键「引用选中代码」，聊天消息右键「引用到输入框」（有选区引选区，无选区引整条），也可从输入框的「+」菜单引用文件；引用显示在输入框上方、可单独移除，发送时作为带来源的引用块交给模型，随草稿保存，切换会话不丢
- 外观设置新增「对话宽度」，提供紧凑 / 默认 / 宽 / 超宽四档，控制大屏幕上消息与输入框的最大宽度；字号新增「超大」档
- 用量页「模型用量」改为矩形树图：每个模型一块瓦片，面积即该模型的 token 占比，取代原来的条形排行；Provider 与项目两张排行保持原样
- 新增帮助入口与应用内更新日志卡片，可直接查看版本变更；帮助菜单同时提供问题反馈入口
- 上下文支持提前自动压缩，可在设置中启用或关闭，并按绝对 token 数调整触发阈值
- 会话标签页现在会显示待处理的问题、审批和失败状态，切换会话时不易错过需要处理的事项
- 会话错误会写入对应会话记录，不再只显示一次性的提示；重启或稍后查看时仍可追溯
- 限制自动保存与子会话记录缓存的内存占用，改善长会话和大量后台工作的稳定性
- 修复：内置浏览器底部不再显示状态栏

## [0.2.6]

- 全新双层侧栏：最左侧为固定图标栏（会话、审阅、设置），第二栏随模式切换；窗口顶部新增贯穿全宽的顶栏，窗口控件固定贴右，不再与会话标签争抢宽度
- 新增独立的「审阅」页，取代原右侧面板的审阅入口：可选择跟随会话或任意已记录的工作目录，按「已暂存 / 未暂存」分组，支持单文件与全部文件的暂存、取消暂存、丢弃（删除未跟踪文件保留单独确认弹窗）
- 审阅页 Diff 支持并排 / 统一视图切换与文件间导航；查看历史快照时写操作控件自动隐藏
- 新增提交对话框：可只提交已暂存内容，或暂存全部并提交；支持用编码代理按已暂存内容生成提交信息，以及提交并推送
- 分支行提供刷新、Fetch、Pull（仅快进）、Push 与分支切换
- 用量从设置分类移出，成为第一栏的独立模式，页面全宽渲染；设置与命令面板不再重复提供入口
- 各主题家族适配新的 L 形外框：顶栏与第一栏、第二栏之间以轻微色差分层，深浅模式均适配
- 更新应用图标与品牌 logo
- 修复：提交信息字号不一致；没有已暂存更改时打不开提交卡片
- 优化左上角控件位置

## [0.2.5]

- 更新下载改为择优取源：国内网络下 GitHub 的多个地址并非全部可达，下载前会实测发布页与两个公共中转，取最快且确实传来二进制字节的那个；卡片显示「正在选择下载源…」。下载后仍按 Release 公布的 sha256 校验，中转只能影响速度、无法影响内容
- 一键更新不再把安装包留在临时目录：装完由新启动的应用负责清理，不再每次更新累积约 12 MB

## [0.2.4]

- 通用设置新增「浏览器工具」开关，默认关闭：开启后才向模型注入内置浏览器工具与使用说明；切换即时生效（无需重启会话），关闭状态下工具与说明完全离开模型上下文，误触发调用也会被守护层拒绝
- 更新卡片新增「一键更新」：自动下载与当前架构匹配的安装包，按 Release 公布的 sha256 校验无误后静默安装，无需确认也不需要管理员权限，装完自动重启应用；解压即用的便携版没有安装记录，仍只提供前往 Release 下载的入口。GitHub 的多地址在部分网络下并非全部可达，下载前会实测发布页与两个公共中转并取最快者，校验始终以 Release 公布的 sha256 为准，中转只能影响速度、无法影响内容
- 内置浏览器工具扩展为 16 个：新增截图（返回模型可见的图像）、evaluate（受控执行页面 JavaScript，与其他变更操作同等授权）、按键、下拉选择、悬停、双击、拖拽、坐标点击与关闭页面；截图以有界 Media 通道送达，close 仅限会话自动打开的页面。evaluate 结果为 undefined 时按 null 返回而非误报原生错误，CDP 协议层拒绝改报可采取的受控错误；open 在分享发布经受住一个轮询周期后才向模型报成功，发布被拒不再谎报 pageId

## [0.2.3]

- 支持 OpenCode v2.0.19（此前仅保证到 v2.0.16）
- 新增 Git 历史面板：从会话胶囊即可进入，带本地/云端标签；超长提交信息不再溢出卡片、遮挡按钮
- 新增 12 套可切换主题家族：VS Code Modern、Codex、Nord、Linear、Notion、One、Proof、Raycast、Rose Pine、Solarized、Vercel、VS Code Plus，原有 Fintwind 主题保留
- 文件界面支持多开与回退
- 优化用量统计：子代理的 token 计入总量，模型和提供商排行按子代理实际使用的模型归属，费用按各自模型估算，并改用 models.dev 官方 API 价
- 优化模型界面的刷新与加载状态
- 侧边栏项目会话改为逐步显示
- 对话结尾补充角色、模型和耗时；信息缺失时省略，不再显示不可靠的 token 统计
- 修复浅色模式下 pwsh 行内预测提示不可见的问题
- 顶部会话标签统一为 190 px 宽

## [0.2.2]

- Open multiple sessions in tabs within one window, with more stable unread indicators
- Improve theme contrast and keyboard accessibility across the interface
- Refine token-speed display, file-drop attachment handling, and session activity presentation
- Remove the obsolete Todo section from session cards to match the OpenCode 2 protocol

## [0.2.1]

- Attach images and PDFs inline as data URLs so the model sees them directly instead of re-reading the files
- Follow OpenCode 2.0.11+ contract changes, keep session listing working on v2, and trace sessions back to their owning task
- Refresh the activity card UI, with the shimmer highlight now using the theme accent color on activity and thinking cards
- Merge content that trails a turn into its closing message
- Add a show/hide toggle to API key inputs

## [0.2.0]

- Send image and text attachments through OpenCode's files channel instead of splicing `@path` into the message, and drop files from the desktop onto the transcript or composer to attach them
- Show images that were already saved in a session's history
- Add a context-window panel with clearer usage stats
- When a message cannot be reverted, show a disabled button and the reason instead of hiding the control
- Add an in-app update card when a newer release is available
- On the Usage page, open a single day to see hourly charts and a usage breakdown
- Shimmer running tool names, and keep that treatment on subagent cards
- Rename native sessions through OpenCode's stable PATCH endpoint so the title syncs to the server
- Add catalog support for GPT-6, Claude Opus 5.5, and MiMo V2.6
- Say when a built-in provider check is reading the local model catalog, not testing the provider API
- Fix the info capsule turning transparent on hover, and even out sidebar shortcut spacing

## [0.1.8]

- Share one private OpenCode server across the app instead of a per-project pool, so sessions stay in sync with fewer conflicts
- Rebuild the provider page with login and logout for built-in providers
- Revert and restore a session through OpenCode's native revert and unrevert, keeping the same session
- Add a collapse control on the question card, and move where a new project is created
- Show more detail on subagent cards
- Smooth the Usage page, and count reasoning tokens in turn token speed the same way the OpenCode TUI does
- Remove the OpenCode Go plan from the context-usage popover
- Keep streaming text from being split apart when a tool call arrives
- Update StepFun model information
- Raise the frame rate of the session activity halo
- Remove the web client

## [0.1.7]

- Show raw OpenCode tool names (read, grep, execute, …) on activity cards, replacing generic icons, and render nested Code Mode tool calls from `toolCalls`
- Add settings to change the UI and code fonts
- Update the model thinking-level reference table for 2026 models

## [0.1.6]

- Add a Usage page that scans OpenCode session records for token and cost stats, with KPI cards, an activity heatmap, daily bars, and per-model ranking
- Cap the expanded height of the project sidebar, tool lists, and expanded cards so long lists stay inside the window
- Probe OpenCode 2.0.5's `/api/status` health endpoint first, falling back to `/api/health` for older CLIs
- Keep a session's transcript visible after the backend process exits, instead of dropping the render
- Fix remote MCP OAuth login so the authenticate flow completes instead of failing on the CLI callback chain

## [0.1.5]

- Typeset inline and display math natively instead of approximating it with Unicode characters, so formulas keep their real glyphs, sizes and theme color
- Accept the `\[…\]` and `\(…\)` delimiters models emit far more often than `$…$`, without CommonMark unescaping them into plain text
- Add Copy Expression to a formula's context menu
- Keep unfinished math in a streaming message from swallowing the markdown that follows it
- Typeset formulas on background workers with cached results, so the UI thread never blocks on the math engine

## [0.1.4]

- Show the app version in General settings, and an Update button next to Settings when a newer GitHub release is available
- Give sidebar session cards a provider-colored model avatar with a spinning halo while the session works, replacing the cramped three-line layout
- Simplify access modes to Ask, Auto-accept edits, and Full access; sessions saved with the old Auto mode open as Full access
- Fix subagent permission prompts and form replies being routed to the parent session, which left tools stuck as running until manually stopped

## [0.1.3]

- Add a font size setting
- Add the ability to remove a project
- Add an MCP marketplace and remote MCP OAuth login, with live connection status
- Fix the subagent lifecycle status in the UI
- Keep subagent user-message bubbles from shrinking as the panel widens

## [0.1.2]

- Show turn-footer token stats and provider retry cards
- Reconnect the daemon in place after a disconnect instead of failing permanently
- Keep late-arriving reasoning from splitting into an orphan thinking block
- Improve compaction UX and window chrome

## [0.1.1]

- Drive the `opencode` CLI instead of `opencode2` after OpenCode merged the two

## [0.1.0]

First Fintwind release, forked from [waku](https://github.com/egoist/waku) 0.1.7.
Versions from this point are Fintwind's own; they are not waku releases.

- Windows-only native desktop for OpenCode (Rust + GPUI)
- Drive a single OpenCode backend; remove other agent providers
- Group sessions by project with a card-style sidebar and per-group new session
- Model picker with provider filter, company brand icons, input modality, and
  thinking-effort variants
- Virtualize reasoning, show tool calls step by step, and render LaTeX in the
  transcript
- MCP management UI
- Close the window to quit and stop the OpenCode backend on exit
- Title-bar action to open the project in File Explorer
