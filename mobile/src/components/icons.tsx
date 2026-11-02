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

function embeddedIcon(source: string) {
  return function EmbeddedIcon(props: HTMLAttributes<HTMLSpanElement>) {
    return (
      <span
        aria-hidden="true"
        {...props}
        className={cn("app-icon", props.className)}
        dangerouslySetInnerHTML={{ __html: source }}
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
