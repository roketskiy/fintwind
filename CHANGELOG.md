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

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用；定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、[图像模型配置](docs/image-generation.md)与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

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

继续使用 OpenCode 2（`@opencode/cli`）作为后端，不适用于 OpenCode 1.x。图像生成需要 OpenCode 2.2.1 或更新版本，具体可用性取决于已连接的提供商及模型；官方 Goal 适配依据 OpenCode 2.2.1，旧版本没有官方 Goal 时不能使用。定时任务 MCP 需要 OpenCode 2.1.4 或更新版本，使用实验性 MCP 接口。未验证完整的模型或版本兼容区间，功能范围见 [Goal 说明](docs/goals.md)、[图像模型配置](docs/image-generation.md)、[提供商凭据说明](docs/custom-provider-credentials.md)与 [定时任务 MCP 说明](docs/scheduled-tasks-mcp.md)。

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
