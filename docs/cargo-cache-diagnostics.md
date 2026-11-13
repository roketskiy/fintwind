# 构建缓存说明

正式应用使用 Go 构建，CI 使用 `actions/setup-go` 缓存 Go 模块和编译结果。
缓存输入由根目录的 `go.mod`、`go.sum` 和 Go 版本决定。

旧 Rust/Cargo 的指纹诊断、缓存恢复脚本和 CI actions 已退役。
旧实现仅以源码形式保留在 [legacy/rust](../legacy/rust/README.md)，不再提供构建工程。

当前构建、检查与 Windows 打包用法见 [CONTRIBUTING.md](../CONTRIBUTING.md)。
排查 Go 构建时可使用 `go env GOCACHE GOMODCACHE` 查看缓存路径，
使用 `go build -x ./...` 查看实际调用。不要清理正在运行的 launcher 目录或用户数据。
