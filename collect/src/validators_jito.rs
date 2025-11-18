use crate::{common::*, solana_service::solana_client_with_timeout};
use anchor_lang::AccountDeserialize;
use jito_priority_fee_distribution::state::PriorityFeeDistributionAccount;
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
use std::fmt;
use std::time::Duration;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct JitoParams {
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
        long = "epoch",
        help = "Overriding 'epoch' to act as if current epoch was set to this value."
    )]
    epoch: Option<u64>,
}

const JITO_SNAPSHOT_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum JitoAccountType {
    MevTipDistribution,
    PriorityFeeDistribution,
}

impl fmt::Display for JitoAccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JitoAccountType::MevTipDistribution => write!(f, "MevTipDistribution"),
            JitoAccountType::PriorityFeeDistribution => write!(f, "PriorityFeeDistribution"),
        }
    }
}

impl JitoAccountType {
    pub fn account_size(&self) -> usize {
        match self {
            JitoAccountType::MevTipDistribution => TipDistributionAccount::SIZE,
            JitoAccountType::PriorityFeeDistribution => PriorityFeeDistributionAccount::SIZE,
        }
    }

    pub fn program_id(&self) -> Pubkey {
        match self {
            JitoAccountType::MevTipDistribution => "4R3gSG8BpU4t19KYj8CfnbtRpnT8gtk4dvTHxVRwc2r7"
                .parse()
                .expect("Invalid Jito Tip Distribution program ID"),
            JitoAccountType::PriorityFeeDistribution => {
                "Priority6weCZ5HwDn29NxLFpb7TDp2iLZ6XKc5e8d3"
                    .parse()
                    .expect("Invalid Jito Priority Fee Distribution program ID")
            }
        }
    }

