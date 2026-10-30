// Release automation only. This script does not build or launch the application.
import { appendFile } from "node:fs/promises";
import { extractReleaseNotes } from "./changelog.ts";

const stableVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function repository(): string {
  const value = required("GITHUB_REPOSITORY");
  if (!/^[\w.-]+\/[\w.-]+$/.test(value)) {
    throw new Error("Invalid GITHUB_REPOSITORY");
  }
  return value;
}

function commit(): string {
  const sha = required("GITHUB_SHA");
  if (!/^[a-f0-9]{40}$/.test(sha)) throw new Error("Invalid GITHUB_SHA");
  return sha;
}

async function api<T>(
  path: string,
  options: { token?: string; body?: unknown; allowMissing?: boolean } = {},
): Promise<T | null> {
  const base = process.env.GITHUB_API_URL ?? "https://api.github.com";
  const response = await fetch(`${base.replace(/\/$/, "")}${path}`, {
    method: options.body === undefined ? "GET" : "POST",
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${options.token ?? required("GITHUB_TOKEN")}`,
      "Content-Type": "application/json",
      "X-GitHub-Api-Version": "2022-11-28",
    },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    signal: AbortSignal.timeout(10_000),
  });
  if (response.status === 404 && options.allowMissing) return null;
  if (!response.ok) throw new ApiError(response.status, path);
  return response.status === 204 ? null : (await response.json()) as T;
}

class ApiError extends Error {
  constructor(public status: number, path: string) {
    super(`GitHub API ${status}: ${path}`);
  }
}

interface Release {
  draft: boolean;
  prerelease: boolean;
  tag_name: string;
  target_commitish: string;
}

interface GitObject {
  object: { type: string; sha: string };
}

async function findRelease(root: string, tag: string): Promise<Release | null> {
  const published = await api<Release>(`${root}/releases/tags/${tag}`, { allowMissing: true });
  if (published) return published;
  // The tag endpoint promises published releases only. Drafts (including
  // untagged manual builds) must also be checked through the authenticated list.
  for (let page = 1; page <= 20; page++) {
    const releases = await api<Release[]>(`${root}/releases?per_page=100&page=${page}`);
    if (!releases) throw new Error("Missing release list");
    const matching = releases.find((release) => release.tag_name === tag);
    if (matching) return matching;
    if (releases.length < 100) return null;
  }
  throw new Error("Release listing exceeded 2000 entries; cannot safely check for an existing draft");
}

async function prepare(): Promise<void> {
  const manifest = Bun.TOML.parse(await Bun.file("Cargo.toml").text()) as {
    package?: { name?: string; version?: string };
  };
  const name = manifest.package?.name;
  const version = manifest.package?.version;
  if (name !== "fintwind" || typeof version !== "string" || !stableVersion.test(version)) {
    throw new Error("Cargo.toml must declare a stable Fintwind version (x.y.z)");
  }
  const lock = Bun.TOML.parse(await Bun.file("Cargo.lock").text()) as {
    package?: { name: string; version: string }[];
  };
  const locked = lock.package?.find((entry) => entry.name === name);
  if (locked?.version !== version) {
    throw new Error("Cargo.lock Fintwind version does not match Cargo.toml");
  }
  const tag = `v${version}`;
  if (required("GITHUB_EVENT_NAME") === "push" && required("GITHUB_REF_NAME") !== tag) {
    throw new Error(`Tag must match Cargo.toml: expected ${tag}`);
  }
  const notes = extractReleaseNotes(await Bun.file("CHANGELOG.md").text(), version);
  if (!notes) throw new Error(`CHANGELOG.md needs non-empty release notes for ${version}`);

  const root = `/repos/${repository()}`;
  const sha = commit();
  const release = await findRelease(root, tag);
  if (release && !release.draft) {
    throw new Error(`${tag} is already published; choose a new version`);
  }
  let ref = await api<GitObject>(`${root}/git/ref/tags/${tag}`, { allowMissing: true });
  // Annotated tags can point to other annotated tags. Bound the traversal.
  for (let depth = 0; ref?.object.type === "tag" && depth < 10; depth++) {
    ref = await api<GitObject>(`${root}/git/tags/${ref.object.sha}`);
  }
  if (ref && (ref.object.type !== "commit" || ref.object.sha !== sha)) {
    throw new Error(`${tag} points to a different commit; never move a release tag`);
  }
  if (release && !ref && release.target_commitish !== sha) {
    throw new Error(`${tag} draft does not identify this exact commit`);
  }
  await Bun.write("release-notes.md", `${notes}\n`);
  await appendFile(required("GITHUB_OUTPUT"), `version=${version}\ntag=${tag}\n`);
  console.log(`Release preflight passed: ${tag} at ${sha}`);
}

interface WorkflowRun {
  id: number;
  head_sha: string;
  head_branch: string;
  event: string;
  status: string;
  conclusion: string | null;
  created_at: string;
  html_url: string;
}

async function checks(): Promise<void> {
  const sha = commit();
  const repo = repository();
  const timeout = Number(process.env.RELEASE_CHECK_TIMEOUT_SECONDS ?? "5400");
  if (!Number.isFinite(timeout) || timeout < 1 || timeout > 5400) {
    throw new Error("RELEASE_CHECK_TIMEOUT_SECONDS must be between 1 and 5400");
  }
  const started = Date.now();
  const deadline = started + timeout * 1000;
  const missingDeadline = started + Math.min(30_000, timeout * 1000);
  const help = `Run Tests manually for this exact commit, then rerun Release: https://github.com/${repo}/actions/workflows/test.yml`;
  let lastStatus = "";
  while (Date.now() < deadline) {
    const data = await api<{ workflow_runs: WorkflowRun[] }>(
      `/repos/${repo}/actions/workflows/test.yml/runs?head_sha=${sha}&per_page=100`,
    );
    const runs = data?.workflow_runs.filter((run) =>
      run.head_sha === sha && (run.event === "workflow_dispatch" ||
        (run.event === "push" && run.head_branch === "main")),
    ).sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at) || b.id - a.id);
    const latest = runs?.[0];
    if (latest?.status === "completed") {
      if (latest.conclusion !== "success") {
        throw new Error(`Tests ${latest.conclusion}: ${latest.html_url}. ${help}`);
      }
      console.log(`Same-commit Tests passed: ${latest.html_url}`);
      return;
    }
    if (!latest && Date.now() >= missingDeadline) break;
    const status = latest ? `Waiting for Tests: ${latest.html_url}` : "Waiting for same-commit Tests to appear";
    if (status !== lastStatus) console.log(status);
    lastStatus = status;
    await Bun.sleep(Math.min(15_000, timeout * 100, Math.max(1, deadline - Date.now())));
  }
  throw new Error(`No successful same-commit Tests within the wait limit. ${help}`);
}

