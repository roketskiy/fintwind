// Adapted from Beautiful UI by Shane Levine (MIT), revision 44a274e598395ab61e7c96c26fda2758780253b7.
// Source: components/primitives/ThinkingState.tsx, Reasoning variant — expandable
// header (status + chevron), vertical rail, prose content, settle-once behavior.
// Demo stage timers (STAGES/useSequence), synthetic rows and staged follow-up
// sequencing are intentionally excluded: expanded/working/settled derive from the
// real RemoteActivity (reasoning text, complete, failed). Tool rows show their
// name and failures only, never tool body text or target.
import * as React from "react";

import {
  BotIcon, ChevronDownIcon, EyeIcon, FileDiffIcon, FileIcon, FolderIcon,
  GitBranchIcon, GithubIcon, GlobeIcon, ImageIcon, InfoIcon, ListIcon,
  PackageIcon, PencilIcon, SearchIcon, SparkleIcon, TerminalIcon, WrenchIcon,
} from "@/components/icons";
import { CollapsibleTrigger } from "@/components/ui/collapsible";
import {
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace";
import type { RemoteActivity } from "@/lib/types";

const KIND_ICONS: Readonly<Record<string, typeof WrenchIcon>> = {
  command: TerminalIcon,
  fileChange: PencilIcon,
  fileRead: FileIcon,
  fileSearch: SearchIcon,
  fileList: FolderIcon,
  search: SearchIcon,
  plan: ListIcon,
};

const TOOL_ICONS: Readonly<Record<string, typeof WrenchIcon>> = {
  bash: TerminalIcon, shell: TerminalIcon, terminal: TerminalIcon,
  read: FileIcon, readfile: FileIcon,
  edit: PencilIcon, write: PencilIcon, writefile: PencilIcon, multiedit: PencilIcon,
  applypatch: FileDiffIcon, patch: FileDiffIcon,
  grep: SearchIcon, glob: SearchIcon, findfiles: SearchIcon,
  search: SearchIcon, websearch: SearchIcon, searchweb: SearchIcon,
  webfetch: GlobeIcon, fetch: GlobeIcon, urlfetch: GlobeIcon,
  openurl: GlobeIcon, browser: GlobeIcon, openbrowser: GlobeIcon, navigate: GlobeIcon,
  ls: FolderIcon, listdirectory: FolderIcon, listfiles: FolderIcon,
  todo: ListIcon, todowrite: ListIcon, todoread: ListIcon, updateplan: ListIcon, plan: ListIcon,
  task: BotIcon, agent: BotIcon, newagent: BotIcon, spawnagent: BotIcon,
  dispatchagent: BotIcon, subagent: BotIcon,
  question: InfoIcon, ask: InfoIcon, askuser: InfoIcon, askuserquestion: InfoIcon,
  requestinput: InfoIcon, userinput: InfoIcon,
  screenshot: EyeIcon, computerscreenshot: EyeIcon, computer: EyeIcon, computeruse: EyeIcon,
  imagegenerate: ImageIcon, generateimage: ImageIcon,
  skill: PackageIcon,
};

function activityIcon(activity: RemoteActivity) {
  // Match a tool identifier, not arbitrary words in a descriptive title. Like
  // desktop, unfamiliar MCP names keep their integration glyph; semantic kinds
  // remain the fallback for localized or descriptive provider titles.
  const name = activity.title.trim().toLowerCase();
  if (/^[\w.:/-]+$/.test(name)) {
    if (name.includes("__")) return PackageIcon;
    if (/^(?:fintwind_)?browser[_.:/]/.test(name)) return GlobeIcon;
    if (/^github[_.:/]/.test(name)) return GithubIcon;
    if (/^git[_.:/]/.test(name)) return GitBranchIcon;
    const leaf = name.split(/[.:/]/).at(-1)!.replace(/[_-]/g, "");
    const icon = Object.hasOwn(TOOL_ICONS, leaf) ? TOOL_ICONS[leaf] : undefined;
    if (icon) return icon;
  }
  return Object.hasOwn(KIND_ICONS, activity.kind) ? KIND_ICONS[activity.kind]! : WrenchIcon;
}

function statusOf(activity: RemoteActivity) {
  if (activity.failed) {
    return { status: "error" as const, tone: "error", text: "失败" };
  }
  if (!activity.complete) {
    return { status: "running" as const, tone: "running", text: null };
  }
  return { status: undefined, tone: "done", text: null };
}

export function ReasoningActivityRow({
  activity,
}: {
  activity: RemoteActivity;
}) {
  const { status } = statusOf(activity);
  const working = !activity.complete && !activity.failed;
  const [manualExpanded, setManualExpanded] = React.useState<boolean | null>(
    null,
  );
  // Natural settle: open while running, settled (closed) once done, unless the
  // user explicitly toggled.
  const expanded = manualExpanded ?? working;

  const label = activity.failed ? "思考失败" : "思考";
  const heading = <><SparkleIcon /><span role="status" className={working ? "bui-reasoning-shimmer" : undefined}>{label}</span></>;

  if (!activity.text) {
    return <div className="bui-thinking-trigger" data-status={status}>{heading}</div>;
  }

  return (
    <div className="bui-think">
      <TraceCollapsible open={expanded} onOpenChange={setManualExpanded}>
        <CollapsibleTrigger data-slot="trace-collapsible-trigger" className="bui-thinking-trigger bui-press" data-status={status}>
          {heading}<ChevronDownIcon className="bui-thinking-chevron" />
        </CollapsibleTrigger>
        <TraceCollapsibleContent>
          {activity.truncated && <p>较早的思考已省略</p>}
          <div className="reasoning-text">{activity.text}</div>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </div>
  );
}

export function ToolActivityRow({ activity }: { activity: RemoteActivity }) {
  const { status, tone, text } = statusOf(activity);
  const Icon = activityIcon(activity);

  return (
    <TraceLine status={status} className="bui-tool">
      <TraceIcon><Icon /></TraceIcon>
      <TraceTitle>{activity.title || "工具调用"}</TraceTitle>
      {text && <TraceDescription className="bui-tool-status" data-tone={tone}>
        {text}
      </TraceDescription>}
    </TraceLine>
  );
}
