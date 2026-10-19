use crate::common::CommonParams;
use crate::solana_service::*;
use clap::Parser;
use log::info;
use serde::{Deserialize, Serialize};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcGetVoteAccountsConfig;
use solana_rpc_client_api::response::{RpcVoteAccountInfo, RpcVoteAccountStatus};
use solana_sdk::clock::Epoch;
use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;
use std::time::Duration;

#[derive(Debug, Parser)]
pub struct CreditsBackfillParams {
    #[arg(long = "from-epoch")]
    from_epoch: Epoch,

    #[arg(long = "to-epoch")]
    to_epoch: Epoch,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreditsBackfill {
    pub epochs: BTreeMap<Epoch, HashMap<String, EpochCredits>>,
}

/// Each vote account with a history gets an entry in each epoch, (None, None) where it has no value.
pub fn credits_backfill(
    vote_accounts: &RpcVoteAccountStatus,
    histories: &HashMap<String, Vec<EpochCreditsEntry>>,
    epochs: RangeInclusive<Epoch>,
    alpenglow_migration_epoch: Option<Epoch>,
) -> CreditsBackfill {
    let mut backfill = CreditsBackfill {
        epochs: epochs
            .clone()
            .map(|epoch| (epoch, HashMap::new()))
            .collect(),
    };
    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let Some(history) = histories.get(&vote_account.vote_pubkey) else {
            continue;
        };
        // split_epoch_credits gives a 0 reward for an epoch with no entry only when activated_stake > 0.
        // The current stake is not the stake of a past epoch, so 1 always gives the 0, and store clears it for rows with no stake.
        let vote_account = RpcVoteAccountInfo {
            epoch_credits: history.clone(),
            activated_stake: 1,
            ..vote_account.clone()
        };
        for epoch in epochs.clone() {
            let credits = split_epoch_credits(&vote_account, epoch, alpenglow_migration_epoch)
                .unwrap_or(EpochCredits {
                    tower_credits: None,
                    vote_reward_lamports: None,
                });
            backfill
                .epochs
                .get_mut(&epoch)
                .unwrap()
                .insert(vote_account.vote_pubkey.clone(), credits);
        }
    }
    backfill
}

fn get_all_vote_accounts(client: &RpcClient) -> anyhow::Result<RpcVoteAccountStatus> {
    Ok(
        client.get_vote_accounts_with_config(RpcGetVoteAccountsConfig {
            keep_unstaked_delinquents: Some(true),
            ..Default::default()
        })?,
    )
}

pub fn collect_credits_backfill(
    common_params: CommonParams,
    params: CreditsBackfillParams,
) -> anyhow::Result<()> {
    let (from_epoch, to_epoch) = (params.from_epoch, params.to_epoch);
    info!("Collecting the credits backfill of epochs {from_epoch}..={to_epoch}");
    let client = solana_client_with_timeout(
        common_params.rpc_url,
        Duration::from_secs(params.rpc_timeout),
        common_params.commitment,
    );
    let vote_accounts = get_all_vote_accounts(&client)?;
    let vote_pubkeys: Vec<String> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|vote_account| vote_account.vote_pubkey.clone())
        .collect();
    let histories = get_full_epoch_credits(&client, &vote_pubkeys)?;
    info!(
        "Read the epochCredits of {} of {} vote accounts",
        histories.len(),
        vote_pubkeys.len()
    );
    let backfill = credits_backfill(
        &vote_accounts,
        &histories,
        from_epoch..=to_epoch,
        is_alpenglow_active(&client)?,
    );
    for (epoch, credits) in &backfill.epochs {
        let tower = credits
            .values()
            .filter(|c| c.tower_credits.is_some())
            .count();
        let rewards = credits
            .values()
            .filter(|c| c.vote_reward_lamports.is_some())
            .count();
        info!(
            "Epoch {epoch}: {} vote accounts, {tower} with tower credits, {rewards} with vote reward lamports",
            credits.len()
        );
    }
    serde_yaml::to_writer(std::io::stdout(), &backfill)?;
    Ok(())
}
