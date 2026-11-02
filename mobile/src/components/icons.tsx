import type { HTMLAttributes } from "react";
import { cn } from "@/lib/utils";
import check from "../../../assets/icons/check.svg?raw";
import copy from "../../../assets/icons/copy.svg?raw";
import down from "../../../assets/icons/arrow-down.svg?raw";
import back from "../../../assets/icons/arrow-left.svg?raw";
import up from "../../../assets/icons/arrow-up.svg?raw";
import chevron from "../../../assets/icons/chevron-down.svg?raw";
import next from "../../../assets/icons/chevron-right.svg?raw";
import plus from "../../../assets/icons/plus.svg?raw";
import folder from "../../../assets/icons/folder.svg?raw";
import message from "../../../assets/icons/message-square-text.svg?raw";
import lock from "../../../assets/icons/lock.svg?raw";
import laptop from "../../../assets/icons/laptop.svg?raw";
import loading from "../../../assets/icons/loader-circle.svg?raw";
import search from "../../../assets/icons/search.svg?raw";
import close from "../../../assets/icons/x.svg?raw";
import paperclip from "../../../assets/icons/paperclip.svg?raw";
import terminal from "../../../assets/icons/terminal.svg?raw";
import settings from "../../../assets/icons/settings.svg?raw";
import target from "../../../assets/icons/target.svg?raw";
import sparkle from "../../../assets/icons/sparkle.svg?raw";
import pause from "../../../assets/icons/pause.svg?raw";
import play from "../../../assets/icons/play.svg?raw";
import bot from "../../../assets/icons/bot.svg?raw";
import trash from "../../../assets/icons/trash.svg?raw";
import stop from "../../../assets/icons/stop.svg?raw";

function embeddedIcon(source: string) {
  const themed = source.replace(/(?:#000000|#000|\bblack\b)/gi, "currentColor");
  return function EmbeddedIcon(props: HTMLAttributes<HTMLSpanElement>) {
    return (
      <span
        aria-hidden="true"
        {...props}
        className={cn("app-icon", props.className)}
        dangerouslySetInnerHTML={{ __html: themed }}
      />
    );
  };
}

export const CheckIcon = embeddedIcon(check);
export const CopyIcon = embeddedIcon(copy);
export const ArrowDownIcon = embeddedIcon(down);
export const ArrowLeftIcon = embeddedIcon(back);
export const ArrowUpIcon = embeddedIcon(up);
export const ChevronDownIcon = embeddedIcon(chevron);
export const ChevronRightIcon = embeddedIcon(next);
export const PlusIcon = embeddedIcon(plus);
export const FolderIcon = embeddedIcon(folder);
export const MessageIcon = embeddedIcon(message);
export const LockIcon = embeddedIcon(lock);
export const LaptopIcon = embeddedIcon(laptop);
export const LoaderIcon = embeddedIcon(loading);
export const SearchIcon = embeddedIcon(search);
export const CloseIcon = embeddedIcon(close);
export const PaperclipIcon = embeddedIcon(paperclip);
export const TerminalIcon = embeddedIcon(terminal);
export const SettingsIcon = embeddedIcon(settings);
export const TargetIcon = embeddedIcon(target);
export const SparkleIcon = embeddedIcon(sparkle);
export const PauseIcon = embeddedIcon(pause);
export const PlayIcon = embeddedIcon(play);
export const BotIcon = embeddedIcon(bot);
export const TrashIcon = embeddedIcon(trash);
export const StopIcon = embeddedIcon(stop);
