# OpenCode 可执行文件

在「设置 → 通用 → 运行环境 → OpenCode 可执行文件」中选择文件。
便携版可选择 `opencode-cli.exe`，无需加入 PATH。选择后立即保存；
「恢复自动检测」清除此配置，恢复原有的 OpenCode 查找规则。

路径保存在 daemon 的 `settings.json` 中，键为 `opencode_binary_path`。
路径必须是 daemon 主机上存在的绝对文件路径。Windows 接受
`.exe`、`.com`、`.cmd`、`.bat`，Unix 要求文件具有执行权限。
保存时只检查文件路径和文件类型，不执行所选程序，也不据此保证其 OpenCode API 版本兼容。

该配置用于模型目录、版本检测、之后启动的会话，以及由 daemon 发起的后台操作。
正在运行的会话继续使用原来的 OpenCode 进程。切换路径会清空客户端检测缓存，
旧路径尚未完成的检测结果不能覆盖新配置。文件被移动或删除后会报告不可用，
不会自动改用另一份 OpenCode；仍可修改其他设置或清除此路径。

连接远程 daemon 时，本机文件选择按钮禁用；远程路径可由该主机的配置文件指定。

## 验证

```powershell
cargo run --locked --package fintwind-core --example opencode_binary_blackbox -- target/opencode-binary-blackbox
```

黑盒检查使用独立目录，覆盖便携文件名、中文和空格路径、配置重新打开、
无效路径拒绝保存、临时覆盖优先级、文件删除，以及恢复自动检测。
检查不启动 OpenCode，不写入用户配置。结果保存在该次运行的 `report.json`。
