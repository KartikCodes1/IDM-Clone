import type { RowSetting } from "../hooks/useRowSettings";
import type { DownloadSummary } from "../lib/ipc";
import { DownloadRow, type RowRequest } from "./DownloadRow";

interface Props {
  items: DownloadSummary[];
  onRequest: (item: DownloadSummary, request: RowRequest) => void;
  onSetting: (item: DownloadSummary, setting: RowSetting) => void;
}

export function DownloadList({ items, onRequest, onSetting }: Props) {
  return (
    <ul className="list" aria-label="Downloads">
      {items.map((item) => (
        <DownloadRow key={item.id} item={item} onRequest={onRequest} onSetting={onSetting} />
      ))}
    </ul>
  );
}
