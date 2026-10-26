use collect::solana_service::MAX_TOWER_CREDITS_IN_EPOCH;
use log::warn;
use rust_decimal::prelude::*;
use std::collections::HashMap;
use tokio_postgres::Client;

struct VotePerformanceInput {
    vote_account: String,
    activated_stake: Decimal,
    /// `activated_stake` of the previous epoch. Agave pays the Alpenglow vote reward on this stake.
    epoch_stake: Option<Decimal>,
    leader_slots: u64,
    credits: Option<u64>,
    vote_reward_lamports: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum EpochKind {
    Tower,
    // The only epoch with both tower credits and vote rewards
    Migration,
    Alpenglow,
}

/// Cluster sums of one epoch. The Alpenglow sums cover the rows with a vote reward and an `epoch_stake`.
#[derive(Debug, Default, PartialEq)]
struct EpochTotals {
    has_credits: bool,
    has_rewards: bool,
    tower_stake: Decimal,
    tower_weighted_credits: Decimal,
    alpenglow_stake: Decimal,
    alpenglow_leader_slots: Decimal,
    alpenglow_reward: Decimal,
}

impl VotePerformanceInput {
    // Testnet epochs 1042-1053 hold vote reward lamports in `credits`, about 3e11
    fn tower_credits(&self) -> Option<u64> {
        self.credits
            .filter(|credits| *credits <= MAX_TOWER_CREDITS_IN_EPOCH)
    }
}

fn alpenglow_input(row: &VotePerformanceInput) -> Option<(Decimal, Decimal, Decimal)> {
    Some((
        row.epoch_stake?,
        Decimal::from(row.leader_slots),
        Decimal::from(row.vote_reward_lamports?),
    ))
}

impl EpochTotals {
    fn from_rows(rows: &[VotePerformanceInput]) -> Self {
        let mut totals = Self::default();
        for row in rows {
            totals.has_credits |= row.tower_credits().is_some();
            totals.has_rewards |= row.vote_reward_lamports.is_some();
            if let Some(credits) = row.tower_credits() {
                totals.tower_stake += row.activated_stake;
                totals.tower_weighted_credits += row.activated_stake * Decimal::from(credits);
            }
            if let Some((stake, slots, reward)) = alpenglow_input(row) {
                totals.alpenglow_stake += stake;
                totals.alpenglow_leader_slots += slots;
                totals.alpenglow_reward += reward;
            }
        }
        totals
    }

    fn kind(&self) -> Option<EpochKind> {
        match (self.has_credits, self.has_rewards) {
            (true, true) => Some(EpochKind::Migration),
            (true, false) => Some(EpochKind::Tower),
            (false, true) => Some(EpochKind::Alpenglow),
            (false, false) => None,
        }
    }

    /// Votes of `row` relative to the cluster, 1 = on par.
    fn performance(&self, row: &VotePerformanceInput) -> Option<f64> {
        match self.kind()? {
            EpochKind::Tower => self.tower_performance(row),
            EpochKind::Alpenglow => self.alpenglow_performance(row),
            EpochKind::Migration => None,
        }
    }

    // credits / stake-weighted mean credits
    fn tower_performance(&self, row: &VotePerformanceInput) -> Option<f64> {
        let credits = Decimal::from(row.tower_credits()?);
        if self.tower_stake.is_zero() || self.tower_weighted_credits.is_zero() {
            return None;
        }
        (credits / (self.tower_weighted_credits / self.tower_stake)).to_f64()
    }

