use crate::leader_schedule::{
    bigquery_dataset, bigquery_wait_is_over, create_bigquery_client, GOOGLE_BQ_PROJECT_ID,
};
use anyhow::{bail, Context};
use google_cloud_bigquery::client::Client as BqClient;
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{info, warn};
use solana_clock::Epoch;
use solana_epoch_info::EpochInfo;
use solana_rpc_client::rpc_client::RpcClient;
use std::collections::HashSet;

/// Vote accounts stakes-etl saw charged the SIMD-0357 VAT for the epoch; empty while not loaded.
async fn query_vat_debited(
    bq_client: &BqClient,
    dataset: &str,
    epoch: Epoch,
) -> anyhow::Result<Vec<String>> {
    let query = format!(
        "SELECT vote_account \
         FROM `{GOOGLE_BQ_PROJECT_ID}.{dataset}.rewards_validators_inflation_vat` \
         WHERE epoch = {epoch}"
    );
    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };
    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await
        .context("Failed to query the VAT debits")?;

    let mut debited = vec![];
    let mut seen = HashSet::new();
    while let Some(row) = iter.next().await? {
        let vote_account = row
            .column::<String>(0)
            .context("Failed to read vote_account")?;
        if !seen.insert(vote_account.clone()) {
            bail!("VAT debits of epoch {epoch} hold vote account {vote_account} more than once");
        }
        debited.push(vote_account);
    }
    Ok(debited)
}

/// Fails while the wait for the VAT debits of the closed epoch runs; Ok once it is over.
fn missing_debits_verdict(
    closed_epoch: Epoch,
    epoch_info: &EpochInfo,
    finalize_epoch_rpc_fallback: Option<u64>,
) -> anyhow::Result<()> {
    let Some(wait_slots) = finalize_epoch_rpc_fallback else {
        bail!("VAT debits of closed epoch {closed_epoch} are not in BigQuery yet")
    };
    if !bigquery_wait_is_over(closed_epoch, epoch_info, wait_slots) {
        bail!(
            "VAT debits of closed epoch {closed_epoch} are not in BigQuery yet (epoch {} at slot {}/{}), the epoch closes without them at slot {wait_slots} of the next epoch",
            epoch_info.epoch,
            epoch_info.slot_index,
            epoch_info.slots_in_epoch
        )
    }
    Ok(())
}

/// The VAT debits of a closed epoch. `None` closes the epoch with an unknown verdict: no
/// BigQuery dataset for the cluster, or the `finalize_epoch_rpc_fallback` wait is over.
pub fn vat_debited_of_closed_epoch(
    rpc_client: &RpcClient,
    epoch: Epoch,
    finalize_epoch_rpc_fallback: Option<u64>,
) -> anyhow::Result<Option<Vec<String>>> {
    let Some(dataset) = bigquery_dataset(rpc_client)? else {
        warn!("BigQuery has no VAT debits for this cluster, epoch {epoch} closes without them");
        return Ok(None);
    };
    info!("Reading the VAT debits of epoch {epoch} from BigQuery dataset {dataset}");
    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let debited = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_vat_debited(&bq_client, dataset, epoch).await
    })?;
    if !debited.is_empty() {
        info!("Epoch {epoch} has {} VAT debits", debited.len());
        return Ok(Some(debited));
    }
    missing_debits_verdict(
        epoch,
        &rpc_client.get_epoch_info()?,
        finalize_epoch_rpc_fallback,
    )?;
    warn!("VAT debits of closed epoch {epoch} are not in BigQuery after the wait, the epoch closes without them");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch_info(epoch: Epoch, slot_index: u64) -> EpochInfo {
        EpochInfo {
            epoch,
            slot_index,
            slots_in_epoch: 432_000,
            absolute_slot: 0,
            block_height: 0,
            transaction_count: None,
        }
    }

    #[test]
    fn missing_debits_fail_finalize_without_a_fallback() {
        let err = missing_debits_verdict(10, &epoch_info(12, 0), None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "VAT debits of closed epoch 10 are not in BigQuery yet"
        );
    }

    #[test]
    fn missing_debits_fail_finalize_while_the_wait_runs() {
        let err = missing_debits_verdict(10, &epoch_info(11, 89_999), Some(90_000)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "VAT debits of closed epoch 10 are not in BigQuery yet (epoch 11 at slot 89999/432000), the epoch closes without them at slot 90000 of the next epoch"
        );
    }

    #[test]
    fn missing_debits_close_the_epoch_unknown_after_the_wait() {
        assert!(missing_debits_verdict(10, &epoch_info(11, 90_000), Some(90_000)).is_ok());
    }
}
