# 启动日活统计

应用在本地 `app.db` 的 `installation_identity` 单例表中保存随机 UUID v4。该身份属于桌面数据目录。连接远程 daemon 不会把多台桌面的身份合并。删除数据目录会生成新身份，复制整个数据库会复制该身份。

该表由桌面统计模块维护。`drizzle.config.ts` 将它排除在 `db:push` 和 `db:pull` 的管理范围外，避免 daemon schema 工具把身份表当成废弃表删除。

正式版和 beta 每次启动在后台向 `https://fintwind.xyz/api/activity` 发送以下 JSON。开发构建只生成本地身份，不发送生产统计。

```json
{"installation_id":"550e8400-e29b-41d4-a716-446655440000"}
```

请求沿用应用代理，超时为 10 秒。数据库和网络操作都在后台执行。连接在请求前关闭，上报失败不影响应用启动。没有离线补报队列或持续心跳。该接口目前没有后端服务，上报不会产生统计记录。

DAU 表示当天成功上报的独立桌面数据目录数量。它不是自然人数。离线启动可能漏计，持续跨日运行不会自动计入次日。公开匿名接口不能证明请求来自官方应用。

在本仓库运行本地身份与上报黑盒验证。

```powershell
cargo run --locked --package fintwind --example activity_blackbox
```

该验证使用独立临时数据库和 loopback HTTP，不启动应用或 daemon，不访问生产统计接口。
