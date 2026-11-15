import { useCallback } from "react";

import type { RowRequest } from "../components/DownloadRow";
import { retryRestartsFromZero } from "../lib/actions";
import {
  cancelDownload,
  pauseDownload,
  removeDownload,
  resumeDownload,
  retryDownload,
  showInFolder,
} from "../lib/commands";
import type { DownloadSummary } from "../lib/ipc";
import { messageOf } from "./useEngine";

function dispatch(item: DownloadSummary, request: RowRequest): Promise<void> {
  switch (request.action) {
    case "pause":
      return pauseDownload(item.id);
    case "resume":
      return resumeDownload(item.id);
    case "cancel":
      return cancelDownload(item.id);
    case "retry":
      return retryDownload(item.id, retryRestartsFromZero(item.last_error));
    case "remove":
      return removeDownload(item.id, request.deletePartial === true);
    case "show":
      return showInFolder(item.id);
  }
}

/** Runs a row command, reports a failure as an error toast, and refreshes the list. */
export function useRowRequests(
  notifyError: (message: string) => void,
  refresh: () => void,
): (item: DownloadSummary, request: RowRequest) => void {
  return useCallback(
    (item, request) => {
      void dispatch(item, request)
        .catch((err: unknown) => notifyError(messageOf(err)))
        .finally(refresh);
    },
    [notifyError, refresh],
  );
}
