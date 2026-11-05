import { useEffect, useSyncExternalStore } from "react";
export type PhoneTheme = "auto" | "light" | "dark";
const KEY = "fintwind-phone-theme";
const listeners = new Set<() => void>();
let current: PhoneTheme | undefined;
export function savedTheme(): PhoneTheme {
  try { const value = localStorage.getItem(KEY); if (value === "light" || value === "dark") return value; } catch { /* Automatic mode remains usable without storage. */ }
  // The previous "system" value becomes automatic mode too.
  return "auto";
}
export function applyTheme(value: PhoneTheme): void {
  const hour = new Date().getHours();
  const dark = value === "dark" || (value === "auto" && (hour < 7 || hour >= 19));
  document.documentElement.classList.toggle("dark", dark);
  document.documentElement.dataset.theme = value;
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", dark ? "#151619" : "#fafafa");
}
export function usePhoneTheme() {
  const theme = useSyncExternalStore((listener) => { listeners.add(listener); return () => { listeners.delete(listener); }; }, () => current ??= savedTheme());
  useEffect(() => {
    applyTheme(theme);
    if (theme !== "auto") return;
    let timer: number;
    const update = () => applyTheme(current ?? theme);
    const schedule = () => {
      const now = new Date();
      const elapsed = now.getSeconds() * 1000 + now.getMilliseconds();
      timer = window.setTimeout(() => { update(); schedule(); }, 60_000 - elapsed);
    };
    const onVisibility = () => { if (document.visibilityState === "visible") update(); };
    schedule();
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("focus", update);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", update);
    };
  }, [theme]);
  return { theme, setTheme: (value: PhoneTheme) => { try { localStorage.setItem(KEY, value); } catch { /* Current page still switches. */ } current = value; applyTheme(value); listeners.forEach((listener) => listener()); } };
}
