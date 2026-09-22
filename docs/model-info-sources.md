# 模型信息的来源与查证顺序

模型名称、上下文、输出上限、模态、思考档位和默认档分布在不同来源。按
「及时性 → 权威性」分四层使用，跳层采信会把转售商的错误字段写进配置。

## 1. models.dev API（第一站，最及时）

```
https://models.dev/api.json
```

每个模型的两个关键字段：

- `reasoning`：是否是推理模型。
- `reasoning_options`：档位形态，三种类型：
  - `{"type":"effort","values":["none","low","medium","high",...]}` — 档位枚举
  - `{"type":"toggle"}` — 只能开/关
  - `{"type":"budget_tokens","min":1024}` — token 预算

名称、`limit.context`、`limit.output`、`modalities` 也取自这里，按 id 填空缺、
不覆盖已有值，不手改 `temp/model-cache`。

### 聚合数据不能单独信

models.dev 是社区聚合，转售商条目互相矛盾是常态。实测（223 家供应商、
215 家带 reasoning 字段）的例子：同一个 `claude-sonnet-4-6`，转售商标
`effort: low/medium/high/max`（把 OpenAI 的词套到 Claude 上），另一家标
`toggle + effort + budget_tokens` 三种全给。

规则：**只读厂商自己的 provider 条目**（xAI 用 `xai`，不用转售商的改价或
改模态副本）。转售商副本只做交叉印证，不作为依据。

## 2. 厂商官方文档（最终权威）

models.dev 不标默认档，也不说明能否关闭思考——这两项必须回厂商文档：

| 厂商 | 查什么 |
|---|---|
| Anthropic | extended/adaptive thinking：`thinking.type`、budget 范围、默认是否开 |
| OpenAI | reasoning effort：哪些模型支持哪些档（`minimal`/`low`/`medium`/`high`/`xhigh`） |
| Google | Gemini `thinkingConfig`：`thinkingLevel` vs `thinkingBudget`、隐式思考何时可关 |
| xAI | grok 系列的 reasoning 参数与默认档 |

「网关会不会接受某个值」文档同样不会告诉你，只能实测：配置被 OpenCode
接受、目录能列出档位，不等同于真实供应商已验证。

## 3. 交叉验证（可选，两条免费 JSON）

- **OpenRouter** `GET /api/v1/models` 与模型详情的 `supported_parameters`
  （含 `reasoning_effort` / `reasoning_effort_values`）。
- **LiteLLM** `model_prices_and_context_window.json` 的
  `supports_reasoning_effort` / `supported_reasoning_effort_values`。

与 models.dev 不同源，三方一致时可信度很高；不一致时以厂商文档为准。

## 4. 形态贴 OpenCode V2 协议

档位最终要经 OpenCode V2 规范化（`mode_key`、`variants`），落表前对一下
opencode v2 仓库里 provider 对 `reasoning_options` 的解释，确认 `tiered` /
`toggle` / budget 三类如何映射到 `reasoningEffort` / `thinking.type` /
`thinkingConfig`。推断规则见 [thinking-modes.md](thinking-modes.md) 的
「推断范围与限制」。

## 实操顺序

1. models.dev 扫 `reasoning_options`，拿候选档位与名称、上限、模态。
2. 厂商文档定默认档和能否关闭思考。
3. OpenRouter / LiteLLM 交叉印证。
4. 写进 `models_thinking_modes.json`：每个版本单独一条，`mode_key` 用
   `reasoning_options` 原值，只标一个 `is_default`，`thinking_budget_tokens`
   和 `vendor_label` 保持 `null` 除非厂商给出预算；不从上一个版本整段复制
   ——相邻版本的档位可以不同。价格、知识截止和发布日期不进这张表。
5. 应用里重新保存一次供应商（只补缺失档），OpenCode 重新读取配置后菜单
   才显示新档。参考表改动需要重新编译。

新增模型的完整字段规则与写表细节见
[thinking-modes.md](thinking-modes.md) 的「新增模型」。
