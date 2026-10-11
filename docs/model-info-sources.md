# 模型信息的来源与数据流

模型名称、上下文窗口、输出上限、模态、价格和思考档位全部来自
models.dev，运行时拉取，无任何手工登记的静态表。本文记录数据的来源
可信度层级和运行时链路，避免每次重新摸索。

## 一、信息来源与查证顺序

排查一个模型的字段问题（档位不对、价格异常）时，按「及时性 → 权威性」
分四层核对，跳层采信会把转售商的错误字段写进结论。

### 1. models.dev API（运行时唯一来源）

```
https://models.dev/api.json
```

一份文件几 MB，别整份读进上下文。只查厂商自己的条目，用脚本按 id 取字段：

```powershell
$catalog = Invoke-RestMethod -Uri 'https://models.dev/api.json'
$m = $catalog.openai.models.'gpt-6-sol'          # 或 anthropic / xai / google
$m.name; $m.reasoning_options | ConvertTo-Json -Depth 10
$m.limit | ConvertTo-Json -Depth 10; $m.modalities | ConvertTo-Json -Depth 10
$m.cost | ConvertTo-Json -Depth 10               # 价率（USD/百万 token）与 tiers
```

关键模型字段：

- `limit.context` / `limit.output` / `modalities` — 上下文、输出上限、模态。
- `cost.input` / `output` / `cache_read` / `cache_write` — 基础价率；
  `cost.tiers[]`（`tier.size` 阈值 + 分档价率）与便捷字段
  `context_over_200k` — 上下文超阈值后的价率，`context_over_200k`
  等价于一个 `size: 200000` 的 tier。
- `reasoning_options` — 思考档位形态，三种类型（可混杂出现）：
  - `{"type":"effort","values":["low","medium","high",...]}` — 档位枚举
  - `{"type":"toggle"}` — 只能开/关
  - `{"type":"budget_tokens","min":1024}` — token 预算下限

#### 聚合数据不能单独信

models.dev 是社区聚合，转售商条目互相矛盾是常态。实测（223 家供应商、
215 家带 reasoning 字段）的例子：同一个 `claude-sonnet-4-6`，转售商标
`effort: low/medium/high/max`（把 OpenAI 的词套到 Claude 上），另一家标
`toggle + effort + budget_tokens` 三种全给。

运行时解析因此**厂商条目优先**：每个模型副本记录来源 provider，解析时
（计费、档位、补全都一样）先取用户所配 provider 对应条目的副本，命中即
用；该条目没有此模型时才回退跨供应商多数投票兜底。实测例子：
`qwen3.6-27b` 的全局多数票是仅 1 票的乱标转售商副本（含厂商没有的
`high` 档），厂商条目才是正确档位。人工排查时规则相同：**只读厂商自己
的 provider 条目**（xAI 用 `xai`，不用转售商的改价或改模态副本），转售
商副本只做交叉印证。

### 2. 厂商官方文档（最终权威）

models.dev 不标默认档，也不说明能否关闭思考——这两项必须回厂商文档：

| 厂商 | 查什么 |
|---|---|
| Anthropic | extended/adaptive thinking：`thinking.type`、budget 范围、`effort` 默认档、能否关闭思考 |
| OpenAI | reasoning effort：哪些模型支持哪些档（`none`/`low`/`medium`/`high`/`xhigh`/`max`）与默认档 |
| Google | Gemini `thinkingConfig`：`thinkingLevel` vs `thinkingBudget`、隐式思考何时可关 |
| xAI | grok 系列的 reasoning 参数与默认档 |

「网关会不会接受某个值」文档同样不会告诉你，只能实测：配置被 OpenCode
接受、目录能列出档位，不等同于真实供应商已验证。

### 3. 交叉验证（可选，两条免费 JSON）

- **OpenRouter** `GET /api/v1/models` 与模型详情的 `supported_parameters`
  （含 `reasoning_effort` / `reasoning_effort_values`）。
