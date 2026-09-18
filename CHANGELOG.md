# Changelog

All notable changes to Fintwind. This file is the **source of truth for the
release notes published with each GitHub release**: the Release workflow
extracts the section whose heading matches the version being released and
publishes it as the release body.

Format follows [Keep a Changelog](https://keepachangelog.com). Add a new
`## [<version>]` section at the top for each release, matching the version in
`Cargo.toml`.

Fintwind versions are independent of [waku](https://github.com/egoist/waku).
Do not copy upstream version numbers into this file.

Write release notes for the final product users receive, not the development
history. When a feature is still unreleased, fold its fixes and refinements into
the original feature bullet instead of adding separate entries for them.

## [unreleased]

## [0.1.7]

- Show raw OpenCode tool names (read, grep, execute, …) on activity cards, replacing generic icons, and render nested Code Mode tool calls from `toolCalls`
- Add settings to change the UI and code fonts
- Update the model thinking-level reference table for 2026 models

## [0.1.6]

- Add a Usage page that scans OpenCode session records for token and cost stats, with KPI cards, an activity heatmap, daily bars, and per-model ranking
- Cap the expanded height of the project sidebar, tool lists, and expanded cards so long lists stay inside the window
- Probe OpenCode 2.0.5's `/api/status` health endpoint first, falling back to `/api/health` for older CLIs
- Keep a session's transcript visible after the backend process exits, instead of dropping the render
- Fix remote MCP OAuth login so the authenticate flow completes instead of failing on the CLI callback chain

## [0.1.5]

- Typeset inline and display math natively instead of approximating it with Unicode characters, so formulas keep their real glyphs, sizes and theme color
- Accept the `\[…\]` and `\(…\)` delimiters models emit far more often than `$…$`, without CommonMark unescaping them into plain text
- Add Copy Expression to a formula's context menu
- Keep unfinished math in a streaming message from swallowing the markdown that follows it
- Typeset formulas on background workers with cached results, so the UI thread never blocks on the math engine

## [0.1.4]

- Show the app version in General settings, and an Update button next to Settings when a newer GitHub release is available
- Give sidebar session cards a provider-colored model avatar with a spinning halo while the session works, replacing the cramped three-line layout
- Simplify access modes to Ask, Auto-accept edits, and Full access; sessions saved with the old Auto mode open as Full access
- Fix subagent permission prompts and form replies being routed to the parent session, which left tools stuck as running until manually stopped

## [0.1.3]

- Add a font size setting
- Add the ability to remove a project
- Add an MCP marketplace and remote MCP OAuth login, with live connection status
- Fix the subagent lifecycle status in the UI
- Keep subagent user-message bubbles from shrinking as the panel widens

## [0.1.2]

- Show turn-footer token stats and provider retry cards
- Reconnect the daemon in place after a disconnect instead of failing permanently
- Keep late-arriving reasoning from splitting into an orphan thinking block
- Improve compaction UX and window chrome

## [0.1.1]

- Drive the `opencode` CLI instead of `opencode2` after OpenCode merged the two

## [0.1.0]

First Fintwind release, forked from [waku](https://github.com/egoist/waku) 0.1.7.
Versions from this point are Fintwind's own; they are not waku releases.

- Windows-only native desktop for OpenCode (Rust + GPUI)
- Drive a single OpenCode backend; remove other agent providers
- Group sessions by project with a card-style sidebar and per-group new session
- Model picker with provider filter, company brand icons, input modality, and
  thinking-effort variants
- Virtualize reasoning, show tool calls step by step, and render LaTeX in the
  transcript
- MCP management UI
- Close the window to quit and stop the OpenCode backend on exit
- Title-bar action to open the project in File Explorer
