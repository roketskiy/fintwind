// Adapted from Beautiful UI PromptBar.tsx (MIT), revision
// 44a274e598395ab61e7c96c26fda2758780253b7, composer + controls section.
// Preserve its inset surface, compact controls and growing input. No demo,
// voice placeholder, canvas, hover tracking, or automatic send on Enter.
import { forwardRef, type ClipboardEventHandler, type KeyboardEventHandler, type ReactNode } from "react";
import { ArrowUpIcon, ChevronDownIcon, PlusIcon, StopIcon } from "@/components/icons";
import { PopoverTrigger } from "@/components/ui/popover";
import { Spinner } from "@/components/ui/spinner";
import "./prompt-bar.css";

interface Props {
  draft: string;
  placeholder: string;
  inputDisabled: boolean;
  locked: boolean;
  canSend: boolean;
  sending: boolean;
  running: boolean;
  stopDisabled: boolean;
  attachmentsSupported: boolean;
  configurable: boolean;
  modelName: string;
  mode: string;
  effort: string;
  commandsOpen: boolean;
  activeCommand?: string;
  attachments: ReactNode;
  onDraft: (draft: string) => void;
  onPaste: ClipboardEventHandler<HTMLTextAreaElement>;
  onKeyDown: KeyboardEventHandler<HTMLTextAreaElement>;
  onAttach: () => void;
  onModel: () => void;
  onOptions: () => void;
  onStop: () => void;
  onSend: () => void;
}

export const PromptBar = forwardRef<HTMLTextAreaElement, Props>(function PromptBar(props, ref) {
  return <form className="bui-prompt" onSubmit={(event) => { event.preventDefault(); props.onSend(); }}>
    {props.attachments}
    <label className="sr-only" htmlFor="message-input">消息</label>
    <textarea ref={ref} id="message-input" className="bui-prompt-input" rows={1}
      value={props.draft} placeholder={props.placeholder} maxLength={32768}
      disabled={props.inputDisabled} onPaste={props.onPaste} onKeyDown={props.onKeyDown}
      onChange={(event) => props.onDraft(event.target.value)} role="combobox"
      aria-autocomplete="list" aria-expanded={props.commandsOpen}
      aria-controls={props.commandsOpen ? "slash-commands" : undefined}
      aria-activedescendant={props.activeCommand} />
    <div className="bui-prompt-controls">
      {props.attachmentsSupported && <button type="button" className="bui-prompt-control bui-prompt-icon" disabled={props.locked}
        aria-label="添加图片或文件" onClick={props.onAttach}><span className="bui-prompt-surface"><PlusIcon /></span></button>}
      {props.configurable && <>
        <PopoverTrigger className="bui-prompt-control bui-prompt-model" disabled={props.locked}
          title={props.modelName || "选择模型"} aria-label={`选择模型：${props.modelName || "未选择"}`} onClick={props.onModel}>
          <span className="bui-prompt-model-name">{props.modelName || "选择模型"}</span><ChevronDownIcon />
        </PopoverTrigger>
        <PopoverTrigger className="bui-prompt-control bui-prompt-options" disabled={props.locked}
          aria-label={`会话选项：${props.mode}，${props.effort}`} onClick={props.onOptions}>
          <span>{props.mode}</span><span aria-hidden="true" className="bui-prompt-divider">·</span><span className="bui-prompt-effort">{props.effort}</span>
        </PopoverTrigger>
      </>}
      {!props.configurable && <span className="bui-prompt-model" />}
      {props.running && <button type="button" className="bui-prompt-control bui-prompt-icon" disabled={props.stopDisabled}
        aria-label="停止" onClick={props.onStop}><span className="bui-prompt-surface bui-prompt-stop"><StopIcon /></span></button>}
      <button type="submit" className="bui-prompt-control bui-prompt-icon bui-prompt-send" disabled={!props.canSend}
        aria-label={props.running ? "发送引导" : "发送消息"}>
        <span className="bui-prompt-surface">{props.sending ? <Spinner /> : <ArrowUpIcon />}</span>
      </button>
    </div>
  </form>;
});
