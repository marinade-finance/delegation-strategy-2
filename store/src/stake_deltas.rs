use crate::dto::ValidatorRecord;
use crate::utils::last_reported_epoch;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::*;
use std::collections::HashMap;

/// Windows the stake deltas describe.
const DAYS_7: i64 = 7;
const DAYS_30: i64 = 30;

/// The epochs the 7- and 30-day deltas subtract against: the newest one that had ended that long
/// before the newest epoch end on record. `None` when history does not reach back.
pub(crate) fn delta_epochs<'a>(
    validators: impl IntoIterator<Item = &'a ValidatorRecord>,
) -> (Option<u64>, Option<u64>) {
    let mut ends: HashMap<u64, DateTime<Utc>> = Default::default();
    for stats in validators
        .into_iter()
        .flat_map(|validator| &validator.epoch_stats)
    {
        if let Some(epoch_end_at) = stats.epoch_end_at {
            ends.insert(stats.epoch, epoch_end_at);
        }
    }

    let Some(latest_end) = ends.values().max().copied() else {
        return (None, None);
    };
    let newest_ended_before = |days| {
        let cutoff = latest_end - Duration::days(days);
        ends.iter()
            .filter(|(_, epoch_end_at)| **epoch_end_at <= cutoff)
            .map(|(epoch, _)| *epoch)
            .max()
    };

    (newest_ended_before(DAYS_7), newest_ended_before(DAYS_30))
}

/// Stamps each record with 30 and 7 day stake deltas. A record with no row in an epoch counts as zero stake there.
pub(crate) fn stamp_stake_deltas<'a>(records: impl IntoIterator<Item = &'a mut ValidatorRecord>) {
    // Collected because the epochs are resolved over the same records that are then written.
    let mut records: Vec<&mut ValidatorRecord> = records.into_iter().collect();
    let (delta_7d_epoch, delta_30d_epoch) = delta_epochs(records.iter().map(|record| &**record));
    let Some(current_epoch) = last_reported_epoch(records.iter().map(|record| &**record)) else {
        return;
    };

    for record in &mut records {
        let stake_at = |epoch| {
            record
                .epoch_stats
                .iter()
                .find(|stats| stats.epoch == epoch)
                .map_or(Decimal::ZERO, |stats| stats.activated_stake)
        };
        let current = stake_at(current_epoch);
        let deltas = (
            delta_7d_epoch.map(|epoch| current - stake_at(epoch)),
            delta_30d_epoch.map(|epoch| current - stake_at(epoch)),
        );

        (record.stake_delta_7d, record.stake_delta_30d) = deltas;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ValidatorEpochStats;

    const EPOCH_SECONDS: i64 = 2 * 24 * 3600;
    const CURRENT_EPOCH: u64 = 100;

    /// `(epoch, stake)`. Epochs run ~2 days, so 96 ended ~8 days before 100 and 85 ~30.
    /// `stale_days` ages every epoch end.
    fn aged_validator(
        vote_account: &str,
        epochs: &[(u64, i64)],
        stale_days: i64,
    ) -> ValidatorRecord {
        // Fixed, and shared by every record a test builds: the windows land on exact epoch
        // boundaries, so two instants microseconds apart decide them differently.
        let latest_end = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        ValidatorRecord {
            vote_account: vote_account.to_string(),
            epoch_stats: epochs
                .iter()
                .map(|(epoch, stake)| ValidatorEpochStats {
                    epoch: *epoch,
                    epoch_end_at: Some(
                        latest_end
                            - Duration::days(stale_days)
                            - Duration::seconds((CURRENT_EPOCH - epoch) as i64 * EPOCH_SECONDS),
                    ),
                    activated_stake: Decimal::from(*stake),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn validator(vote_account: &str, epochs: &[(u64, i64)]) -> ValidatorRecord {
        aged_validator(vote_account, epochs, 0)
    }

    fn stamped(validators: Vec<ValidatorRecord>) -> HashMap<String, ValidatorRecord> {
        let mut validators: HashMap<_, _> = validators
            .into_iter()
            .map(|validator| (validator.vote_account.clone(), validator))
            .collect();
        stamp_stake_deltas(validators.values_mut());
        validators
    }

    /// Reaches back over both windows: 96 is the 7-day reference, 85 the 30-day one.
    fn spanning_both_windows(vote_account: &str) -> ValidatorRecord {
        validator(
            vote_account,
            &[(CURRENT_EPOCH, 300), (99, 300), (96, 200), (85, 100)],
        )
    }

    #[test]
    fn a_validator_carries_the_delta_of_its_own_stake() {
        let validators = stamped(vec![spanning_both_windows("grew")]);

        assert_eq!(validators["grew"].stake_delta_7d, Some(Decimal::from(100)));
        assert_eq!(validators["grew"].stake_delta_30d, Some(Decimal::from(200)));
    }

    #[test]
    fn a_validator_with_no_row_in_the_reference_epoch_reads_its_whole_stake_as_growth() {
        let validators = stamped(vec![
            validator("joined", &[(CURRENT_EPOCH, 500), (99, 500)]),
            spanning_both_windows("established"),
        ]);

        assert_eq!(
            validators["joined"].stake_delta_7d,
            Some(Decimal::from(500))
        );
        assert_eq!(
            validators["joined"].stake_delta_30d,
            Some(Decimal::from(500))
        );
    }

    #[test]
    fn no_reference_epoch_leaves_a_validator_without_a_delta() {
        // Two epochs ~2 days apart: neither window has anything to subtract against.
        let validators = stamped(vec![validator("young", &[(CURRENT_EPOCH, 300), (99, 200)])]);

        assert_eq!(validators["young"].stake_delta_7d, None);
        assert_eq!(validators["young"].stake_delta_30d, None);
    }

    #[test]
    fn records_that_stopped_arriving_keep_the_window_they_had() {
        let fresh = stamped(vec![spanning_both_windows("grew")]);
        let stale = stamped(vec![aged_validator(
            "grew",
            &[(CURRENT_EPOCH, 300), (99, 300), (96, 200), (85, 100)],
            20,
        )]);

        assert_eq!(stale["grew"].stake_delta_7d, fresh["grew"].stake_delta_7d);
        assert_eq!(stale["grew"].stake_delta_30d, fresh["grew"].stake_delta_30d);
    }

    #[test]
    fn a_validator_the_list_does_not_serve_still_gets_a_delta() {
        // Unstaked and not voting, so it is not eligible; its loss still shows on its group's row.
        let validators = stamped(vec![
            validator("left", &[(CURRENT_EPOCH, 0), (99, 0), (96, 700), (85, 700)]),
            spanning_both_windows("stayed"),
        ]);

        assert_eq!(validators["left"].stake_delta_7d, Some(Decimal::from(-700)));
    }
}
