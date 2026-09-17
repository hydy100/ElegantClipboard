import { useCallback, useEffect, useRef } from "react";

/** Independent pending writes per field; flush on tab switch or page close. */
export function useKeyedDebounce<K>(delay = 300) {
  const pending = useRef(new Map<K, { timer: ReturnType<typeof setTimeout>; write: () => void }>());
  const flush = useCallback(() => {
    const writes = [...pending.current.values()];
    pending.current.clear();
    for (const { timer, write } of writes) {
      clearTimeout(timer);
      write();
    }
  }, []);
  useEffect(() => {
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, [flush]);
  return useCallback((key: K, write: () => void) => {
    const previous = pending.current.get(key);
    if (previous) clearTimeout(previous.timer);
    const timer = setTimeout(() => {
      pending.current.delete(key);
      write();
    }, delay);
    pending.current.set(key, { timer, write });
  }, [delay]);
}
