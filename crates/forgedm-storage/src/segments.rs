//! Segment plans and checkpoints. A plan or checkpoint replaces the whole segment set of a
//! download in one `BEGIN IMMEDIATE` transaction.

use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::convert::{to_i64, to_u32, to_u64};
use crate::db::Storage;
use crate::error::StorageError;
use crate::events::prune_events;
use crate::models::SegmentRecord;
use crate::sealer::id_text;

type SegmentRow = (i64, i64, Option<i64>, i64, i64, i64);

/// Segment repository, from [`Storage::segments`].
#[derive(Debug, Clone)]
pub struct Segments {
    storage: Storage,
}

/// `start <= committed` and, when bounded, `committed <= end` (so also `start <= end`).
fn validate_bounds(seg: &SegmentRecord) -> Result<(), StorageError> {
    let within_end = seg.end.is_none_or(|end| seg.committed <= end);
    if seg.committed >= seg.start && within_end {
        Ok(())
    } else {
        Err(StorageError::SegmentBounds { seg_no: seg.seg_no })
    }
}

/// Bytes committed across segments: the sum of `committed - start`.
fn committed_sum(segments: &[SegmentRecord]) -> Result<u64, StorageError> {
    segments.iter().try_fold(0_u64, |sum, seg| {
        sum.checked_add(seg.committed - seg.start)
            .ok_or(StorageError::InvalidValue {
                field: "bytes_committed",
            })
    })
}

/// Validate bounds and compute the committed sum for a fresh plan.
fn committed_sum_checked(segments: &[SegmentRecord]) -> Result<u64, StorageError> {
    segments.iter().try_for_each(validate_bounds)?;
    committed_sum(segments)
}

/// Segments, ordered by `start`, must be disjoint and cover `[0, total_size)`.
fn validate_tiling(
    segments: &[SegmentRecord],
    total_size: Option<u64>,
) -> Result<(), StorageError> {
    let mut next = 0_u64;
    for (index, seg) in segments.iter().enumerate() {
        if seg.start != next {
            return Err(StorageError::SegmentTiling {
                reason: "gap or overlap between segments",
            });
        }
        match seg.end {
            Some(end) => next = end,
            None if index + 1 == segments.len() => return Ok(()),
            None => {
                return Err(StorageError::SegmentTiling {
                    reason: "open-ended segment is not last",
                });
            }
        }
    }
    match total_size {
        Some(total) if !segments.is_empty() && next != total => Err(StorageError::SegmentTiling {
            reason: "segments do not cover the total size",
        }),
        _ => Ok(()),
    }
}

fn segment_from_row(row: SegmentRow) -> Result<SegmentRecord, StorageError> {
    let (seg_no, start, end, committed, retry_count, updated_at_ms) = row;
    Ok(SegmentRecord {
        seg_no: to_u32("seg_no", seg_no)?,
        start: to_u64("start_offset", start)?,
        end: end.map(|e| to_u64("end_offset", e)).transpose()?,
        committed: to_u64("committed_offset", committed)?,
        retry_count: to_u32("retry_count", retry_count)?,
        updated_at_ms,
    })
}

