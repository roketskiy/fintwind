import { useEffect, useSyncExternalStore } from "react";
export type PhoneTheme = "system" | "light" | "dark";
const KEY = "fintwind-phone-theme";
const listeners = new Set<() => void>();
let current: PhoneTheme | undefined;
export function savedTheme(): PhoneTheme {
  try { const value = localStorage.getItem(KEY); if (value === "light" || value === "dark") return value; } catch { /* System remains usable without storage. */ }
  return "system";
}
export function applyTheme(value: PhoneTheme): void {
  const dark = value === "dark" || (value === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
  document.documentElement.dataset.theme = value;
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", dark ? "#151619" : "#fafafa");
}
export function usePhoneTheme() {
  const theme = useSyncExternalStore((listener) => { listeners.add(listener); return () => { listeners.delete(listener); }; }, () => current ??= savedTheme());
  useEffect(() => {
    applyTheme(theme);
    const media = matchMedia("(prefers-color-scheme: dark)");
    const update = () => applyTheme(theme);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [theme]);
  return { theme, setTheme: (value: PhoneTheme) => { try { localStorage.setItem(KEY, value); } catch { /* Current page still switches. */ } current = value; applyTheme(value); listeners.forEach((listener) => listener()); } };
}
