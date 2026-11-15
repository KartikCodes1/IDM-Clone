import type { DownloadSummary } from "../lib/ipc";
import { DownloadRow, type RowRequest } from "./DownloadRow";

interface Props {
  items: DownloadSummary[];
  onRequest: (item: DownloadSummary, request: RowRequest) => void;
}

export function DownloadList({ items, onRequest }: Props) {
  return (
    <ul className="list" aria-label="Downloads">
      {items.map((item) => (
        <DownloadRow key={item.id} item={item} onRequest={onRequest} />
      ))}
    </ul>
  );
}
