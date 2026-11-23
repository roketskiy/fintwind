// Prompt layout and token selection adapted from Beautiful UI PromptBar (MIT).
import { useEffect, useId, useRef, useState, type ClipboardEvent, type KeyboardEvent } from "react";
import {
  CloseIcon,
  PaperclipIcon,
} from "@/components/icons";
import { GlideMenu } from "@/components/beautiful/primitives";
import { Popover, PopoverContent, PopoverTitle } from "@/components/ui/popover";
import { PromptBar } from "@/components/beautiful/prompt-bar";
import { composerCommands, slashQuery, type ComposerCommand } from "@/lib/composer-commands";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { ModelPicker } from "@/components/model-picker";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Skeleton } from "@/components/ui/skeleton";
import { useDelayedLoading } from "@/hooks/use-delayed-loading";
import "./beautiful/segmented-control.css";
import { remoteClient } from "@/lib/remote-client";
import { errorMessage } from "@/lib/error-message";
import type {
  CommandMap,
  CommandType,
  ComposerOptions,
  SessionDetail,
} from "@/lib/types";

interface Props {
  session: SessionDetail;
  draft: string;
  onDraft: (value: string) => void;
  disabled: boolean;
  busy: boolean;
  stopDisabled?: boolean;
  onError: (value: string) => void;
  mutate: <K extends CommandType>(
    type: K,
    fields: CommandMap[K]["fields"],
    onFailure?: (message: string) => void,
  ) => Promise<CommandMap[K]["result"] | null>;
  previewOptions?: ComposerOptions;
}
type PickedFile = { id: string; file: File; preview?: string };
type UploadProgress = { id: string; name: string; sent: number; total: number };

function base64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",")[1] || "");
    reader.onerror = () => reject(new Error("无法读取附件。"));
    reader.readAsDataURL(file);
  });
}

