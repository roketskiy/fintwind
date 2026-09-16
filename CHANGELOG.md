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

## [0.1.4]

- Show the app version in General settings, and an Update button next to Settings when a newer GitHub release is available

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
