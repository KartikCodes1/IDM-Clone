import type { ReactNode } from "react";

interface EmptyStateProps {
  title: string;
  children: ReactNode;
  action?: { label: string; onClick: () => void };
  tone?: "neutral" | "warning";
}

export function EmptyState({ title, children, action, tone = "neutral" }: EmptyStateProps) {
  return (
    <section className={`empty empty-${tone}`} aria-live="polite">
      <h2>{title}</h2>
      <p className="muted">{children}</p>
      {action && (
        <button type="button" className="button" onClick={action.onClick}>
          {action.label}
        </button>
      )}
    </section>
  );
}
