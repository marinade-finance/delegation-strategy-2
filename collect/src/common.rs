use anyhow::Context;
use solana_client::rpc_client::RpcClient;
use solana_sdk::epoch_info::EpochInfo;
use std::{thread, time::Duration};
use structopt::StructOpt;

/// Below this the epoch is too young to measure against block time's one-second granularity.
const MIN_SLOTS_TO_MEASURE: u64 = 1000;

/// Skew between the container clock and the cluster's block time is a fixed offset, so only a long window dilutes it.
const MIN_SECONDS_TO_MEASURE: u64 = 600;

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

struct EpochClock {
    seconds_since_epoch_start: u64,
    milliseconds_per_slot: Option<u64>,
}

/// Anchored on a block the node still holds: `getBlocks` answers from the pruned ledger's start instead of failing.
fn epoch_clock(rpc_client: &RpcClient, epoch_info: &EpochInfo) -> anyhow::Result<EpochClock> {
    let first_slot = epoch_info
        .absolute_slot
        .saturating_sub(epoch_info.slot_index);
    let anchor = first_slot.max(rpc_client.get_first_available_block()?);
    // The anchor slot may be skipped, and `get_block_time` only answers for produced blocks.
    let anchor_block = *rpc_client
        .get_blocks_with_limit(anchor, 1)?
        .first()
        .context("No block produced yet since the epoch start")?;
    let elapsed = chrono::Utc::now().timestamp() - rpc_client.get_block_time(anchor_block)?;
    let elapsed_seconds =
        u64::try_from(elapsed).context("Anchor block is timestamped in the future")?;
    let measured_slots = epoch_info.absolute_slot.saturating_sub(anchor_block);
    let milliseconds_per_slot = milliseconds_per_slot(elapsed_seconds, measured_slots);

    let seconds_since_epoch_start = if anchor == first_slot {
        elapsed_seconds
    } else {
        // Slot time only ever changes on an epoch boundary, so a rate measured inside the epoch is its own.
        let rate = milliseconds_per_slot.context(
            "Ledger is pruned past the epoch start and the retained window is too short to measure",
        )?;
        seconds_from_rate(epoch_info.slot_index, rate)
    };

    Ok(EpochClock {
        seconds_since_epoch_start,
        milliseconds_per_slot,
    })
}

/// From block time, not slot counts: SIMD-0525 makes any slot-count arithmetic drift against the clock.
pub fn seconds_since_epoch_start(
    rpc_client: &RpcClient,
    epoch_info: &EpochInfo,
) -> anyhow::Result<u64> {
    Ok(epoch_clock(rpc_client, epoch_info)?.seconds_since_epoch_start)
}

/// `None` means unmeasurable, never "assume 400ms" — a stale nominal is what SIMD-0525 invalidates.
pub fn measure_milliseconds_per_slot(
    rpc_client: &RpcClient,
    epoch_info: &EpochInfo,
) -> anyhow::Result<Option<u64>> {
    // A cheap lower bound on the check below, only to skip three RPC round trips that cannot produce an answer.
    if epoch_info.slot_index < MIN_SLOTS_TO_MEASURE {
        return Ok(None);
    }
    Ok(epoch_clock(rpc_client, epoch_info)?.milliseconds_per_slot)
}

fn milliseconds_per_slot(elapsed_seconds: u64, measured_slots: u64) -> Option<u64> {
    if measured_slots < MIN_SLOTS_TO_MEASURE || elapsed_seconds < MIN_SECONDS_TO_MEASURE {
        return None;
    }
    Some(elapsed_seconds * 1000 / measured_slots)
}

fn seconds_from_rate(slot_index: u64, milliseconds_per_slot: u64) -> u64 {
    slot_index * milliseconds_per_slot / 1000
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
    fn too_short_a_window_is_not_measured() {
        assert_eq!(milliseconds_per_slot(0, 0), None);
        assert_eq!(milliseconds_per_slot(300, MIN_SLOTS_TO_MEASURE - 1), None);
        assert!(milliseconds_per_slot(MIN_SECONDS_TO_MEASURE, MIN_SLOTS_TO_MEASURE).is_some());
    }

    #[test]
    fn enough_slots_is_not_enough_when_they_span_too_little_wall_clock() {
        // 1000 slots is 400s at the baseline and 200s at 200ms - both too short for the clock skew to average out.
        assert_eq!(
            milliseconds_per_slot(MIN_SECONDS_TO_MEASURE - 1, MIN_SLOTS_TO_MEASURE),
            None
        );
        assert_eq!(milliseconds_per_slot(400, MIN_SLOTS_TO_MEASURE), None);
    }

    #[test]
    fn a_pruned_ledger_reconstructs_the_same_elapsed_time() {
        // Half the epoch retained: the rate measured over it must rebuild the full elapsed wall clock.
        let retained_slots = SLOTS_IN_EPOCH / 2;
        let rate = milliseconds_per_slot(86_400, retained_slots).unwrap();
        assert_eq!(rate, 400);
        assert_eq!(seconds_from_rate(SLOTS_IN_EPOCH, rate), 172_800);
    }

    #[test]
    fn the_pruned_branch_reports_equal_hours_at_any_slot_time() {
        let at_400ms = seconds_from_rate(108_000, milliseconds_per_slot(21_600, 54_000).unwrap());
        let at_350ms = seconds_from_rate(123_440, milliseconds_per_slot(21_602, 61_720).unwrap());
        assert_eq!(at_400ms / 3_600, 12);
        assert_eq!(at_350ms / 3_600, 12);
    }
}
