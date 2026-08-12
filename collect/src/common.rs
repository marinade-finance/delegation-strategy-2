use anyhow::Context;
use solana_client::rpc_client::RpcClient;
use solana_sdk::epoch_info::EpochInfo;
use std::{thread, time::Duration};
use structopt::StructOpt;

/// Below this the epoch is too young to measure against block time's one-second granularity.
const MIN_SLOTS_TO_MEASURE: u64 = 1000;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(short = "u", long = "url", env = "RPC_URL")]
    pub rpc_url: String,

    #[structopt(short = "c", long = "commitment", default_value = "finalized")]
    pub commitment: String,
}

pub fn retry_blocking<F, T, E, ErrorCallback>(
    make_call: F,
    backoff_strategy: impl Iterator<Item = Duration>,
    on_error: ErrorCallback,
) -> Result<T, E>
where
    F: Fn() -> Result<T, E>,
    E: std::fmt::Debug,
    ErrorCallback: Fn(E, usize, Duration),
{
    for (attempt_index, backoff) in backoff_strategy.enumerate() {
        match make_call() {
            Ok(result) => return Ok(result),
            Err(err) => {
                on_error(err, attempt_index + 1, backoff);
                thread::sleep(backoff);
            }
        }
    }
    make_call()
}

/// From block time, not slot counts: SIMD-0525 makes any slot-count arithmetic drift against the clock.
pub fn seconds_since_epoch_start(
    rpc_client: &RpcClient,
    epoch_info: &EpochInfo,
) -> anyhow::Result<u64> {
    let first_slot = epoch_info
        .absolute_slot
        .saturating_sub(epoch_info.slot_index);
    // The epoch's first slot may be skipped, and `get_block_time` only answers for produced blocks.
    let first_block = *rpc_client
        .get_blocks_with_limit(first_slot, 1)?
        .first()
        .context("No block produced yet in the current epoch")?;
    let elapsed = chrono::Utc::now().timestamp() - rpc_client.get_block_time(first_block)?;
    u64::try_from(elapsed).context("Epoch's first block is timestamped in the future")
}

/// `None` means unmeasurable, never "assume 400ms" — a stale nominal is what SIMD-0525 invalidates.
pub fn measure_milliseconds_per_slot(
    rpc_client: &RpcClient,
    epoch_info: &EpochInfo,
) -> anyhow::Result<Option<u64>> {
    // Repeats the check below only to skip two RPC round trips that could not produce an answer.
    if epoch_info.slot_index < MIN_SLOTS_TO_MEASURE {
        return Ok(None);
    }
    let elapsed_seconds = seconds_since_epoch_start(rpc_client, epoch_info)?;
    Ok(milliseconds_per_slot(
        elapsed_seconds,
        epoch_info.slot_index,
    ))
}

fn milliseconds_per_slot(elapsed_seconds: u64, slot_index: u64) -> Option<u64> {
    if slot_index < MIN_SLOTS_TO_MEASURE {
        return None;
    }
    Some(elapsed_seconds * 1000 / slot_index)
}

pub struct QuadraticBackoffStrategy;

impl QuadraticBackoffStrategy {
    pub fn iter_durations(max_attempts: usize) -> impl Iterator<Item = Duration> {
        (1..=max_attempts).map(|attempt| Duration::from_secs((attempt as u64).pow(2)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOTS_IN_EPOCH: u64 = 432_000;

    #[test]
    fn slot_duration_follows_the_clock_not_the_slot_count() {
        // SIMD-0525 keeps the slot count and shortens the epoch, so only wall clock separates these.
        assert_eq!(milliseconds_per_slot(172_800, SLOTS_IN_EPOCH), Some(400));
        assert_eq!(milliseconds_per_slot(151_200, SLOTS_IN_EPOCH), Some(350));
        assert_eq!(milliseconds_per_slot(86_400, SLOTS_IN_EPOCH), Some(200));
    }

    #[test]
    fn equal_wall_clock_reports_equal_hours_at_any_slot_time() {
        // Twelve real hours in, at 400ms and 350ms; the old slot-count gate read 12h and 13.7h here.
        let at_400ms = milliseconds_per_slot(43_200, 108_000).unwrap();
        let at_350ms = milliseconds_per_slot(43_200, 123_428).unwrap();
        assert_eq!(at_400ms, 400);
        assert_eq!(at_350ms, 350);
    }

    #[test]
    fn too_young_an_epoch_is_not_measured() {
        assert_eq!(milliseconds_per_slot(0, 0), None);
        assert_eq!(milliseconds_per_slot(300, MIN_SLOTS_TO_MEASURE - 1), None);
        assert!(milliseconds_per_slot(400, MIN_SLOTS_TO_MEASURE).is_some());
    }
}
