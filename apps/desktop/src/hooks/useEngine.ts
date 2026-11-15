import { useCallback, useRef, useState } from "react";

import { startEngine, stopEngine } from "../lib/commands";
import { CommandFailure } from "../lib/ipc";

/** User-facing text for any failure of a command. */
export function messageOf(err: unknown): string {
  if (err instanceof CommandFailure && err.error.message !== "") return err.error.message;
  return "Something went wrong. Try again.";
}

export interface EngineControl {
  starting: boolean;
  startError: string | undefined;
  /** Starts the engine; `onDone` runs after the attempt (success or not). */
  start: () => Promise<void>;
  stop: () => Promise<void>;
}

/** Start and stop the background engine; `onChange` runs after either finished. */
export function useEngine(onChange: () => void): EngineControl {
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string>();
  const busy = useRef(false);

  const start = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    setStarting(true);
    setStartError(undefined);
    try {
      await startEngine();
    } catch (err) {
      setStartError(messageOf(err));
    } finally {
      busy.current = false;
      setStarting(false);
      onChange();
    }
  }, [onChange]);

  const stop = useCallback(async () => {
    try {
      await stopEngine();
    } finally {
      onChange();
    }
  }, [onChange]);

  return { starting, startError, start, stop };
}
