//! `forgedm list [--json]`: page through `ListDownloads`.

use forgedm_ipc::Client;
use forgedm_protocol::{DownloadSummary, ListDownloadsRequest, RequestBody, ResultBody};
use serde_json::json;

use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// Largest page the agent serves (PLAN A.3: `limit` 1..=500).
const PAGE_LIMIT: u32 = 500;

/// Fetches every download and prints it as a table or as `{"items": [...], "total": n}`.
pub async fn run(target: &Target, json: bool) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let (items, total) = collect_pages(&mut client).await?;
    if json {
        output::line(&serde_json::to_string(
            &json!({"items": items, "total": total}),
        )?);
    } else {
        output::download_table(&items);
    }
    Ok(())
}

/// Most rows the CLI accepts from one agent; more is treated as a misbehaving peer.
const MAX_LIST_ITEMS: usize = 100_000;
/// Most pages the CLI requests in one run.
const MAX_PAGES: usize = 1_000;

/// Something that serves one page of rows at a time.
trait PageSource {
    type Item;
    /// One page starting at `offset`, and the total the peer reports.
    async fn page(&mut self, offset: u32) -> Result<(Vec<Self::Item>, u32), CliError>;
}

impl PageSource for Client {
    type Item = DownloadSummary;

    async fn page(&mut self, offset: u32) -> Result<(Vec<DownloadSummary>, u32), CliError> {
        let request = ListDownloadsRequest {
            states: None,
            limit: PAGE_LIMIT,
            offset,
        };
        match self.call(RequestBody::ListDownloads(request)).await? {
            ResultBody::ListDownloads(page) => Ok((page.items, page.total)),
            _ => Err(CliError::Unexpected),
        }
    }
}

/// Pages `source` until `total` rows were received or a page comes back empty.
///
/// # Errors
/// [`CliError::Unexpected`] when a page exceeds [`PAGE_LIMIT`], or the page or row caps
/// are exceeded; otherwise whatever the source returns.
async fn collect_pages<S: PageSource>(source: &mut S) -> Result<(Vec<S::Item>, u32), CliError> {
    let mut items: Vec<S::Item> = Vec::new();
    for _ in 0..MAX_PAGES {
        let offset = u32::try_from(items.len()).map_err(|_| CliError::Unexpected)?;
        let (page, total) = source.page(offset).await?;
        if page.len() > PAGE_LIMIT as usize || items.len() + page.len() > MAX_LIST_ITEMS {
            return Err(CliError::Unexpected);
        }
        let page_empty = page.is_empty();
        items.extend(page);
        if page_empty || u32::try_from(items.len()).unwrap_or(u32::MAX) >= total {
            return Ok((items, total));
        }
    }
    Err(CliError::Unexpected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serves `pages` in order (repeating the last forever) with a fixed reported total.
    struct Fake {
        pages: Vec<Vec<u8>>,
        total: u32,
        calls: usize,
    }

    impl PageSource for Fake {
        type Item = u8;

        async fn page(&mut self, _offset: u32) -> Result<(Vec<u8>, u32), CliError> {
            let index = self.calls.min(self.pages.len() - 1);
            self.calls += 1;
            Ok((self.pages[index].clone(), self.total))
        }
    }

    #[tokio::test]
    async fn endless_nonempty_pages_are_cut_off() {
        let mut fake = Fake {
            pages: vec![vec![0]],
            total: u32::MAX,
            calls: 0,
        };

        let result = collect_pages(&mut fake).await;

        assert!(matches!(result, Err(CliError::Unexpected)));
        assert!(fake.calls <= MAX_PAGES);
    }

    #[tokio::test]
    async fn oversized_page_is_rejected() {
        let mut fake = Fake {
            pages: vec![vec![0; PAGE_LIMIT as usize + 1]],
            total: u32::MAX,
            calls: 0,
        };

        let result = collect_pages(&mut fake).await;

        assert!(matches!(result, Err(CliError::Unexpected)));
    }

    #[tokio::test]
    async fn stops_when_total_is_reached() {
        let mut fake = Fake {
            pages: vec![vec![7, 8], vec![]],
            total: 2,
            calls: 0,
        };

        let result = collect_pages(&mut fake).await;

        assert!(matches!(result, Ok((ref items, 2)) if items == &[7, 8]));
    }
}
