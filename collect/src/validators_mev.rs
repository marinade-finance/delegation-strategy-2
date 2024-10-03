use crate::{common::*, solana_service::solana_client_with_timeout};
use anchor_lang::AccountDeserialize;
use jito_tip_distribution::state::TipDistributionAccount;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_program::pubkey::Pubkey;
use solana_sdk::account::Account;
use solana_sdk::clock::Epoch;
use std::collections::HashMap;
use std::time::Duration;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ValidatorsMEVOptions {
    #[structopt(
        long = "rpc-attempts",
        help = "How many times to retry the operation.",
        default_value = "10"
    )]
    rpc_attempts: usize,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[structopt(
        long = "current-epoch-override",
        help = "Act as if current epoch was set to this value."
    )]
    current_epoch_override: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ValidatorMEVSnapshot {
    pub vote_account: String,
    pub mev_commission: u16,
    pub epoch: u64,
    pub total_epoch_rewards: Option<u64>,
    pub claimed_epoch_rewards: Option<u64>,
    pub total_epoch_claimants: Option<u64>,
    pub epoch_active_claimants: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub validators: HashMap<String, ValidatorMEVSnapshot>,
}

fn fetch_program_accounts_with_retry(
    client: &RpcClient,
    program_id: &str,
    byte_pos: usize,
    epoch: u64,
    rpc_attempts: usize,
) -> anyhow::Result<Vec<(Pubkey, Account)>> {
    let program_key = program_id.try_into()?;
    retry_blocking(
        || {
            client.get_program_accounts_with_config(
                &program_key,
                RpcProgramAccountsConfig {
                    filters: Some(vec![
                        RpcFilterType::DataSize(TipDistributionAccount::SIZE.try_into().unwrap()),
                        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                            byte_pos,
                            epoch.to_le_bytes().to_vec(),
                        )),
                    ]),
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64),
                        data_slice: None,
                        commitment: None,
                        min_context_slot: None,
                    },
                    with_context: None,
                },
            )
        },
        QuadraticBackoffStrategy::new(rpc_attempts),
        |err, attempt, backoff| {
            warn!(
                "Attempt {} has failed: {}, retrying in {:?} seconds",
                attempt,
                err.to_string(),
                backoff.as_secs()
            )
        },
    )
    .map_err(|e| {
        anyhow::Error::new(e).context(format!(
            "Failed to fetch program accounts for program_id: {} at index {}",
            program_id, byte_pos
        ))
    })
}

pub fn validators_mev(
    client: &RpcClient,
    epoch: Epoch,
    rpc_attempts: usize,
) -> anyhow::Result<HashMap<String, ValidatorMEVSnapshot>> {
    let mut validators: HashMap<String, ValidatorMEVSnapshot> = Default::default();

    let jito_program = "4R3gSG8BpU4t19KYj8CfnbtRpnT8gtk4dvTHxVRwc2r7".try_into()?;
    // accounts created by validator during the epoch
    let tip_distribution_accounts_before_update = fetch_program_accounts_with_retry(
        &client,
        jito_program,
        0x49, // byte 73
        epoch,
        rpc_attempts,
    )?;
    info!(
        "RPC loaded {} validators tip distribution accounts in state of before updated by JITO for epoch {}",
        tip_distribution_accounts_before_update.len(),
        epoch,
    );
    // account with data uploaded by jito at the start of the next epoch
    let distribution_accounts_updated = fetch_program_accounts_with_retry(
        &client,
        jito_program,
        0x89, // byte 137
        epoch,
        rpc_attempts,
    )?;
    info!(
        "RPC loaded {} validators tip distribution accounts in state of already updated by JITO for epoch {}",
        distribution_accounts_updated.len(),
        epoch,
    );

    for validator_tip_distribution_account in tip_distribution_accounts_before_update
        .into_iter()
        .chain(distribution_accounts_updated.into_iter())
    {
        let fetched_tip_distribution_account: TipDistributionAccount =
            AccountDeserialize::try_deserialize(
                &mut validator_tip_distribution_account.1.data.as_slice(),
            )?;
        if fetched_tip_distribution_account.epoch_created_at != epoch {
            continue;
        }
        let merkle_root = fetched_tip_distribution_account.merkle_root;

        validators.insert(
            fetched_tip_distribution_account
                .validator_vote_account
                .to_string(),
            ValidatorMEVSnapshot {
                vote_account: fetched_tip_distribution_account
                    .validator_vote_account
                    .to_string(),
                mev_commission: fetched_tip_distribution_account.validator_commission_bps,
                epoch,
                total_epoch_rewards: merkle_root.as_ref().map(|mr| mr.max_total_claim),
                claimed_epoch_rewards: merkle_root.as_ref().map(|mr| mr.total_funds_claimed),
                total_epoch_claimants: merkle_root.as_ref().map(|mr| mr.max_num_nodes),
                epoch_active_claimants: merkle_root.as_ref().map(|mr| mr.num_nodes_claimed),
            },
        );
    }

    info!("Loaded {} validators for epoch {}", validators.len(), epoch);

    Ok(validators)
}

pub fn collect_validators_mev_info(
    common_params: CommonParams,
    options: ValidatorsMEVOptions,
) -> anyhow::Result<()> {
    info!("Collecting snapshot of validators MEV");
    let timeout = Duration::from_secs(options.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    let epoch = options
        .current_epoch_override
        .unwrap_or(current_epoch_info.epoch);
    info!("Current epoch: {:?}", current_epoch_info);
    let looking_at_epoch = epoch - 1;
    info!("Looking at epoch: {}", looking_at_epoch);

    let validators = validators_mev(&client, looking_at_epoch, options.rpc_attempts)?;

    serde_yaml::to_writer(
        std::io::stdout(),
        &Snapshot {
            epoch: looking_at_epoch,
            loaded_at_epoch: epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            validators,
        },
    )?;

    Ok(())
}
