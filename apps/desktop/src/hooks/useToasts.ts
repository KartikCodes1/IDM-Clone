import { useCallback, useEffect, useRef, useState } from "react";

export interface Toast {
  id: number;
  tone: "info" | "error";
  message: string;
}

/** How long a toast stays; errors stay longer so they can be read. */
const TOAST_MS = { info: 4000, error: 8000 } as const;

/** Short-lived messages. Text only; callers pass fixed or user-facing strings. */
export function useToasts(): {
  toasts: Toast[];
  push: (tone: Toast["tone"], message: string) => void;
  dismiss: (id: number) => void;
} {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(1);
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());

  const dismiss = useCallback((id: number) => {
    setToasts((current) => current.filter((t) => t.id !== id));
  }, []);

  const push = useCallback(
    (tone: Toast["tone"], message: string) => {
      const id = nextId.current++;
      setToasts((current) => [...current.slice(-3), { id, tone, message }]);
      const timer = setTimeout(() => {
        timers.current.delete(timer);
        dismiss(id);
      }, TOAST_MS[tone]);
      timers.current.add(timer);
    },
    [dismiss],
  );

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending) clearTimeout(timer);
      pending.clear();
    };
  }, []);

  return { toasts, push, dismiss };
}
