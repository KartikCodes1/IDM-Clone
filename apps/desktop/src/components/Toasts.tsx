import type { Toast } from "../hooks/useToasts";

interface Props {
  toasts: Toast[];
  onDismiss: (id: number) => void;
}

/** Floating messages; text only. Errors use role=alert so screen readers announce them. */
export function Toasts({ toasts, onDismiss }: Props) {
  return (
    <div className="toasts">
      {toasts.map((toast) => (
        <div key={toast.id} className={`toast toast-${toast.tone}`} role={toast.tone === "error" ? "alert" : "status"}>
          <span>{toast.message}</span>
          <button type="button" className="toast-close" aria-label="Dismiss message" onClick={() => onDismiss(toast.id)}>
            Dismiss
          </button>
        </div>
      ))}
    </div>
  );
}
