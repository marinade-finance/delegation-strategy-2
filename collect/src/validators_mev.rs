use crate::common::*;
use crate::solana_service::solana_client;
use anchor_lang::AccountDeserialize;
use log::info;
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::RpcFilterType,
};
use solana_sdk::clock::Epoch;
use std::collections::HashMap;
use tip_distribution::state::TipDistributionAccount;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ValidatorMEVSnapshot {
    pub vote_account: String,
    pub mev_commission: u16,
    pub epoch: u64,
    pub total_epoch_rewards: u64,
    pub claimed_epoch_rewards: u64,
    pub total_epoch_claimants: u64,
    pub epoch_active_claimants: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: Epoch,
    pub epoch_slot: u64,
    pub created_at: String,
    pub validators: HashMap<String, ValidatorMEVSnapshot>,
}

pub fn validators_mev(
    client: &RpcClient,
    epoch: Epoch,
) -> anyhow::Result<HashMap<String, ValidatorMEVSnapshot>> {
    let mut validators: HashMap<String, ValidatorMEVSnapshot> = Default::default();

    let jito_program = "4R3gSG8BpU4t19KYj8CfnbtRpnT8gtk4dvTHxVRwc2r7".try_into()?;
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(
            TipDistributionAccount::SIZE.try_into().unwrap(),
        )]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: None,
            commitment: None,
            min_context_slot: None,
        },
        with_context: None,
    };
    let validators_tip_distribution_accounts =
        client.get_program_accounts_with_config(&jito_program, config)?;
    for validator_tip_distribution_account in validators_tip_distribution_accounts {
        let fetched_tip_distribution_account: TipDistributionAccount = AccountDeserialize::try_deserialize(
            &mut validator_tip_distribution_account.1.data.as_slice(),
        )?;
        if fetched_tip_distribution_account.epoch_created_at != epoch - 1 {
            continue;
        }
        if let Some(merkle_root) = fetched_tip_distribution_account.merkle_root {
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
                    total_epoch_rewards: merkle_root.max_total_claim,
                    claimed_epoch_rewards: merkle_root.total_funds_claimed,
                    total_epoch_claimants: merkle_root.max_num_nodes,
                    epoch_active_claimants: merkle_root.num_nodes_claimed,
                },
            );
        }
    }

    Ok(validators)
}

pub fn collect_validators_mev_info(common_params: CommonParams) -> anyhow::Result<()> {
    info!("Collecting snaphost of validators MEV");
    let client = solana_client(common_params.rpc_url, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    let epoch = current_epoch_info.epoch;
    info!("Current epoch: {:?}", current_epoch_info);
    info!("Looking at epoch: {}", epoch - 1);

    let validators = validators_mev(&client, epoch)?;

    serde_yaml::to_writer(
        std::io::stdout(),
        &Snapshot {
            epoch,
            epoch_slot: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            validators,
        },
    )?;

    Ok(())
}
