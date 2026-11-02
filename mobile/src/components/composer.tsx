// Prompt layout and token selection adapted from Beautiful UI PromptBar (MIT).
import { useEffect, useId, useRef, useState, type ClipboardEvent, type KeyboardEvent } from "react";
import {
  CheckIcon,
  CloseIcon,
  PaperclipIcon,
  TerminalIcon,
  SparkleIcon,
} from "@/components/icons";
import { GlideMenu, PixelLoader } from "@/components/beautiful/primitives";
import { Popover, PopoverContent, PopoverTitle } from "@/components/ui/popover";
import { PromptBar } from "@/components/beautiful/prompt-bar";
import { composerCommands, slashQuery, type ComposerCommand } from "@/lib/composer-commands";
import { Button } from "@/components/ui/button";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import "./beautiful/segmented-control.css";
import { remoteClient } from "@/lib/remote-client";
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
  onError: (value: string) => void;
  mutate: <K extends CommandType>(
    type: K,
    fields: CommandMap[K]["fields"],
  ) => Promise<CommandMap[K]["result"] | null>;
  previewOptions?: ComposerOptions;
}
type PickedFile = { id: string; file: File; preview?: string };

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
  onError,
  mutate,
  previewOptions,
}: Props) {
  const [panel, setPanel] = useState<
    "model" | "options" | null
  >(null);
  const [options, setOptions] = useState<ComposerOptions | null>(null);
  const [optionsLoading, setOptionsLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [files, setFiles] = useState<PickedFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const [highlight, setHighlight] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const active = useRef(true);
  const filesRef = useRef(files);
  filesRef.current = files;
  const sending = useRef(false);
  const configurable = session.features.includes("composerOptions");
  const optionsId = useId();
  const attachmentsSupported = session.features.includes("attachments");
  const running = ["connecting", "working", "waiting"].includes(session.status);
  const locked = disabled || busy || uploading;
  const query = slashQuery(draft);
  const commands = composerCommands(session, previewOptions ?? options);
  const matches = commands.filter((command) => command.name.toLowerCase().includes(query ?? ""));
  const commandOpen = query !== null && dismissed !== draft && !panel && !locked;

  useEffect(() => { setHighlight(0); }, [draft]);
  useEffect(() => {
    if (!commandOpen) return;
    document.getElementById(`slash-option-${highlight}`)?.scrollIntoView({ block: "nearest" });
  }, [highlight, commandOpen]);

  function chooseCommand(command: ComposerCommand) {
    onDraft(command.value);
    setDismissed(command.value);
    setPanel(null);
    textarea.current?.focus();
  }

  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (commandOpen && ["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"].includes(event.key)) {
      if (event.key === "Escape") { event.preventDefault(); setDismissed(draft); return; }
      if (!matches.length) return;
      event.preventDefault();
      if (event.key === "ArrowDown" || event.key === "ArrowUp") setHighlight((value) => (value + (event.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length);
      else chooseCommand(matches[Math.min(highlight, matches.length - 1)]);
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
    if ((!panel && query === null) || !configurable || disabled || previewOptions) return;
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
      session.desktopRequired ||
      (!draft.trim() && !files.length)
    )
      return;
    if (new TextEncoder().encode(draft).length > 32768) {
      onError("消息不能超过 32 KiB。");
      return;
    }
    sending.current = true;
    setUploading(true);
    const text = draft;
    try {
      const attachmentIds: string[] = [];
      for (const entry of files) {
        const encoded = await base64(entry.file);
        const uploadId = crypto.randomUUID();
        for (let offset = 0; offset < encoded.length; offset += 65536) {
          if (!active.current) return;
          setProgress(
            `${entry.file.name} · ${Math.round((offset / encoded.length) * 100)}%`,
          );
          const part = encoded.slice(offset, offset + 65536);
          await remoteClient.command("uploadChunk", {
            sessionId: session.id,
            uploadId,
            name: entry.file.name,
            offset,
            dataBase64: part,
            complete: offset + part.length === encoded.length,
          });
        }
        attachmentIds.push(uploadId);
      }
      if (!active.current) return;
      setProgress("");
      const result = await mutate("sendPrompt", {
        sessionId: session.id,
        text,
        ...(attachmentIds.length ? { attachmentIds } : {}),
      });
      if (result && active.current) {
        onDraft("");
        files.forEach((entry) => {
          if (entry.preview) URL.revokeObjectURL(entry.preview);
        });
        setFiles([]);
      }
    } catch (error) {
      if (active.current)
        onError(error instanceof Error ? error.message : "附件发送失败。");
    } finally {
      sending.current = false;
      if (active.current) {
        setUploading(false);
        setProgress("");
      }
    }
  }

  async function configure(
    fields: Omit<CommandMap["configureSession"]["fields"], "sessionId">,
  ) {
    const result = await mutate("configureSession", {
      sessionId: session.id,
      ...fields,
    });
    if (result && active.current) setPanel(null);
  }

  const catalog = previewOptions ?? options;
  const model = catalog?.models.find(
    (candidate) => candidate.id === session.model,
  );
  return (
    <Popover open={panel !== null} onOpenChange={(open) => { if (!open) setPanel(null); }}>
    <div className="composer-region">
      {commandOpen && (
        <div className="slash-palette">
          <div className="palette-heading"><TerminalIcon /><span>命令</span><kbd>esc</kbd></div>
          <GlideMenu className="slash-results">
            <div id="slash-commands" role="listbox" aria-label="斜杠命令">
              {matches.map((command, index) => <Button type="button" variant="row" key={command.name}
                id={`slash-option-${index}`} role="option" aria-selected={index === Math.min(highlight, matches.length - 1)}
                tabIndex={-1} data-menu-row data-highlighted={index === Math.min(highlight, matches.length - 1) || undefined}
                onPointerDown={(event) => event.preventDefault()} onClick={() => chooseCommand(command)}>
                <span className="command-symbol">{command.name.startsWith("goal") ? <SparkleIcon /> : <TerminalIcon />}</span>
                <span className="command-copy"><strong>/{command.name}</strong><small>{command.description}</small></span>
                <span aria-hidden="true">↵</span>
              </Button>)}
              {!matches.length && <p className="palette-empty">{optionsLoading ? "正在加载命令…" : "没有匹配的命令"}</p>}
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
        draft={draft} placeholder={running ? "补充消息…" : "发送消息…"}
        inputDisabled={uploading || busy} locked={locked}
        canSend={!locked && session.canSend && !session.desktopRequired && (!!draft.trim() || !!files.length)}
        sending={uploading || busy} running={running} attachmentsSupported={attachmentsSupported}
        configurable={configurable} modelName={session.modelName}
        mode={session.interactionMode === "plan" ? "Plan" : "Build"} effort={session.reasoningEffort || "默认思考"}
        commandsOpen={commandOpen}
        activeCommand={commandOpen && matches.length ? `slash-option-${Math.min(highlight, matches.length - 1)}` : undefined}
        onDraft={(value) => { setDismissed(null); onDraft(value); }} onPaste={paste} onKeyDown={keyDown}
        onAttach={() => input.current?.click()}
        onModel={() => { setSearch(""); setPanel("model"); }} onOptions={() => setPanel("options")}
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
                <small>{(entry.file.size / 1024).toFixed(0)} KiB</small>
              </span>
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
      {progress && (
        <p className="composer-notice" role="status">
          {progress}
        </p>
      )}
      {!configurable && (
        <p className="composer-notice">更新桌面端后可使用模型选项和附件。</p>
      )}
      <PopoverContent side="top" align="start" sideOffset={8} className="composer-popover">
        <PopoverTitle>{panel === "model" ? "模型" : "会话选项"}</PopoverTitle>
        <div className="composer-panel">
            {optionsLoading && !catalog ? (
              <PixelLoader label="正在加载选项" />
            ) : !catalog ? (
              <p>无法加载选项，请关闭后重试。</p>
            ) : (
              <>
                {panel === "model" && (
                  <>
                    <FieldGroup>
                      <Field>
                        <FieldLabel className="sr-only" htmlFor="model-search">
                          搜索模型
                        </FieldLabel>
                        <Input
                          id="model-search"
                          placeholder="搜索模型或提供商"
                          value={search}
                          onChange={(event) => setSearch(event.target.value)}
                        />
                      </Field>
                    </FieldGroup>
                    <div className="model-list">
                      {catalog.models
                        .filter((item) =>
                          `${item.name} ${item.provider} ${item.id}`
                            .toLowerCase()
                            .includes(search.toLowerCase()),
                        )
                        .map((item) => (
                          <Button
                            key={item.id}
                            variant="row"
                            disabled={
                              locked || running || !catalog.canConfigure
                            }
                            onClick={() => void configure({ model: item.id })}
                          >
                            <span>
                              <strong>{item.name}</strong>
                              <small>{item.provider || item.id}</small>
                            </span>
                            {item.id === session.model && (
                              <CheckIcon data-icon="inline-end" />
                            )}
                          </Button>
                        ))}
                    </div>
                  </>
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
