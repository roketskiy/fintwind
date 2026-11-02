// Adapted from Beautiful UI by Shane Levine (MIT), revision 44a274e598395ab61e7c96c26fda2758780253b7.
// Source: components/primitives/ApprovalCard.tsx — question-as-heading,
// vertical option rows with a small indicator, compact step footer.
// Deliberately excluded: auto-advance timers, staged slide animation, dismiss
// and skip actions (they would bypass required-question validation), and the
// demo aria-pressed fake-radio rows. Option controls here are the existing
// accessible RadioGroup/Checkbox primitives with matching label/id pairs.
import * as React from "react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { RadioGroupItem } from "@/components/ui/radio-group";

function forwardRowClick(event: React.MouseEvent, id: string) {
  const target = event.target as HTMLElement | null;
  if (target?.closest("button,input,textarea,label,a,[role=checkbox],[role=radio]")) {
    return;
  }

  document.getElementById(id)?.click();
}

interface ApprovalOptionRowProps {
  id: string;
  selected: boolean;
  label: string;
  description?: string;
  control: React.ReactNode;
}

function ApprovalOptionRow({
  id,
  selected,
  label,
  description,
  control,
}: ApprovalOptionRowProps) {
  return (
    <div
      className="apc-opt"
      data-selected={selected || undefined}
      onClick={(event) => forwardRowClick(event, id)}
    >
      {control}
      <span className="apc-opt-text">
        <label className="apc-opt-label" htmlFor={id}>
          {label}
        </label>
        {description ? (
          <span className="apc-opt-desc">{description}</span>
        ) : null}
      </span>
    </div>
  );
}

export function ApprovalRadioOption({
  id,
  value,
  label,
  description,
  selected,
  disabled,
}: {
  id: string;
  value: string;
  label: string;
  description?: string;
  selected: boolean;
  disabled?: boolean;
}) {
  return (
    <ApprovalOptionRow
      id={id}
      selected={selected}
      label={label}
      description={description}
      control={
        <RadioGroupItem
          id={id}
          value={value}
          disabled={disabled}
          className="apc-indicator"
        />
      }
    />
  );
}

export function ApprovalCheckOption({
  id,
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: {
  id: string;
  label: string;
  description?: string;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <ApprovalOptionRow
      id={id}
      selected={checked}
      label={label}
      description={description}
      control={
        <Checkbox
          id={id}
          checked={checked}
          disabled={disabled}
          onCheckedChange={onCheckedChange}
          className="apc-indicator"
        />
      }
    />
  );
}

export function ApprovalAction({
  size = "sm",
  variant = "default",
  children,
  className,
  ...props
}: React.ComponentProps<typeof Button>) {
  return (
    <Button size={size} variant="ghost" {...props} data-apc-tone={variant} className={cn("apc-action", className)}>
      <span className="apc-action-face">{children}</span>
    </Button>
  );
}
