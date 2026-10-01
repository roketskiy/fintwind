# OpenCode 会话移动验收

移动以服务端 `session.moved` 和 `Session.Info.location.directory` 为准，不以移动请求的 HTTP 确认或 inbox 入队为准。

## 需要防止的失败

- 当前会话移到另一个普通目录后，项目分组、工作目录仍指向旧目录。
- 移到已存在或未添加的 Git worktree 后，仍显示“本地”和旧分支。
- 从 worktree 移回普通目录后，残留 worktree 路径或分支。
- 别的会话移动事件被当前会话的驱动过滤，或错误改变当前选择。
- 移动会话从原目录列表消失，被当成删除，丢失打开的标签和已加载对话。
- 应用不在运行时已移动的会话，启动/窗口重新获得焦点后仍留在旧项目。
- 较早的后台查询覆盖较新的移动；目录大小写或分隔符差异创建重复项目。
- 移动后文件、Git、补全和后续请求仍使用旧目录。
- 同步期间在 UI 线程运行 Git、文件遍历或阻塞 RPC。

## 自动化验收

`cargo test --locked -p fintwind-core --test opencode_recovery opencode_session_moves -- --ignored --nocapture`

通过真实 daemon、WebSocket 客户端、OpenCode 驱动和 SSE，连接本地假 provider，
配合真实临时 Git 仓库/worktree。覆盖当前及其他会话事件转发、已移动会话按 ID
找回、确实删除的会话不复活、驱动后续请求目录，以及 worktree/普通目录识别。结果和事件时间线保存
到 `temp/recovery-e2e/<uuid>/result-session-moves.json`。不调用真实模型或移动用户会话。

## 应用内验收

在打开的会话中通过 OpenCode 移到已添加目录、未添加目录和 worktree，再移回。
确认侧栏归属、底栏目录/工作区/分支和工作区查询更新；标签、草稿、对话保持。
对非当前会话重复移动，确认不抢走当前选择。默认不启动 watcher 或进行视觉测试。

移动前有未保存的文件时，旧缓冲保留但转为只读，避免覆盖目标目录的同名文件；
已切走的会话也适用。干净编辑器从目标目录重新加载。会话详情加载、runtime
attach 和 daemon catalog 均不能替代 native roster 的位置切换，以免未确认的
异步保存或跨客户端通知绕过编辑器隔离和工作区刷新。
