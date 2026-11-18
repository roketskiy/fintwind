import { useEffect, useRef, useState } from "react";

export function useDelayedLoading(active: boolean, key = ""): boolean {
  const [visibleKey, setVisibleKey] = useState<string | null>(null);
  const shownAt = useRef<number | null>(null);
  const currentKey = useRef(key);

  useEffect(() => {
    if (currentKey.current !== key) {
      currentKey.current = key;
      shownAt.current = null;
      setVisibleKey(null);
    }
    if (active) {
      if (shownAt.current !== null) return;
      const timer = window.setTimeout(() => {
        shownAt.current = performance.now();
        setVisibleKey(key);
      }, 300);
      return () => window.clearTimeout(timer);
    }

    if (shownAt.current === null) return;
    const remaining = Math.max(0, 500 - (performance.now() - shownAt.current));
    const timer = window.setTimeout(() => {
      shownAt.current = null;
      setVisibleKey(null);
    }, remaining);
    return () => window.clearTimeout(timer);
  }, [active, key]);

  return visibleKey === key;
}