    // (reward/w) / (Σreward/Σw) with w = epoch_stake/S + leader_slots/N
    fn alpenglow_performance(&self, row: &VotePerformanceInput) -> Option<f64> {
        let (stake, slots, reward) = alpenglow_input(row)?;
        let share = |part: Decimal, total: Decimal| {
            if total.is_zero() {
                Decimal::ZERO
            } else {
                part / total
            }
        };
        let weight = share(stake, self.alpenglow_stake) + share(slots, self.alpenglow_leader_slots);
        // Each share sums to 1 over the rows when its total is above 0
        let total_weight = Decimal::from(
            u8::from(!self.alpenglow_stake.is_zero())
                + u8::from(!self.alpenglow_leader_slots.is_zero()),
        );
        if weight.is_zero() || total_weight.is_zero() || self.alpenglow_reward.is_zero() {
            return None;
        }
        (reward / weight / (self.alpenglow_reward / total_weight)).to_f64()
    }
}

/// Votes of one epoch relative to the cluster, 1 = on par. Vote accounts without a value are left out.
fn vote_performance(rows: &[VotePerformanceInput]) -> HashMap<String, f64> {
    let totals = EpochTotals::from_rows(rows);
    rows.iter()
        .filter_map(|row| {
            totals
                .performance(row)
                .map(|ratio| (row.vote_account.clone(), ratio))
        })
        .collect()
}

/// `vote_performance` keyed by (epoch, vote account) for the epochs `from_epoch..=to_epoch`.
pub async fn load_vote_performance(
    psql_client: &Client,
    from_epoch: u64,
    to_epoch: u64,
) -> anyhow::Result<HashMap<(u64, String), f64>> {
    let rows = psql_client
        .query(
            // epoch_stake is the activated_stake of the previous epoch
            "SELECT validators.epoch, validators.vote_account, validators.activated_stake,
                NULLIF(previous.activated_stake, 0) AS epoch_stake,
                validators.leader_slots, validators.credits, validators.vote_reward_lamports
            FROM validators
                LEFT JOIN validators previous ON previous.vote_account = validators.vote_account
                    AND previous.epoch = validators.epoch - 1
            WHERE validators.epoch BETWEEN $1 AND $2",
            &[&Decimal::from(from_epoch), &Decimal::from(to_epoch)],
        )
        .await?;

    let mut by_epoch: HashMap<u64, Vec<VotePerformanceInput>> = Default::default();
    for row in rows {
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        by_epoch
            .entry(epoch)
            .or_default()
            .push(VotePerformanceInput {
                vote_account: row.get("vote_account"),
                activated_stake: row.get("activated_stake"),
                epoch_stake: row.get("epoch_stake"),
                leader_slots: row.get::<_, Decimal>("leader_slots").try_into()?,
                credits: row
                    .get::<_, Option<Decimal>>("credits")
                    .map(u64::try_from)
                    .transpose()?,
                vote_reward_lamports: row
                    .get::<_, Option<Decimal>>("vote_reward_lamports")
                    .map(u64::try_from)
                    .transpose()?,
            });
    }

    for (epoch, rows) in &by_epoch {
        let left_out = rows
            .iter()
            .filter(|row| row.credits.is_some() && row.tower_credits().is_none())
            .count();
        if left_out > 0 {
            warn!(
                "Epoch {epoch}: {left_out} rows hold credits above {MAX_TOWER_CREDITS_IN_EPOCH}."
            );
        }
    }

    Ok(by_epoch
        .into_iter()
        .flat_map(|(epoch, rows)| {
            vote_performance(&rows)
                .into_iter()
                .map(move |(vote_account, ratio)| ((epoch, vote_account), ratio))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tower(vote_account: &str, stake: u64, credits: Option<u64>) -> VotePerformanceInput {
        VotePerformanceInput {
            vote_account: vote_account.into(),
            activated_stake: stake.into(),
            epoch_stake: Some(stake.into()),
            leader_slots: 0,
            credits,
            vote_reward_lamports: None,
        }
    }

    fn alpenglow(
        vote_account: &str,
        stake: u64,
        leader_slots: u64,
        reward: Option<u64>,
    ) -> VotePerformanceInput {
        VotePerformanceInput {
            vote_account: vote_account.into(),
            activated_stake: stake.into(),
            epoch_stake: Some(stake.into()),
            leader_slots,
            credits: None,
            vote_reward_lamports: reward,
        }
    }

    fn assert_performance(rows: &[VotePerformanceInput], expected: &[(&str, Option<f64>)]) {
        assert_case("", rows, expected)
    }

    fn assert_case(case: &str, rows: &[VotePerformanceInput], expected: &[(&str, Option<f64>)]) {
        let performance = vote_performance(rows);
        for (vote_account, ratio) in expected {
            match (performance.get(*vote_account), ratio) {
                (Some(actual), Some(ratio)) => assert!(
                    (actual - ratio).abs() < 1e-12,
                    "{case} {vote_account}: {actual} != {ratio}"
                ),
                (None, None) => {}
                (actual, ratio) => panic!("{case} {vote_account}: {actual:?} != {ratio:?}"),
            }
        }
        assert_eq!(
            performance.len(),
            expected.iter().filter(|(_, ratio)| ratio.is_some()).count(),
            "{case}"
        );
    }

    // (vote account, stake, leader slots, reward)
    type VectorValidator = (&'static str, u64, u64, u64);
    // (case name, validators, expected vote_performance)
    type VectorCase = (
        &'static str,
        Vec<VectorValidator>,
        Vec<(&'static str, Option<f64>)>,
    );

    // typescript-common alpenglow-vectors.json, the cases where S and N are the sums over the set
    #[test]
    fn alpenglow_matches_the_shared_vectors() {
        let cases: Vec<VectorCase> = vec![
            (
                "validator-bonds alpenglow_collection: C expects 600",
                vec![("A", 100, 3, 1000), ("B", 100, 2, 1000), ("C", 100, 1, 400)],
                vec![
                    ("A", Some(1.0)),
                    ("B", Some(1000.0 / 800.0)),
                    ("C", Some(400.0 / 600.0)),
                ],
            ),
            (
                "unequal stakes with a floor remainder",
                vec![("X", 200, 1, 700), ("Y", 100, 1, 300), ("Z", 50, 1, 1)],
                vec![
                    ("X", Some(1_470_000.0 / 950_950.0)),
                    ("Y", Some(630_000.0 / 650_650.0)),
                    ("Z", Some(2_100.0 / 500_500.0)),
                ],
            ),
            (
                "no leader slots weighs by stake only",
                vec![("P", 100, 0, 100), ("Q", 300, 0, 100)],
                vec![("P", Some(2.0)), ("Q", Some(40_000.0 / 60_000.0))],
            ),
            (
                "no credits in the set",
                vec![("A", 100, 1, 0), ("B", 100, 1, 0)],
                vec![("A", None), ("B", None)],
            ),
            (
                "no stake weighs by leader slots only",
                vec![("A", 0, 3, 1000), ("B", 0, 3, 0)],
                vec![("A", Some(2.0)), ("B", Some(0.0))],
            ),
            (
                "zero-weight member",
                vec![("A", 100, 1, 100), ("Z", 0, 0, 0)],
                vec![("A", Some(1.0)), ("Z", None)],
            ),
            (
                "credits past 2^53 stay exact",
                vec![
                    ("A", 400_000_000_000_000_000, 10_000, 9_007_199_254_740_993),
                    ("B", 400_000_000_000_000_000, 10_000, 1),
                ],
                vec![
                    ("A", Some(9_007_199_254_740_993.0 / 4_503_599_627_370_497.0)),
                    ("B", Some(1.0 / 4_503_599_627_370_497.0)),
                ],
            ),
            ("both totals zero", vec![("A", 0, 0, 50)], vec![("A", None)]),
            ("empty set", vec![], vec![]),
        ];
        for (case, validators, expected) in cases {
            let rows: Vec<_> = validators
                .into_iter()
                .map(|(vote_account, stake, slots, reward)| {
                    alpenglow(vote_account, stake, slots, Some(reward))
                })
                .collect();
            assert_case(case, &rows, &expected);
        }
    }

    #[test]
    fn a_small_and_a_large_validator_are_on_par() {
        assert_performance(
            &[
                alpenglow("large", 900, 90, Some(900_000)),
                alpenglow("small", 100, 10, Some(100_000)),
            ],
            &[("large", Some(1.0)), ("small", Some(1.0))],
        );
    }

    #[test]
    fn skipped_leader_slots_count_as_downtime() {
        // 'skipper' earns the voter half only: (250 / 0.5) / (1750 / 2)
        assert_performance(
            &[
                alpenglow("a", 100, 10, Some(500)),
                alpenglow("b", 100, 10, Some(500)),
                alpenglow("c", 100, 10, Some(500)),
                alpenglow("skipper", 100, 10, Some(250)),
            ],
            &[
                ("a", Some(8.0 / 7.0)),
                ("b", Some(8.0 / 7.0)),
                ("c", Some(8.0 / 7.0)),
                ("skipper", Some(4.0 / 7.0)),
            ],
        );
    }

    #[test]
    fn a_null_reward_is_left_out_of_the_sums() {
        assert_performance(
            &[
                alpenglow("a", 100, 10, Some(1000)),
                alpenglow("b", 100, 10, Some(1000)),
                alpenglow("unknown", 100, 10, None),
            ],
            &[("a", Some(1.0)), ("b", Some(1.0)), ("unknown", None)],
        );
    }

    #[test]
    fn alpenglow_weighs_by_the_epoch_stake() {
        // 'grown' doubles its stake in the epoch, agave pays it on the 100 of the previous epoch
        assert_performance(
            &[
                VotePerformanceInput {
                    activated_stake: 200.into(),
                    ..alpenglow("grown", 100, 10, Some(1000))
                },
                alpenglow("steady", 100, 10, Some(1000)),
            ],
            &[("grown", Some(1.0)), ("steady", Some(1.0))],
        );
    }

    #[test]
    fn alpenglow_leaves_out_a_reward_without_epoch_stake() {
        assert_performance(
            &[
                alpenglow("a", 100, 10, Some(1000)),
                alpenglow("b", 100, 10, Some(1000)),
                VotePerformanceInput {
                    epoch_stake: None,
                    ..alpenglow("unknown", 100, 10, Some(1000))
                },
            ],
            &[("a", Some(1.0)), ("b", Some(1.0)), ("unknown", None)],
        );
    }

    #[test]
    fn tower_compares_credits_to_the_stake_weighted_mean() {
        // mean = (300·1000 + 100·600 + 100·400) / 500 = 800
        assert_performance(
            &[
                tower("a", 300, Some(1000)),
                tower("b", 100, Some(600)),
                tower("c", 100, Some(400)),
            ],
            &[("a", Some(1.25)), ("b", Some(0.75)), ("c", Some(0.5))],
        );
    }

    #[test]
    fn tower_leaves_null_credits_out_of_the_mean() {
        assert_performance(
            &[tower("a", 100, Some(100)), tower("unknown", 900, None)],
            &[("a", Some(1.0)), ("unknown", None)],
        );
    }

    #[test]
    fn tower_does_not_overflow_at_the_tower_credits_limit() {
        let stake = 44_000_000_000_000_000;
        let rows: Vec<_> = (0..500)
            .map(|i| tower(&i.to_string(), stake, Some(MAX_TOWER_CREDITS_IN_EPOCH)))
            .collect();
        assert_eq!(vote_performance(&rows).get("0"), Some(&1.0));
    }

    #[test]
    fn credits_above_the_tower_limit_are_left_out() {
        // Testnet epoch 1050: 4.4e16 lamports of stake and 4.8e13 in `credits`
        let stake = 44_000_000_000_000_000;
        let rows: Vec<_> = (0..500)
            .map(|i| tower(&i.to_string(), stake, Some(48_000_000_000_000)))
            .collect();
        assert!(vote_performance(&rows).is_empty());
        assert_eq!(EpochTotals::from_rows(&rows).kind(), None);
    }

    #[test]
    fn the_migration_epoch_has_no_value() {
        let rows = [
            VotePerformanceInput {
                credits: Some(500),
                ..alpenglow("up", 100, 10, Some(500))
            },
            tower("down", 100, Some(0)),
        ];
        assert_eq!(
            EpochTotals::from_rows(&rows).kind(),
            Some(EpochKind::Migration)
        );
        assert_performance(&rows, &[("up", None), ("down", None)]);
    }
}