- **LiteLLM** `model_prices_and_context_window.json` 的
  `supports_reasoning_effort` / `supported_reasoning_effort_values`。

与 models.dev 不同源，三方一致时可信度很高；不一致时以厂商文档为准。

---

## 二、运行时数据流

| 环节 | 文件 | 职责 |
|---|---|---|
| 拉取与解析 | `crates/fintwind-client/src/models_dev.rs` | 下载 `api.json`，拍平成按规范化 id 分组的表；每副本记录来源 provider，解析时厂商条目优先、无命中才跨副本投票 |
| 档位 → 请求选项 | `crates/fintwind-client/src/provider_thinking.rs` | `mode_options` 按 API 格式把档位翻译成各家请求参数 |
| 写入配置 | `crates/fintwind-client/src/opencode_config.rs` | 保存供应商时把档位写进 `variants`/`options`，并清掉目录不再列出的陈旧档位 |
| 目录标签 | `crates/fintwind-core/src/model_catalog.rs` | 服务器目录的 variant 用 models.dev 的默认档兜底 |
| 费用估算 | `src/app/usage_page.rs` | 按会话上下文选价率档（tiers），乘 token 计费 |

三条消费路径都读同一份 `ModelsDevTable`（内存 10 分钟复用 + 磁盘缓存
离线回退；计费价目表单独 24 小时 TTL）：

1. **供应商页补全**：`resolve_model_basics` 填名称/窗口/模态。
2. **思考档位**：`resolve_thinking` 把 `reasoning_options` 归并成档位集。
3. **费用估算**：`resolve_model_cost` + `for_context` 选档计价。

### 思考档位的归并与默认档

`resolve_thinking_for` 优先读所配 provider 对应条目的 `reasoning_options`
（转售商副本不参与），无命中才回退多数副本，然后归并：

- **effort 项优先**：档位 = effort 原值（`none` 映射为 `nothinking`）；
  旁边有 toggle 项就在最前补 `nothinking`。
- **只有 budget**：展开为 `nothinking` + `low`/`medium`/`max` 三档，
  预算取预设（1024/8192/24576）且不低于目录给的 `min`。
- **只有 toggle**：`nothinking`/`thinking` 两档。

models.dev 不标默认档，`default_mode_of` 启发式推导：含 `medium` 选
`medium`；否则去 `nothinking` 后取上中位（思考模型默认要思考）；toggle
默认开；budget 默认 `medium`。默认档被用户或服务器配置的显式设置覆盖，
推断只在缺省时生效。

目录确认某模型没有任何档位描述时，写配置退到**标准六档**
`none/low/medium/high/xhigh/max`（`none` 即关闭档），默认 `medium`；
Anthropic 线上六档按预算预设（1024/8192/24576）落参数。目录本身不可得
（无网络无缓存）时不动配置，保留原有档位。

### 计费口径

跟随 OpenCode 官方 `calculateCost`：`reasoning` 并入 output 桶按 output
价率计；档位按本次请求上下文 `input + cache_read + cache_write` 选——
超过某 tier 的 `size` 就用该档价率，tier 未标的价率沿用基础值。
`context_over_200k` 解析时折算成 200000 档，两种写法同价。

---

## 三、何时需要改代码

- **新模型 / 新版本**：什么都不用做。models.dev 更新后，缓存按 TTL 刷新，
  供应商页重新「获取模型」即补齐。
- **新协议格式**（某家供应商的档位请求参数变了）：改
  `provider_thinking.rs::mode_options`，按 API 格式加分支并补测试。
- **解析失败 / 字段漂移**：跑 live 验证
  `cargo test -p fintwind-client --lib -- --ignored`，它对真实
  `api.json` 校验 `reasoning_options` 与 `cost.tiers` 的形状和端到端解析。
- 新公司品牌图标仍只动 `src/ui/mod.rs` 的 `MODEL_COMPANY_MARKS`。
