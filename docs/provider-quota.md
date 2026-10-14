# Provider quota and balance

## Scope

The built-in provider detail shows vendor-reported subscription windows or API
balance. Custom endpoints are excluded. Credentials come from OpenCode V2's
authenticated `/api/credential` API on the daemon host, never from legacy auth
files, a browser session, or the desktop renderer. Only the selected stored
credential is queried. DeepSeek's environment connection is also supported: the
server must advertise `DEEPSEEK_API_KEY` as its selected connection, and the
daemon resolves that exact variable from the environment passed to its server.
The same handling applies to registered `ZHIPU_API_KEY`, `KIMI_API_KEY`,
`MINIMAX_API_KEY`, and `STEPFUN_API_KEY` connections for their exact catalog IDs.
Other environment and external credentials are not guessed. OpenCode remains the token-refresh
owner. Expired OAuth credentials are not refreshed or rewritten by Fintwind.

Except for the user-requested unchanged OpenAI path, stored credential selection
uses the first entry of OpenCode's ordered integration connection list, not a
guess based on the `/api/credential` active flag. This also handles multiple
legacy records without active flags and preserves stored-before-environment
precedence. A selected ID missing from the credential response fails closed.

Initial adapters: Codex-compatible OpenAI OAuth, public GitHub Copilot OAuth,
Google Code Assist OAuth with a project, Zhipu/Z.ai Coding Plan, Kimi Code,
MiniMax Token Plan, DeepSeek API balance, and StepFun China API balance.
Claude CLI and StepFun Step Plan browser sessions are excluded. A Step Plan
provider must not display the unrelated prepaid API balance.

## Failure cases (before implementation)

- API keys must never be mistaken for ChatGPT or Google consumer subscriptions.
- ChatGPT token-sharing OAuth is not assumed to authorize Codex usage routes.
- Missing, external, unresolvable environment, expired, or ambiguous credentials must not
  trigger credential-file discovery or a vendor call.
- Account changes and logout must invalidate old readings, including requests
  already in flight. A token rotation must not reuse another credential's cache.
- Credentials, raw vendor errors, and response bodies must not cross the quota
  RPC or appear in logs, process arguments, or artifacts.
- Unknown fields, missing numeric values, nulls, malformed JSON, and empty quota
  arrays must not become zero usage or unlimited allowance.
- Zero remaining balance is valid. Missing balance is not zero. Decimal money
  strings and different currencies must remain separate.
- Primary/secondary, per-model, tool-call, weekly, and extra-credit limits must
  remain distinct. Seconds, milliseconds, ISO dates, and fractional remaining
  values must be converted according to each adapter's response contract.
- MiniMax HTTP 200 with a failing `base_resp` is not success; exhausted status
  takes priority over a stale remaining percentage.
- 401/403 must clear previous data. Temporary failures may preserve a same-account
  snapshot with its original measurement time and a visible failure state.
- 429 must impose a cooldown even on manual refresh. Other manual refreshes have
  a floor; concurrent requests share one query. Successful reads have a TTL.
- A reset time passing is not evidence that the current account has zero usage:
  keep the dated reading and query again, rather than inventing fresh data.
- No filesystem, subprocess, network request, or blocking lock in rendering.
- No permanent polling when the page is closed; refresh on selection/page entry
  and through a keyboard-accessible refresh command.

## Verification

`cargo run --locked -p fintwind-core --example provider_quota_blackbox -- <artifact.json>`
exercises the public quota service with synthetic OpenCode credentials and
scripted vendor replies. It covers the adapters, cache, cooldowns, account changes,
malformed payloads, and the credential boundary, and writes a repeatable JSON
report. It never starts OpenCode, the installed app, or the production daemon,
and never reads user credentials. This is a module-level black-box check, not a
live-account or visual test.

`cargo run --locked -p fintwind-core --example provider_quota_live -- <opencode.exe> <new-artifact.json>`
is a separate live, read-only diagnostic for DeepSeek and Zhipu. It reads their
existing OpenCode connections and exercises the production quota service and
curl transport against the real vendor endpoints, producing only status/counts
and timestamps. It is not a desktop UI or daemon RPC E2E test. It requires an
already-running local OpenCode service; no app/daemon is launched, no model
generation or credential update is made, and no process is terminated.

### Issues found by real-account diagnosis (2026-10-15)

