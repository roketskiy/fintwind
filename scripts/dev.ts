#!/usr/bin/env bun

import { fileURLToPath } from "node:url";

const child = Bun.spawn(
  ["pwsh", "-NoLogo", "-NoProfile", "-File", "scripts/mygo.ps1", "-Profile", "agent", ...Bun.argv.slice(2)],
  { cwd: fileURLToPath(new URL("..", import.meta.url)), stdin: "inherit", stdout: "inherit", stderr: "inherit" },
);
process.exitCode = await child.exited;
