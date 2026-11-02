import { useEffect, useRef, useState, type ClipboardEvent } from "react";
import {
  ArrowUpIcon,
  CheckIcon,
  ChevronDownIcon,
  CloseIcon,
  ExpandIcon,
  PaperclipIcon,
  TerminalIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Drawer,
  DrawerContent,
  DrawerHeader,
  DrawerTitle,
} from "@/components/ui/drawer";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupTextarea,
} from "@/components/ui/input-group";
import { Spinner } from "@/components/ui/spinner";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
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
}: Props) {
  const [panel, setPanel] = useState<
    "model" | "options" | "commands" | "editor" | null
  >(null);
  const [options, setOptions] = useState<ComposerOptions | null>(null);
  const [optionsLoading, setOptionsLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [files, setFiles] = useState<PickedFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const active = useRef(true);
  const filesRef = useRef(files);
  filesRef.current = files;
  const sending = useRef(false);
  const configurable = session.features.includes("composerOptions");
  const attachmentsSupported = session.features.includes("attachments");
  const running = ["connecting", "working", "waiting"].includes(session.status);
  const locked = disabled || busy || uploading;

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
    if (!panel || panel === "editor" || !configurable || disabled) return;
    let current = true;
    setOptionsLoading(true);
    setOptions(null);
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
  }, [panel, session.id, configurable, disabled, onError]);

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

  const model = options?.models.find(
    (candidate) => candidate.id === session.model,
  );
  return (
    <div className="composer-region">
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
      {!!files.length && (
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
      )}
      {progress && (
        <p className="composer-notice" role="status">
          {progress}
        </p>
      )}
      {!configurable && (
        <p className="composer-notice">更新桌面端后可使用模型选项和附件。</p>
      )}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void send();
        }}
      >
        <FieldGroup>
          <Field>
            <FieldLabel className="sr-only" htmlFor="message-input">
              消息
            </FieldLabel>
            <InputGroup>
              <InputGroupTextarea
                id="message-input"
                placeholder={running ? "补充消息…" : "发送消息…"}
                rows={2}
                value={draft}
                maxLength={32768}
                disabled={uploading || busy}
                onPaste={paste}
                onChange={(event) => onDraft(event.target.value)}
              />
              <InputGroupAddon align="block-end">
                {attachmentsSupported && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    disabled={locked}
                    aria-label="添加图片或文件"
                    onClick={() => input.current?.click()}
                  >
                    <PaperclipIcon />
                  </Button>
                )}
                {configurable && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="model-chip"
                    disabled={locked}
                    onClick={() => {
                      setSearch("");
                      setPanel("model");
                    }}
                  >
                    <span className="truncate">
                      {session.modelName || "选择模型"}
                    </span>
                    <ChevronDownIcon data-icon="inline-end" />
                  </Button>
                )}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label="展开编辑"
                  disabled={uploading || busy}
                  onClick={() => setPanel("editor")}
                >
                  <ExpandIcon />
                </Button>
                <div className="composer-spacer" />
                {running ? (
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={locked}
                    onClick={() =>
                      void mutate("stop", { sessionId: session.id })
                    }
                  >
                    停止
                  </Button>
                ) : null}
                <Button
                  type="submit"
                  size="icon-sm"
                  disabled={
                    locked ||
                    !session.canSend ||
                    session.desktopRequired ||
                    (!draft.trim() && !files.length)
                  }
                  aria-label={running ? "加入待发送" : "发送消息"}
                >
                  {uploading || busy ? <Spinner /> : <ArrowUpIcon />}
                </Button>
              </InputGroupAddon>
            </InputGroup>
          </Field>
        </FieldGroup>
      </form>
      {configurable && (
        <div className="composer-options-bar">
          <Button
            variant="ghost"
            size="sm"
            disabled={locked}
            onClick={() => setPanel("options")}
          >
            {session.interactionMode === "plan" ? "Plan" : "Build"}
            <ChevronDownIcon data-icon="inline-end" />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={locked}
            onClick={() => setPanel("options")}
          >
            {session.reasoningEffort || "默认思考"}
          </Button>
          <div className="composer-spacer" />
          <Button
            variant="ghost"
            size="sm"
            disabled={locked}
            onClick={() => setPanel("commands")}
          >
            <TerminalIcon data-icon="inline-start" />
            命令
          </Button>
        </div>
      )}
      <Drawer
        open={panel !== null}
        onOpenChange={(open) => {
          if (!open) setPanel(null);
        }}
        showSwipeHandle
      >
        <DrawerContent>
          <DrawerHeader>
            <DrawerTitle>
              {panel === "model"
                ? "模型"
                : panel === "commands"
                  ? "命令"
                  : panel === "editor"
                    ? "编辑消息"
                    : "会话选项"}
            </DrawerTitle>
          </DrawerHeader>
          <div className="composer-panel">
            {panel === "editor" ? (
              <>
                <FieldGroup>
                  <Field>
                    <FieldLabel className="sr-only" htmlFor="expanded-message">
                      消息
                    </FieldLabel>
                    <InputGroup>
                      <InputGroupTextarea
                        id="expanded-message"
                        autoFocus
                        rows={12}
                        value={draft}
                        disabled={uploading || busy}
                        maxLength={32768}
                        onPaste={paste}
                        onChange={(event) => onDraft(event.target.value)}
                      />
                    </InputGroup>
                  </Field>
                </FieldGroup>
                <Button onClick={() => setPanel(null)}>完成</Button>
              </>
            ) : optionsLoading ? (
              <Spinner aria-label="正在加载选项" />
            ) : !options ? (
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
                      {options.models
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
                              locked || running || !options.canConfigure
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
                  <FieldGroup>
                    <Field>
                      <FieldLabel>模式</FieldLabel>
                      <ToggleGroup
                        value={[session.interactionMode || "build"]}
                        disabled={locked || running || !options.canConfigure}
                        onValueChange={(values) => {
                          if (values[0])
                            void configure({
                              interactionMode: String(values[0]),
                            });
                        }}
                      >
                        <ToggleGroupItem value="build">Build</ToggleGroupItem>
                        <ToggleGroupItem value="plan">Plan</ToggleGroupItem>
                      </ToggleGroup>
                    </Field>
                    {!!model?.efforts.length && (
                      <Field>
                        <FieldLabel>思考强度</FieldLabel>
                        <ToggleGroup
                          className="flex-wrap"
                          value={[session.reasoningEffort]}
                          disabled={locked || running || !options.canConfigure}
                          onValueChange={(values) => {
                            if (values[0])
                              void configure({
                                reasoningEffort: String(values[0]),
                              });
                          }}
                        >
                          {model.efforts.map((effort) => (
                            <ToggleGroupItem key={effort.id} value={effort.id}>
                              {effort.label}
                            </ToggleGroupItem>
                          ))}
                        </ToggleGroup>
                      </Field>
                    )}
                    {running && <p>运行结束后可修改会话选项。</p>}
                  </FieldGroup>
                )}
                {panel === "commands" && (
                  <div className="model-list">
                    {options.commands.length ? (
                      options.commands.map((command) => (
                        <Button
                          key={command.name}
                          variant="row"
                          disabled={locked}
                          onClick={() => {
                            onDraft(
                              `/${command.name.replace(/^\//, "")} ${draft}`,
                            );
                            setPanel(null);
                          }}
                        >
                          <span>
                            <strong>/{command.name.replace(/^\//, "")}</strong>
                            <small>{command.description}</small>
                          </span>
                        </Button>
                      ))
                    ) : (
                      <p>当前会话没有已登记命令。</p>
                    )}
                  </div>
                )}
              </>
            )}
          </div>
        </DrawerContent>
      </Drawer>
    </div>
  );
}
