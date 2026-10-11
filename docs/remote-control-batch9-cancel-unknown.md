# 第九批：取消幂等与未知终态

本批处理取消请求和查询失败的收敛边界。目标是：未知不能变成成功，
也不能因为清掉本地标记而再次派发原 prompt。

## 失败矩阵

| 失败方式 | 影响 | 修复 |
| --- | --- | --- |
| `CancelQueuedPrompt` 绕过通用 dedup | 同一个取消重放时再次写入/执行，可能得到错误的未知结果 | 取消先经过统一 replay gate；`accepted` 只查询 daemon，不重发副作用 |
| daemon 查询返回 `pending` 被当作终态 | host 清除取消标记，下一 tick 重新派发原 prompt | `pending` 保持队列和 dedup 原状，下一 tick 只查询 |
| 精确取消查询返回 `unknown` 后仍保留 `queued` | 后续 tick 可能把证据未知的 prompt 自动派发 | 本地队列写入 `unknown` 终态；prompt 和 cancel 都回答 `Unknown` |
| host 恢复查询使用设备当前 grant | 设备重配后旧取消无法查询，取消长期停在 `Accepted` | host dedup 和恢复记录保存取消接受时的 grant，查询使用原 grant |
| 取消绑定写入返回 `0` 未检查 | host 以为已绑定，重启后没有恢复锚点 | `set_cancel_target` 必须确认更新了 accepted 记录，否则回答 `Unknown` |
| 队列恢复与孤儿恢复同时回答同一个 C | 设备收到重复终态 | 以 prompt/C operation id 删除已由队列路径完成的孤儿记录 |
| accepted C 没有持久 target 时重放 | 重放 payload 可能冒认另一个 prompt | 缺少 target 直接收敛为 `Unknown`；已有 target 必须与重放 payload 精确相等 |
| 恢复查询与重放同时收敛 C | 两条路径都覆盖终态并推送结果 | 终态写入使用 `accepted → terminal` CAS；只有 CAS 获胜者推送结果 |
| `expired`/`notSent` 被映射为 prompt `Executed` | 未运行的 prompt 被显示为已执行 | prompt 使用 `Expired` 或已知拒绝；只有 `dispatched` 映射为 `Executed` |

## 语义

- `Accepted` 只表示取消意图已持久，不能表示取消成功。
- daemon 查询失败或仍为 `pending` 时，保留原取消意图，不重新提交取消。
- daemon 返回 `unknown` 时，原 prompt 进入不可派发的本地 `unknown` 终态；不自动重试。
- 取消终态使用原 prompt 的 grant 查询。新 grant 不能执行旧 prompt，但 host
  可以读取旧 prompt 已经产生的只读终态。
- host 清扫已有设备取消绑定时不覆盖原绑定，终态仍归属于原 C。
- prompt 与 cancel 的终态分别记录；取消成功不会把原 prompt 伪装成 `Executed`。
- 恢复、重放和派发完成路径只能将 dedup 记录从 `accepted` CAS 迁移一次，避免
  同一个 prompt 或 C 被重复推送。

## 静态验证

```sh
cargo check --locked -p fintwind-remote-host
```

本批不运行真实模型、生产数据、watcher 或恶意 wire。行为验收仍以
`docs/remote-control-normal-suite.md` 中的独立证据为准。
