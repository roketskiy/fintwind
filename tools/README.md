# 开发工具

这些命令与应用共用根目录的 Go 模块，不随安装包发布。从仓库根目录运行。

- `i18n/`：收集界面文字，维护 `cmd/app/locales/` 中的 JSON 词库。
- `export/`：只读源数据库并导出脱敏数据，必须显式指定输入与输出路径。
- `e2e/`：使用独立数据和受控进程验证行为；`runner/` 提供环境白名单和进程树容纳。

```powershell
go run ./tools/i18n --dry-run --catalog-only --files cmd/app
pwsh -NoProfile -File scripts/check-go-backend.ps1
```

自动验证不导入真实凭据，不访问生产数据，不发起付费模型请求。
启动器的无窗口验证通过 `MyGo-Agent.cmd --name <task> --verify --daemon <绝对路径>` 执行。
