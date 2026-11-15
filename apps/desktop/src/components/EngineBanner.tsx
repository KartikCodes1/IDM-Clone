interface Props {
  starting: boolean;
  error: string | undefined;
  onStart: () => void;
  onCheck: () => void;
}

/** Shown while the background engine cannot be reached. */
export function EngineBanner({ starting, error, onStart, onCheck }: Props) {
  return (
    <section className="empty empty-warning" aria-live="polite">
      <h2>Background engine not running</h2>
      <p className="muted">
        {starting
          ? "Starting the background engine..."
          : "ForgeDM downloads run in a separate background engine. Start it to add and manage downloads."}
      </p>
      {error !== undefined && !starting && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
      <div className="button-row">
        <button type="button" className="button" disabled={starting} onClick={onStart}>
          Start engine
        </button>
        <button type="button" className="button button-quiet" onClick={onCheck}>
          Check again
        </button>
      </div>
    </section>
  );
}