    pub fn db_table_name(&self) -> &'static str {
        match self {
            JitoAccountType::MevTipDistribution => "mev",
            JitoAccountType::PriorityFeeDistribution => "jito_priority_fee",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MevTipDistributionValidatorSnapshot {
    pub vote_account: String,
    pub mev_commission: u16, // in basis points
    pub epoch: u64,
    pub total_epoch_rewards: Option<u64>,
    pub claimed_epoch_rewards: Option<u64>,
    pub total_epoch_claimants: Option<u64>,
    pub epoch_active_claimants: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PriorityFeeDistributionValidatorSnapshot {
    pub vote_account: String,
    pub priority_commission: u16, // in basis points
    pub total_lamports_transferred: u64,
    pub merkle_root_upload_authority: String,
    pub epoch: u64,
    pub total_epoch_rewards: Option<u64>,
    pub claimed_epoch_rewards: Option<u64>,
    pub total_epoch_claimants: Option<u64>,
    pub epoch_active_claimants: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ValidatorSnapshot {
    MevTipDistribution(MevTipDistributionValidatorSnapshot),
    PriorityFeeDistribution(PriorityFeeDistributionValidatorSnapshot),
}

impl ValidatorSnapshot {
    pub fn vote_account(&self) -> &str {
        match self {
            ValidatorSnapshot::MevTipDistribution(snapshot) => &snapshot.vote_account,
            ValidatorSnapshot::PriorityFeeDistribution(snapshot) => &snapshot.vote_account,
        }
    }

    pub fn epoch(&self) -> u64 {
        match self {
            ValidatorSnapshot::MevTipDistribution(snapshot) => snapshot.epoch,
            ValidatorSnapshot::PriorityFeeDistribution(snapshot) => snapshot.epoch,
        }
    }

    pub fn total_epoch_rewards(&self) -> Option<u64> {
        match self {
            ValidatorSnapshot::MevTipDistribution(snapshot) => snapshot.total_epoch_rewards,
            ValidatorSnapshot::PriorityFeeDistribution(snapshot) => snapshot.total_epoch_rewards,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JitoSnapshot {
    pub epoch: Epoch,
    pub version: u16,
    pub account_type: JitoAccountType,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub validators: HashMap<String, ValidatorSnapshot>,
}

impl JitoSnapshot {
    pub fn get_mev_validators(&self) -> HashMap<String, &MevTipDistributionValidatorSnapshot> {
        let mut result = HashMap::new();
        if matches!(self.account_type, JitoAccountType::MevTipDistribution) {
            for (vote_account, validator_snapshot) in &self.validators {
                if let ValidatorSnapshot::MevTipDistribution(mev_snapshot) = validator_snapshot {
                    result.insert(vote_account.clone(), mev_snapshot);
                }
            }
        }
        result
    }

    pub fn get_priority_fee_validators(
        &self,
    ) -> HashMap<String, &PriorityFeeDistributionValidatorSnapshot> {
        let mut result = HashMap::new();
        if matches!(self.account_type, JitoAccountType::PriorityFeeDistribution) {
            for (vote_account, validator_snapshot) in &self.validators {
                if let ValidatorSnapshot::PriorityFeeDistribution(priority_snapshot) =
                    validator_snapshot
                {
                    result.insert(vote_account.clone(), priority_snapshot);
                }
            }
        }
        result
    }
}

fn fetch_program_accounts_with_retry(
    client: &RpcClient,
    jito_account_type: &JitoAccountType,
    byte_pos: usize,
    epoch: u64,
    rpc_attempts: usize,
) -> anyhow::Result<Vec<(Pubkey, Account)>> {
    let program_id = jito_account_type.program_id();
    let account_size = jito_account_type.account_size();
    retry_blocking(
        || {
            client.get_program_accounts_with_config(
                &program_id,
                RpcProgramAccountsConfig {
                    filters: Some(vec![
                        RpcFilterType::DataSize(account_size.try_into().unwrap()),
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
                    sort_results: None,
                },
            )
        },
        QuadraticBackoffStrategy::iter_durations(rpc_attempts),
        |err, attempt, backoff| {
            warn!(
                "Attempt {} has failed: {}, retrying in {:?} seconds",
                attempt,
                err,
                backoff.as_secs()
            )
        },
    )
    .map_err(|e| {
        anyhow::Error::new(e).context(format!(
            "Failed to fetch program accounts for program_id: {program_id} at index {byte_pos}"
        ))
    })
}

pub fn jito_accounts(
    client: &RpcClient,
    account_type: &JitoAccountType,
    epoch: Epoch,
    rpc_attempts: usize,
) -> anyhow::Result<Vec<(Pubkey, Account)>> {
    // accounts created by validator during the epoch
    let tip_distribution_accounts_before_update = fetch_program_accounts_with_retry(
        client,
        account_type,
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
        client,
        account_type,
        0x89, // byte 137
        epoch,
        rpc_attempts,
    )?;
    info!(
        "RPC loaded {} validators tip distribution accounts in state of already updated by JITO for epoch {}",
        distribution_accounts_updated.len(),
        epoch,
    );

    let loaded_accounts: Vec<(Pubkey, Account)> = tip_distribution_accounts_before_update
        .into_iter()
        .chain(distribution_accounts_updated)
        .collect();

    info!(
        "Loaded {} jito accounts for epoch {}",
        loaded_accounts.len(),
        epoch
    );

    Ok(loaded_accounts)
}

fn deserialize_mev_tip_distribution(
    accounts: &[(Pubkey, Account)],
    epoch: Epoch,
) -> anyhow::Result<HashMap<String, ValidatorSnapshot>> {
    let mut validators: HashMap<String, ValidatorSnapshot> = Default::default();
    for mev_tip_distribution_account in accounts {
        let fetched_tip_distribution_account: TipDistributionAccount =
            AccountDeserialize::try_deserialize(
                &mut mev_tip_distribution_account.1.data.as_slice(),
            )?;
        if fetched_tip_distribution_account.epoch_created_at != epoch {
            continue;
        }
        let merkle_root = fetched_tip_distribution_account.merkle_root;

        validators.insert(
            fetched_tip_distribution_account
                .validator_vote_account
                .to_string(),
            ValidatorSnapshot::MevTipDistribution(MevTipDistributionValidatorSnapshot {
                vote_account: fetched_tip_distribution_account
                    .validator_vote_account
                    .to_string(),
                mev_commission: fetched_tip_distribution_account.validator_commission_bps,
                epoch,
                total_epoch_rewards: merkle_root.as_ref().map(|mr| mr.max_total_claim),
                claimed_epoch_rewards: merkle_root.as_ref().map(|mr| mr.total_funds_claimed),
                total_epoch_claimants: merkle_root.as_ref().map(|mr| mr.max_num_nodes),
                epoch_active_claimants: merkle_root.as_ref().map(|mr| mr.num_nodes_claimed),
            }),
        );
    }
    Ok(validators)
}

fn deserialize_priority_fee_distribution(
    accounts: &[(Pubkey, Account)],
    epoch: Epoch,
) -> anyhow::Result<HashMap<String, ValidatorSnapshot>> {
    let mut validators: HashMap<String, ValidatorSnapshot> = Default::default();

    for priority_fee_distribution_account in accounts {
        let fetched_priority_fee_distribution_account: PriorityFeeDistributionAccount =
            AccountDeserialize::try_deserialize(
                &mut priority_fee_distribution_account.1.data.as_slice(),
            )?;

        if fetched_priority_fee_distribution_account.epoch_created_at != epoch {
            continue;
        }

        let merkle_root = fetched_priority_fee_distribution_account.merkle_root;

        validators.insert(
            fetched_priority_fee_distribution_account
                .validator_vote_account
                .to_string(),
            ValidatorSnapshot::PriorityFeeDistribution(PriorityFeeDistributionValidatorSnapshot {
                vote_account: fetched_priority_fee_distribution_account
                    .validator_vote_account
                    .to_string(),
                priority_commission: fetched_priority_fee_distribution_account
                    .validator_commission_bps,
                total_lamports_transferred: fetched_priority_fee_distribution_account
                    .total_lamports_transferred,
                merkle_root_upload_authority: fetched_priority_fee_distribution_account
                    .merkle_root_upload_authority
                    .to_string(),
                epoch,
                total_epoch_rewards: merkle_root.as_ref().map(|mr| mr.max_total_claim),
                claimed_epoch_rewards: merkle_root.as_ref().map(|mr| mr.total_funds_claimed),
                total_epoch_claimants: merkle_root.as_ref().map(|mr| mr.max_num_nodes),
                epoch_active_claimants: merkle_root.as_ref().map(|mr| mr.num_nodes_claimed),
            }),
        );
    }
    Ok(validators)
}

pub fn collect_jito_info(
    common_params: CommonParams,
    jito_params: JitoParams,
    account_type: JitoAccountType,
) -> anyhow::Result<()> {
    info!("Collecting snapshot of JITO validators accounts");
    let timeout = Duration::from_secs(jito_params.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");
    let looking_at_epoch = jito_params.epoch.unwrap_or(current_epoch_info.epoch - 1);
    info!("Looking at epoch: {looking_at_epoch}");

    let raw_accounts = jito_accounts(
        &client,
        &account_type,
        looking_at_epoch,
        jito_params.rpc_attempts,
    )?;

    let validators = match account_type {
        JitoAccountType::MevTipDistribution => {
            info!("Deserializing MEV Tip Distribution accounts for epoch {looking_at_epoch}");
            deserialize_mev_tip_distribution(&raw_accounts, looking_at_epoch)?
        }
        JitoAccountType::PriorityFeeDistribution => {
            info!("Deserializing Priority Fee Distribution accounts for epoch {looking_at_epoch}");
            deserialize_priority_fee_distribution(&raw_accounts, looking_at_epoch)?
        }
    };

    serde_yaml::to_writer(
        std::io::stdout(),
        &JitoSnapshot {
            version: JITO_SNAPSHOT_VERSION,
            account_type: account_type.clone(),
            epoch: looking_at_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            validators,
        },
    )?;

    Ok(())
}
