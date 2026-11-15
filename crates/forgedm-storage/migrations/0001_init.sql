-- ForgeDM schema v1 (ARCHITECTURE section 7.2). LF line endings only: sqlx checksums the
-- file bytes, so a CRLF checkout would be a migration checksum mismatch.
-- Enum-valued columns are validated in Rust, not by CHECK, because SQLite cannot alter a
-- CHECK without rebuilding the table.

CREATE TABLE downloads (
  id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36),
  state TEXT NOT NULL, state_reason TEXT,
  source TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 1 CHECK (priority BETWEEN 0 AND 2),
  queue_position INTEGER NOT NULL,
  original_url_sealed BLOB NOT NULL, final_url_sealed BLOB,
  url_display TEXT NOT NULL,
  target_dir TEXT NOT NULL, requested_name TEXT, display_name TEXT NOT NULL, file_name TEXT,
  part_path TEXT, part_file_id BLOB, finalize_target TEXT, final_path TEXT,
  collision_policy TEXT NOT NULL DEFAULT 'rename', transfer_mode TEXT,
  total_size INTEGER CHECK (total_size IS NULL OR total_size >= 0),
  mime TEXT,
  etag TEXT, etag_strong INTEGER CHECK (etag_strong IN (0,1)),
  last_modified TEXT, last_modified_strong INTEGER CHECK (last_modified_strong IN (0,1)),
  checksum_expected TEXT CHECK (checksum_expected IS NULL OR (length(checksum_expected) = 64 AND checksum_expected NOT GLOB '*[^0-9a-f]*')),
  checksum_actual TEXT CHECK (checksum_actual IS NULL OR length(checksum_actual) = 64),
  length_verified INTEGER CHECK (length_verified IN (0,1)),
  max_connections INTEGER NOT NULL DEFAULT 8 CHECK (max_connections BETWEEN 1 AND 16),
  rate_limit_bps INTEGER CHECK (rate_limit_bps IS NULL OR rate_limit_bps >= 1024),
  bytes_committed INTEGER NOT NULL DEFAULT 0 CHECK (bytes_committed >= 0),
  retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0), next_retry_at_ms INTEGER,
  motw_status TEXT,
  last_error_code TEXT, last_error_message TEXT, last_error_retryable INTEGER CHECK (last_error_retryable IN (0,1)),
  created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, completed_at_ms INTEGER
) STRICT;
CREATE INDEX downloads_sched ON downloads(state, priority DESC, queue_position);
CREATE INDEX downloads_created ON downloads(created_at_ms DESC, id);

CREATE TABLE segments (
  download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  seg_no INTEGER NOT NULL CHECK (seg_no >= 0),
  start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
  end_offset INTEGER,
  committed_offset INTEGER NOT NULL,
  retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (download_id, seg_no),
  CHECK (end_offset IS NULL OR end_offset >= start_offset),
  CHECK (committed_offset >= start_offset),
  CHECK (end_offset IS NULL OR committed_offset <= end_offset)
) STRICT, WITHOUT ROWID;

CREATE TABLE download_events (
  id INTEGER PRIMARY KEY,
  download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  at_ms INTEGER NOT NULL, kind TEXT NOT NULL, from_state TEXT, to_state TEXT, code TEXT,
  detail_json TEXT CHECK (detail_json IS NULL OR (json_valid(detail_json) AND length(detail_json) <= 4096))
) STRICT;
CREATE INDEX download_events_by_download ON download_events(download_id, id);

CREATE TABLE settings (
  key TEXT PRIMARY KEY NOT NULL,
  value_json TEXT NOT NULL CHECK (json_valid(value_json) AND length(value_json) <= 16384),
  updated_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
