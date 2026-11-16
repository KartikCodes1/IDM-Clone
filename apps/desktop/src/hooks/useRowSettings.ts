import { useCallback } from "react";

import { updateDownloadConnections, updatePriority } from "../lib/commands";
import type { DownloadSummary } from "../lib/ipc";
import { messageOf } from "./useEngine";

/** A per-download setting chosen in a row. */
export type RowSetting = { kind: "priority"; value: string } | { kind: "connections"; value: number };

/** Applies a priority or connection-count change; reports the outcome as a toast. */
export function useRowSettings(
  notify: (tone: "info" | "error", message: string) => void,
  refresh: () => void,
): (item: DownloadSummary, setting: RowSetting) => void {
  return useCallback(
    (item, setting) => {
      const task =
        setting.kind === "priority"
          ? updatePriority(item.id, setting.value)
          : updateDownloadConnections(item.id, setting.value).then((limits) => {
              const count = limits.max_connections ?? setting.value;
              notify("info", `${item.display_name}: up to ${count} connection${count === 1 ? "" : "s"}`);
            });
      void task.catch((err: unknown) => notify("error", messageOf(err))).finally(refresh);
    },
    [notify, refresh],
  );
}
