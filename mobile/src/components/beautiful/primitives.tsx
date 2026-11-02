// Adapted from Beautiful UI by Shane Levine (MIT), revision 44a274e598395ab61e7c96c26fda2758780253b7.
// Source: components/primitives/GlideMenu.tsx, LoadingState.tsx and TaskRows.tsx.
// Demo timers, synthetic task states, external media and continuous canvas effects are intentionally excluded.
import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

export function GlideMenu({ children, className }: { children: ReactNode; className?: string }) {
  // Preserve the shared menu layout, without hover tracking or a moving highlight.
  return <div className={cn("bui-glide", className)}>{children}</div>;
}

export function PixelLoader({ label = "正在处理", compact = false }: { label?: string; compact?: boolean }) {
  return <span className={cn("bui-loader", compact && "bui-loader-compact")} role="status">
    <span className="bui-pixels" aria-hidden="true">{Array.from({ length: 9 }, (_, index) => <i key={index} style={{ animationDelay: `${(index % 3 + Math.abs(Math.floor(index / 3) - 1)) * 90}ms` }} />)}</span>
    {!compact && <span className="bui-shimmer">{label}</span>}
    {compact && <span className="sr-only">{label}</span>}
  </span>;
}