- DeepSeek's real connection was environment-backed, absent from stored
  credentials. The first implementation silently omitted its balance.
- Zhipu's real response uses `CREDIT_LIMIT`, not only `TOKENS_LIMIT`. Its rolling
  window may omit `nextResetTime`; absence remains unknown, not an estimated reset.
- Current OpenAI sign-in is `chatgpt-token-sharing`, not Codex OAuth. Per user
  instruction it remains unchanged; no new sign-in or usage-page entry is added.

## Active upstream implementations (checked 2026-10-15)

The following repositories had commits on 2026-10-14. The listed revisions were
read directly, not inferred from README features. Adapted mappings and contract
examples retain MIT notices in `provider-quota-upstream-license.txt`.

| Provider | Source at fixed revision | Processing adopted |
| --- | --- | --- |
| Kimi Code | CodexBar `278e9056e830000532e4c3a6342dc0b2c75f00d6`, `Sources/CodexBarCore/Providers/Kimi/KimiModels.swift`, `KimiUsageSnapshot.swift`, `KimiUsageFetcher.swift`, `Tests/CodexBarTests/KimiRatioPoolTests.swift` | Key-based GET `/coding/v1/usages`; support legacy counts and `usages.limit_5h`, `limit_7d`, `limit_month_total` ratio pools; used counts take precedence over contradictory remaining counts. Matching windows with reset clocks within two seconds use the tighter reading; a different period cannot override a fresh ratio pool. No cookie-based enrichment. |
| MiniMax | CodexBar same revision, `Sources/CodexBarCore/Providers/MiniMax/MiniMaxUsageFetcher.swift`; Magpie `8d2a4a7da3d885beb7e758357b3d735a548cfe64`, `internal/provider/planquota.go` | GET `/v1/token_plan/remains` in the selected region; accept root or `data` payloads, check business status, preserve plan title, interpret percentages as remaining, and keep model/window lanes separate. Missing optional window status does not discard a valid percentage; exhausted status overrides it. Status 3 is not rendered as a metered window. |
| Copilot | Magpie same revision, `internal/provider/subscription_usage.go`; CodexBar `Sources/CodexBarCore/Providers/Copilot/CopilotUsageFetcher.swift` | Use the GitHub OAuth token on `/copilot_internal/user`, not a generated Copilot service token. Numbers may be strings. Keep chat/completion/premium lanes separate; zero entitlement is not an unused allowance. No fake ceiling for credit-billed seats. Enterprise credentials remain unsupported rather than leaking to the public endpoint. |
| Gemini | Magpie same revision, `internal/provider/google.go` | Compatible Code Assist OAuth plus project calls POST `/v1internal:retrieveUserQuota`; remaining fraction becomes used percentage, reset is RFC3339, and model buckets stay separate. Ordinary Gemini API keys are not subscription credentials. Only existing compatible project metadata is supported: no CLI credential-file discovery, account onboarding, or new OAuth login. |

Deliberate deviations: retain Fintwind/OpenCode credential ownership and existing
proxy handling; do not copy browser cookie import, hidden WebViews, pretend client
identity, cross-region retries with keys, automatic account switches, or calls
that consume/reset allowance. OpenAI behavior is unchanged by user instruction.

Verification categories are separate: DeepSeek/Zhipu have a sanitized real-account
read artifact; other adapters have source-backed contract cases only, not live
account tests. Neither proves the currently installed desktop UI has been updated.

## Research sources

- DeepSeek: https://api-docs.deepseek.com/api/get-user-balance
- StepFun API balance: https://platform.stepfun.com/docs/zh/api-reference/accounts/get
- Step Plan is separate: https://platform.stepfun.com/docs/zh/step-plan/overview
- OpenCode V2 schemas: https://opencode.ai/v2/openapi.json
- Magpie, reviewed at `00501401ae03f0362822ce29841153f589e36152`:
  `internal/provider/subscription_usage.go`, `google.go`, `planquota.go`, and
  `stepfun_plan.go`. Internal vendor endpoints are not stable public API promises.
- OpenCode V2 provider sources: `packages/core/src/plugin/provider/openai.ts`,
  `chatgpt.ts`, and `github-copilot.ts` on the `v2` branch. In particular,
  `chatgpt-token-sharing` is not the legacy Codex OAuth method.