async function website(): Promise<void> {
  const tag = required("RELEASE_TAG");
  if (!tag.startsWith("v") || !stableVersion.test(tag.slice(1))) {
    throw new Error("RELEASE_TAG must be a stable release tag (vx.y.z)");
  }
  const repo = repository();
  const release = await api<Release>(`/repos/${repo}/releases/tags/${tag}`);
  if (!release || release.draft || release.prerelease || release.tag_name !== tag) {
    throw new Error("Website synchronization requires an existing full release");
  }
  const owner = repo.split("/")[0];
  const token = required("WEBSITE_RELEASE_PAT");
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      await api(`/repos/${owner}/fintwind-website/dispatches`, {
        token,
        body: { event_type: "release-published", client_payload: { version: tag } },
      });
      console.log(`Website synchronization requested for ${tag}`);
      return;
    } catch (error) {
      if (attempt === 2 || (error instanceof ApiError && error.status !== 429 && error.status < 500)) {
        throw error;
      }
      await Bun.sleep(1000 * (attempt + 1));
    }
  }
}

try {
  switch (process.argv[2]) {
    case "prepare": await prepare(); break;
    case "checks": await checks(); break;
    case "website": await website(); break;
    default: throw new Error("Usage: bun scripts/release-ci.ts prepare|checks|website");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
