import { useEffect, useRef, useState } from "react";

interface Props {
  onStop: () => void;
}

/** A small control in the title bar; stopping asks for confirmation first. */
export function StopEngine({ onStop }: Props) {
  const [confirming, setConfirming] = useState(false);
  const group = useRef<HTMLSpanElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const wasConfirming = useRef(false);

  // Keep keyboard focus: into the confirmation when it opens, back to the trigger when it closes.
  useEffect(() => {
    if (confirming) group.current?.querySelector("button")?.focus();
    else if (wasConfirming.current) trigger.current?.focus();
    wasConfirming.current = confirming;
  }, [confirming]);

  if (!confirming) {
    return (
      <button ref={trigger} type="button" className="button button-quiet button-small" onClick={() => setConfirming(true)}>
        Stop engine
      </button>
    );
  }
  return (
    <span ref={group} className="confirm-inline" role="group" aria-label="Confirm stopping the engine">
      <span className="muted">Downloads pause until the engine starts again.</span>
      <button
        type="button"
        className="button button-danger button-small"
        onClick={() => {
          setConfirming(false);
          onStop();
        }}
      >
        Stop now
      </button>
      <button type="button" className="button button-quiet button-small" onClick={() => setConfirming(false)}>
        Keep running
      </button>
    </span>
  );
}
