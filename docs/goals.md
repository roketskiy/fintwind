# 内置 Goal

Goal 为当前 OpenCode 会话保存一个目标。开启 Goal 的会话在每轮请求中收到目标协议和 `fintwind_goal_settle` 结算工具，模型在主轮结束时调用它报告 done、continue、wait 或 blocked。continue 表示还有可立即执行的工作；wait 表示已派发后台任务，等待完成通知。wait 保持目标进行中，不主动提交续跑或结算提醒，通知触发后续执行时再正常结算。执行成功后读取这次报告并更新目标，需要继续时才提交下一次执行；成功但没有报告时发送一次提醒。不存在独立的判定请求。普通会话不自动启用 Goal，也看不到结算工具。

## 使用

先在设置中打开 Goal 开关，再输入 `/goal <目标>` 开始。默认关闭。关闭后隐藏 Goal 入口并暂停自动续跑，保留已有目标；再次开启后需明确恢复才继续。开关立即作用于已有会话，并保存到设置中。

输入框上方的目标条显示目标、状态与自本次开始或恢复以来的总用时，包含后台等待时间。展开后查看完整目标、已执行次数、次数上限与最近结果。目标完成时关闭目标条，会话记录最后一条汇总后显示目标结果卡片，包含完成摘要、执行次数、工具调用次数、模型和时间。提交的新目标会作为用户消息显示在会话中；自动续跑不伪装成用户消息。回复执行中需先等待或停止，才能开始或恢复目标。暂停、恢复、完成和清除也可通过 `/goal pause`、`/goal resume`、`/goal complete`、`/goal clear` 操作。

每次开始或恢复后，最多执行 20 次。一次是从模型开始工作到该次回复结束，期间可以调用多个工具，轮末给出一次结算。恢复会重新计算次数。计数表示执行上限，不表示目标完成比例。无工具进展、重复回复、执行错误、中断或连续两轮没有结算报告都会停止续跑。进展按整轮测量：后台任务完成通知（子代理唤醒）不切割测量窗口，通知前的工具调用仍然计入本轮；wait 结算只在子代理会话确有新活动时豁免停滞计数，纯等待加无证据的唤醒照常累计。结算由会话模型自己给出，其结论不能替代实际验收。暂停等原因显示在展开内容中。

执行中可以发送补充信息。引导消息或排队消息会保留当前目标，并使尚未消费的结算报告失效，之后的一轮重新结算。点击停止会暂停目标并中断当前执行。

目标存在 OpenCode 的插件存储中，按会话隔离。服务重启或事件连接中断后暂停目标，明确恢复后才重新执行。Fintwind 只保存显示所需的快照。插件通过 Fintwind 自有 OpenCode 进程的内联配置加载，不修改项目 `.opencode` 或用户配置。

## 来源与取舍

Luna 调研了以下 OpenCode v2 实现，选择 zignd 的目标循环作为精简复刻参考。

| 实现 | 评估 | 结论 |
| --- | --- | --- |
| [zignd/opencode-goal-plugin](https://github.com/zignd/opencode-goal-plugin/tree/cf07a69a4efb2c70a81c7da3dc17d9a9afdd0656) | MIT；Promise 插件 API；会话持久化、独立判定及停止条件可分离 | 使用固定提交的核心机制，保留许可 |
| [ByBrawe/opencode-goal](https://github.com/ByBrawe/opencode-goal/tree/0c834b06843d4f55aa3c6a1489d992adc70a5be4) | 包含更多 host 验证、预算和测试模块 | 本次不引入其完整运行系统 |
| [kryptobaseddev/opencode-goal](https://github.com/kryptobaseddev/opencode-goal/tree/196cea1d4f59d8b5b6d5622fe327d537c876c75a) | 强调结构化验收与 host 验证；README 标为预发布 | 本次不作为基础 |

上游提交较新，选型依据是实际接口和实现范围，不代表已有长期运行可靠性证明。本地适配针对 OpenCode v2.0.27 验证。

Fintwind 保留上游的目标循环机制，把上游的会话内独立判定请求换成主轮内的结算工具，并省去 OpenTUI 面板、presence、终端窗口和自动执行验证命令。消费结算报告前会检查目标代次与 revision，避免旧目标的报告作用到新目标或恢复已暂停的执行。

## UI 依据

当前 [T3 Code ComposerTasksBadge](https://github.com/pingdotgg/t3code/blob/main/apps/web/src/components/chat/ComposerTasksBadge.tsx) 将任务摘要、状态和计数放在输入框附着区域，展开显示详情。[ComposerBanner](https://github.com/pingdotgg/t3code/blob/main/apps/web/src/components/chat/ComposerBanner.tsx) 提供这一层级的布局。Fintwind 采用相同的信息位置与展开方式，使用自身原生控件、主题、字体和焦点处理。轮次不使用任务完成进度条。

具体组件参考 Ely 的 `src/agent/progress.rs` 和 `src/agent/control.rs`。键盘焦点依据项目固定的 `gpui-pre 0.3.8` 及其 `focus_visible` 示例。

## 验证

在 Windows 安装 Python 和 OpenCode 后运行：

```powershell
python scripts/goal-opencode-e2e.py
```

脚本使用真实 OpenCode 与本地脚本模型。每次运行独占数据目录，子进程使用环境白名单，在启动恢复执行前加入独占 Windows Job Object。清理只关闭本轮创建的句柄，不按 PID 或进程名称扫描终止。

报告保存在 `target/goal-opencode-e2e/<run-id>/report.json`。29 项检查覆盖默认关闭、实时开关、关闭保留目标、快速切换时的旧结算失效、初始目标作为真实用户消息保存、主轮内结算与结果数据、普通会话不可见结算工具，以及加载、续跑、替换目标竞态、停止条件、预算、缺失结算提醒、后台等待与完成通知唤醒、重启恢复、会话与目录隔离及命令调用。该脚本验证插件执行行为，不覆盖原生 UI、daemon 完整链路、远程连接或真实模型的结算质量。