export function Composer({
  session,
  draft,
  onDraft,
  disabled,
  busy,
  stopDisabled,
  onError,
  mutate,
  previewOptions,
}: Props) {
  const [panel, setPanel] = useState<
    "model" | "options" | null
  >(null);
  const [options, setOptions] = useState<ComposerOptions | null>(null);
  const [optionsLoading, setOptionsLoading] = useState(false);
  const [files, setFiles] = useState<PickedFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState<UploadProgress | null>(null);
  const lastProgress = useRef<UploadProgress | null>(null);
  const [deliveryError, setDeliveryError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const [highlight, setHighlight] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [caret, setCaret] = useState(0);
  const [fileResults, setFileResults] = useState<{ query: string; files: string[]; error?: string } | null>(null);
  const active = useRef(true);
  const filesRef = useRef(files);
  filesRef.current = files;
  const sending = useRef(false);
  const configurable = session.features.includes("composerOptions");
  const optionsId = useId();
  const attachmentsSupported = session.features.includes("attachments");
  const running = ["connecting", "working", "waiting"].includes(session.status);
  const canSteer = session.features.includes("steering") && session.canSteer === true;
  const locked = disabled || busy || uploading;
  const query = slashQuery(draft);
  const commands = composerCommands(session, previewOptions ?? options);
  const matches = commands.filter((command) => command.name.toLowerCase().includes(query ?? ""));
  const commandOpen = query !== null && dismissed !== draft && !panel && !locked;
  const mentionMatch = /(?:^|\s)@([^\s@]*)$/.exec(draft.slice(0, caret));
  const mentionQuery = mentionMatch?.[1] ?? null;
  const mentionOpen = mentionQuery !== null && dismissed !== draft && !panel && !locked && query === null;
  const fileMatches = fileResults?.query === mentionQuery ? fileResults.files : [];
  const menuOpen = commandOpen || mentionOpen;

  useEffect(() => { setHighlight(0); }, [draft]);
  useEffect(() => {
    if (!menuOpen) return;
    document.getElementById(`slash-option-${highlight}`)?.scrollIntoView({ block: "nearest" });
  }, [highlight, menuOpen]);

  useEffect(() => {
    if (!mentionOpen || mentionQuery === null) return;
    let current = true;
    const timer = window.setTimeout(() => {
      if (previewOptions) {
        const files = ["src/app.tsx", "src/components/conversation.tsx", "README.md"];
        setFileResults({ query: mentionQuery, files: files.filter((file) => file.toLowerCase().includes(mentionQuery.toLowerCase())) });
        return;
      }
      void remoteClient.command("searchFiles", { sessionId: session.id, query: mentionQuery })
        .then(({ files }) => { if (current) setFileResults({ query: mentionQuery, files }); })
        .catch((error: unknown) => { if (current) setFileResults({ query: mentionQuery, files: [], error: errorMessage(error instanceof Error ? error.message : "无法搜索文件。") }); });
    }, 200);
    return () => { current = false; window.clearTimeout(timer); };
  }, [mentionOpen, mentionQuery, session.id, previewOptions]);

  function chooseFile(file: string) {
    if (mentionQuery === null) return;
    const start = caret - mentionQuery.length - 1;
    const insert = `@${file} `;
    const value = draft.slice(0, start) + insert + draft.slice(caret);
    const nextCaret = start + insert.length;
    onDraft(value);
    setDismissed(value);
    setCaret(nextCaret);
    requestAnimationFrame(() => { textarea.current?.focus(); textarea.current?.setSelectionRange(nextCaret, nextCaret); });
  }

  function chooseCommand(command: ComposerCommand) {
    onDraft(command.value);
    setDismissed(command.value);
    setPanel(null);
    textarea.current?.focus();
  }

  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (menuOpen && ["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"].includes(event.key)) {
      if (event.key === "Escape") { event.preventDefault(); setDismissed(draft); return; }
      const count = commandOpen ? matches.length : fileMatches.length;
      if (!count) return;
      event.preventDefault();
      if (event.key === "ArrowDown" || event.key === "ArrowUp") setHighlight((value) => (value + (event.key === "ArrowDown" ? 1 : -1) + count) % count);
      else if (commandOpen) chooseCommand(matches[Math.min(highlight, count - 1)]);
      else chooseFile(fileMatches[Math.min(highlight, count - 1)]);
    } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault(); void send();
    }
  }

  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      filesRef.current.forEach((entry) => {
        if (entry.preview) URL.revokeObjectURL(entry.preview);
      });
    };
  }, []);

  useEffect(() => {
    if ((!panel && query === null) || !configurable || disabled || previewOptions) {
      setOptionsLoading(false);
      return;
    }
    let current = true;
    setOptionsLoading(true);
    void remoteClient
      .command("getComposer", { sessionId: session.id })
      .then((result) => {
        if (current) setOptions(result);
      })
      .catch((error: unknown) => {
        if (current)
          onError(error instanceof Error ? error.message : "无法加载选项。");
      })
      .finally(() => {
        if (current) setOptionsLoading(false);
      });
    return () => {
      current = false;
    };
  }, [panel, query !== null, session.id, configurable, disabled, onError, previewOptions]);

  function pick(incoming: File[]) {
    if (locked || !attachmentsSupported) return;
    if (
      incoming.some(
        (file) =>
          !file.size ||
          file.size > 2 * 1024 * 1024 ||
          new TextEncoder().encode(file.name).length > 128,
      ) ||
      files.length + incoming.length > 8 ||
      [...files.map((entry) => entry.file), ...incoming].reduce(
        (sum, file) => sum + file.size,
        0,
      ) >
        8 * 1024 * 1024
    ) {
      onError("每个附件不超过 2 MiB，最多 8 个，合计不超过 8 MiB。");
      return;
    }
    setFiles((previous) => [
      ...previous,
      ...incoming.map((file) => ({
        id: crypto.randomUUID(),
        file,
        preview: /^image\/(png|jpeg|webp|gif)$/.test(file.type)
          ? URL.createObjectURL(file)
          : undefined,
      })),
    ]);
  }

  function paste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (event.clipboardData.files.length && attachmentsSupported) {
      event.preventDefault();
      pick(Array.from(event.clipboardData.files));
    }
  }

  async function send() {
    if (
      locked ||
      sending.current ||
      !session.canSend ||
      (running && !canSteer) ||
      session.desktopRequired ||
      (!draft.trim() && !files.length)
    )
      return;
    if (new TextEncoder().encode(draft).length > 32768) {
      onError("消息不能超过 32 KiB。");
      return;
    }
    sending.current = true;
    setDeliveryError(null);
    setUploading(true);
    const text = draft;
    const steering = running;
    try {
      const attachmentIds: string[] = [];
      for (const entry of files) {
        const encoded = await base64(entry.file);
        const uploadId = crypto.randomUUID();
        setProgress({ id: entry.id, name: entry.file.name, sent: 0, total: encoded.length });
        for (let offset = 0; offset < encoded.length; offset += 65536) {
          if (!active.current) return;
          const part = encoded.slice(offset, offset + 65536);
          await remoteClient.command("uploadChunk", {
            sessionId: session.id,
            uploadId,
            name: entry.file.name,
            offset,
            dataBase64: part,
            complete: offset + part.length === encoded.length,
          });
          if (active.current) setProgress({ id: entry.id, name: entry.file.name, sent: offset + part.length, total: encoded.length });
        }
        attachmentIds.push(uploadId);
      }
      if (!active.current) return;
      setProgress(null);
      const result = await mutate(steering ? "steerPrompt" : "sendPrompt", {
        sessionId: session.id,
        text,
        ...(attachmentIds.length ? { attachmentIds } : {}),
      }, (message) => { if (active.current) setDeliveryError(message); });
      if (result && active.current) {
        onDraft("");
        files.forEach((entry) => {
          if (entry.preview) URL.revokeObjectURL(entry.preview);
        });
        setFiles([]);
      }
    } catch (error) {
      if (active.current)
        setDeliveryError(error instanceof Error ? error.message : "消息发送失败。");
    } finally {
      sending.current = false;
      if (active.current) {
        setUploading(false);
        setProgress(null);
      }
    }
  }

  async function configure(
    fields: Omit<CommandMap["configureSession"]["fields"], "sessionId">,
  ) {
    await mutate("configureSession", {
      sessionId: session.id,
      ...fields,
    });
  }

  const catalog = previewOptions ?? options;
  const showOptionsSkeleton = useDelayedLoading(optionsLoading && !catalog && panel !== null);
  const showUploadProgress = useDelayedLoading(progress !== null);
  const showSendSpinner = useDelayedLoading((uploading || busy) && progress === null);
  if (progress) lastProgress.current = progress;
  const visibleProgress = showUploadProgress && files.some((entry) => entry.id === lastProgress.current?.id)
    ? lastProgress.current : null;
  const model = catalog?.models.find(
    (candidate) => candidate.id === session.model,
  );
  return (
    <Popover open={panel !== null} onOpenChange={(open) => { if (!open) setPanel(null); }}>
    <div className="composer-region">
      {menuOpen && (
        <div className="slash-palette">
          <div className="palette-heading"><span>{commandOpen ? "命令" : "文件"}</span><kbd>esc</kbd></div>
          <GlideMenu className="slash-results">
            <div id="slash-commands" role="listbox" aria-label={commandOpen ? "斜杠命令" : "文件"}>
              {commandOpen && matches.map((command, index) => <Button type="button" variant="row" key={command.name}
                id={`slash-option-${index}`} role="option" aria-selected={index === Math.min(highlight, matches.length - 1)}
                tabIndex={-1} data-menu-row data-highlighted={index === Math.min(highlight, matches.length - 1) || undefined}
                onPointerDown={(event) => event.preventDefault()} onClick={() => chooseCommand(command)}>
                <span className="command-copy"><strong>/{command.name}</strong><small>{command.description}</small></span>
                <span aria-hidden="true">↵</span>
              </Button>)}
              {mentionOpen && fileMatches.map((file, index) => <Button type="button" variant="row" key={file}
                id={`slash-option-${index}`} role="option" aria-selected={index === Math.min(highlight, fileMatches.length - 1)}
                tabIndex={-1} data-menu-row data-highlighted={index === Math.min(highlight, fileMatches.length - 1) || undefined}
                onPointerDown={(event) => event.preventDefault()} onClick={() => chooseFile(file)}>
                <span className="command-copy"><strong>{file}</strong></span>
              </Button>)}
              {commandOpen && !matches.length && !optionsLoading && <p className="palette-empty">没有匹配的命令</p>}
              {mentionOpen && fileResults?.query === mentionQuery && !fileMatches.length && <p className="palette-empty">{fileResults.error || "没有匹配的文件"}</p>}
            </div>
          </GlideMenu>
        </div>
      )}
      <input
        type="file"
        multiple
        ref={input}
        className="sr-only"
        tabIndex={-1}
        aria-label="选择图片或文件"
        onChange={(event) => {
          pick(Array.from(event.target.files || []));
          event.target.value = "";
        }}
      />
      <PromptBar ref={textarea}
        draft={draft} placeholder={running ? "引导当前回复…" : "发送消息…"}
        inputDisabled={uploading || busy} locked={locked}
        canSend={!locked && session.canSend && (!running || canSteer) && !session.desktopRequired && (!!draft.trim() || !!files.length)}
        sending={showSendSpinner && !visibleProgress} running={running} stopDisabled={stopDisabled ?? locked} attachmentsSupported={attachmentsSupported}
        configurable={configurable} modelName={session.modelName}
        mode={session.interactionMode === "plan" ? "Plan" : "Build"} effort={session.reasoningEffort || "默认思考"}
        commandsOpen={menuOpen}
        activeCommand={menuOpen && (commandOpen ? matches.length : fileMatches.length) ? `slash-option-${Math.min(highlight, (commandOpen ? matches.length : fileMatches.length) - 1)}` : undefined}
        onCaret={setCaret}
        onDraft={(value) => { setDismissed(null); setDeliveryError(null); onDraft(value); }} onPaste={paste} onKeyDown={keyDown}
        onAttach={() => input.current?.click()}
        onModel={() => setPanel("model")} onOptions={() => setPanel("options")}
        onStop={() => { void mutate("stop", { sessionId: session.id }); }} onSend={() => { void send(); }}
        attachments={!!files.length && (
        <div className="attachment-tray">
          {files.map((entry) => (
            <div className="attachment-chip" key={entry.id}>
              {entry.preview ? (
                <img src={entry.preview} alt="" />
              ) : (
                <PaperclipIcon />
              )}
              <span>
                <strong>{entry.file.name}</strong>
                <small>{visibleProgress?.id === entry.id
                  ? `${Math.round(visibleProgress.sent / visibleProgress.total * 100)}%`
                  : `${(entry.file.size / 1024).toFixed(0)} KiB`}</small>
              </span>
              {visibleProgress?.id === entry.id && (
                <progress className="attachment-upload-progress" aria-label={`上传 ${visibleProgress.name}`} value={visibleProgress.sent} max={visibleProgress.total} />
              )}
              <Button
                variant="ghost"
                size="icon-sm"
                disabled={uploading || busy}
                aria-label={`移除 ${entry.file.name}`}
                onClick={() => {
                  if (entry.preview) URL.revokeObjectURL(entry.preview);
                  setFiles((previous) =>
                    previous.filter((file) => file.id !== entry.id),
                  );
                }}
              >
                <CloseIcon />
              </Button>
            </div>
          ))}
        </div>
      )} />
      {deliveryError && <Alert variant="destructive"><AlertDescription className="error-reason">{errorMessage(deliveryError)}</AlertDescription></Alert>}
      {!configurable && (
        <p className="composer-notice">更新桌面端后可使用模型选项和附件。</p>
      )}
      <PopoverContent side="top" align="start" sideOffset={8} className={panel === "model" ? "composer-popover model-picker-popup" : "composer-popover"}>
        <PopoverTitle className={panel === "model" ? "sr-only" : undefined}>{panel === "model" ? "选择模型" : "会话选项"}</PopoverTitle>
        <div className="composer-panel">
            {(optionsLoading && !catalog) || showOptionsSkeleton ? (
              <div className="flex min-h-24 flex-col gap-3 p-3" role="status" aria-label="正在加载选项">
                {showOptionsSkeleton && [0, 1, 2].map((index) => (
                  <Skeleton key={index} className="h-4 w-3/4 rounded-md" />
                ))}
              </div>
            ) : !catalog ? (
              <p>无法加载选项，请关闭后重试。</p>
            ) : (
              <>
                {panel === "model" && (
                  <ModelPicker catalog={catalog} selected={session.model} disabled={locked || running || !catalog.canConfigure}
                    onSelect={async model => {
                      let failure = "无法切换模型，请重试。";
                      const result = await mutate("configureSession", { sessionId: session.id, model }, message => { failure = errorMessage(message); });
                      if (result && active.current) { setPanel(null); return null; }
                      return failure;
                    }} />
                )}
                {panel === "options" && (
                  <FieldGroup className="session-options">
                    <Field orientation="horizontal" className="session-option-row">
                      <FieldLabel id={`${optionsId}-mode`}>模式</FieldLabel>
                      <ToggleGroup
                        className="bui-segments" spacing={0} size="sm"
                        aria-labelledby={`${optionsId}-mode`}
                        value={[session.interactionMode || "build"]}
                        disabled={locked || running || !catalog.canConfigure}
                        onValueChange={(values) => {
                          if (values[0])
                            void configure({
                              interactionMode: String(values[0]),
                            });
                        }}
                      >
                        <ToggleGroupItem value="build"><span className="bui-segment-face">Build</span></ToggleGroupItem>
                        <ToggleGroupItem value="plan"><span className="bui-segment-face">Plan</span></ToggleGroupItem>
                      </ToggleGroup>
                    </Field>
                    {session.features.includes("sessionPermissions") && session.runtimeMode !== undefined && (
                      <Field orientation="horizontal" className="session-option-row">
                        <FieldLabel id={`${optionsId}-permissions`}>权限</FieldLabel>
                        <ToggleGroup
                          className="bui-segments" spacing={0} size="sm"
                          aria-labelledby={`${optionsId}-permissions`}
                          value={[session.runtimeMode]}
                          disabled={locked || running || !catalog.canConfigure}
                          onValueChange={(values) => {
                            const runtimeMode = values[0];
                            if (runtimeMode === "ask" || runtimeMode === "autoAcceptEdits" || runtimeMode === "fullAccess")
                              void configure({ runtimeMode });
                          }}
                        >
                          <ToggleGroupItem value="ask"><span className="bui-segment-face">监督</span></ToggleGroupItem>
                          <ToggleGroupItem value="autoAcceptEdits"><span className="bui-segment-face">自动接受修改</span></ToggleGroupItem>
                          <ToggleGroupItem value="fullAccess"><span className="bui-segment-face">完全访问</span></ToggleGroupItem>
                        </ToggleGroup>
                      </Field>
                    )}
                    {!!model?.efforts.length && (
                      <Field orientation="horizontal" className="session-option-row">
                        <FieldLabel id={`${optionsId}-effort`}>思考强度</FieldLabel>
                        <ToggleGroup
                          className="bui-segments" spacing={0} size="sm"
                          aria-labelledby={`${optionsId}-effort`}
                          value={[session.reasoningEffort]}
                          disabled={locked || running || !catalog.canConfigure}
                          onValueChange={(values) => {
                            if (values[0])
                              void configure({
                                reasoningEffort: String(values[0]),
                              });
                          }}
                        >
                          {model.efforts.map((effort) => (
                            <ToggleGroupItem key={effort.id} value={effort.id}>
                              <span className="bui-segment-face">{effort.label}</span>
                            </ToggleGroupItem>
                          ))}
                        </ToggleGroup>
                      </Field>
                    )}
                    {running && <p className="session-options-status">运行结束后可修改会话选项。</p>}
                  </FieldGroup>
                )}
              </>
            )}
          </div>
      </PopoverContent>
    </div>
    </Popover>
  );
}