/// Delete and reinsert a download's segments inside the caller's transaction.
async fn rewrite_segments(
    conn: &mut SqliteConnection,
    download_id: &str,
    segments: &[SegmentRecord],
) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM segments WHERE download_id = ?1")
        .bind(download_id)
        .execute(&mut *conn)
        .await?;
    for seg in segments {
        sqlx::query(
            "INSERT INTO segments (download_id, seg_no, start_offset, end_offset, \
             committed_offset, retry_count, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(download_id)
        .bind(i64::from(seg.seg_no))
        .bind(to_i64("start_offset", seg.start)?)
        .bind(seg.end.map(|e| to_i64("end_offset", e)).transpose()?)
        .bind(to_i64("committed_offset", seg.committed)?)
        .bind(i64::from(seg.retry_count))
        .bind(seg.updated_at_ms)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

impl Segments {
    pub(crate) fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Replace the segment plan; `bytes_committed` becomes the sum of
    /// `committed - start`. Tiling is validated when loading, not here.
    ///
    /// # Errors
    /// [`StorageError::SegmentBounds`], [`StorageError::NotFound`] or a database error. On
    /// any error nothing changes.
    pub async fn replace_plan(
        &self,
        id: Uuid,
        segments: &[SegmentRecord],
    ) -> Result<(), StorageError> {
        let bytes = committed_sum_checked(segments)?;
        self.rewrite(id, bytes, segments, false).await
    }

    /// Persist a checkpoint: the full current segment set and the committed byte count, plus
    /// event pruning, in one transaction. Atomic: a failure on any row rolls everything back.
    ///
    /// `bytes_committed` must equal the sum of `committed - start` over `segments`, so
    /// progress and trusted data cannot disagree. Like [`Segments::replace_plan`], tiling is
    /// validated when loading, not here.
    ///
    /// # Errors
    /// [`StorageError::SegmentBounds`], [`StorageError::InvalidValue`] when `bytes_committed`
    /// disagrees with the segments, [`StorageError::NotFound`] or a database error.
    pub async fn save_checkpoint(
        &self,
        id: Uuid,
        bytes_committed: u64,
        segments: &[SegmentRecord],
    ) -> Result<(), StorageError> {
        if committed_sum_checked(segments)? != bytes_committed {
            return Err(StorageError::InvalidValue {
                field: "bytes_committed",
            });
        }
        self.rewrite(id, bytes_committed, segments, true).await
    }

    /// Delete all segments and zero `bytes_committed` (restart from zero).
    ///
    /// # Errors
    /// [`StorageError::NotFound`] or a database error.
    pub async fn reset(&self, id: Uuid, at_ms: i64) -> Result<(), StorageError> {
        let text = id_text(id);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET bytes_committed = 0, updated_at_ms = ?2 WHERE id = ?1",
        )
        .bind(&text)
        .bind(at_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        rewrite_segments(&mut tx, &text, &[]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Load and validate a download's segments, ordered by start offset: bounds per row and
    /// tiling against the download's `total_size`, read in one snapshot.
    ///
    /// # Errors
    /// [`StorageError::SegmentBounds`], [`StorageError::SegmentTiling`],
    /// [`StorageError::NotFound`] or a database error.
    pub async fn load(&self, id: Uuid) -> Result<Vec<SegmentRecord>, StorageError> {
        let text = id_text(id);
        let mut tx = self.storage.reader().begin().await?;
        let total: Option<Option<i64>> =
            sqlx::query_scalar("SELECT total_size FROM downloads WHERE id = ?1")
                .bind(&text)
                .fetch_optional(&mut *tx)
                .await?;
        let total = total.ok_or(StorageError::NotFound { id })?;
        let rows: Vec<SegmentRow> = sqlx::query_as(
            "SELECT seg_no, start_offset, end_offset, committed_offset, retry_count, updated_at_ms \
             FROM segments WHERE download_id = ?1 ORDER BY start_offset, seg_no",
        )
        .bind(&text)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let segments: Vec<SegmentRecord> = rows
            .into_iter()
            .map(segment_from_row)
            .collect::<Result<_, _>>()?;
        segments.iter().try_for_each(validate_bounds)?;
        validate_tiling(
            &segments,
            total.map(|t| to_u64("total_size", t)).transpose()?,
        )?;
        Ok(segments)
    }

    async fn rewrite(
        &self,
        id: Uuid,
        bytes_committed: u64,
        segments: &[SegmentRecord],
        prune: bool,
    ) -> Result<(), StorageError> {
        let text = id_text(id);
        let newest = segments.iter().map(|s| s.updated_at_ms).max().unwrap_or(0);
        let mut tx = self.storage.begin_write().await?;
        let changed = sqlx::query(
            "UPDATE downloads SET bytes_committed = ?2, \
             updated_at_ms = MAX(updated_at_ms, ?3) WHERE id = ?1",
        )
        .bind(&text)
        .bind(to_i64("bytes_committed", bytes_committed)?)
        .bind(newest)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::NotFound { id });
        }
        rewrite_segments(&mut tx, &text, segments).await?;
        if prune {
            prune_events(&mut tx, &text).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
