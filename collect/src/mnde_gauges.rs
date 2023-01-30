use std::collections::HashMap;

use anchor_lang::prelude::*;
use solana_account_decoder::*;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_program::pubkey::Pubkey;
use spl_governance::state::proposal::get_proposal_data;

#[derive(AnchorDeserialize, AnchorSerialize, Debug, Clone, Default)]
pub struct Bumps {
    pub template_option_list: u8,
    pub proposal_authority: u8,
}

#[derive(AnchorDeserialize, AnchorSerialize, Debug, Clone, PartialEq, Eq, Default)]
pub enum ProposalTemplateState {
    #[default]
    Idle,
    Inserting,
    InsertCompleted,
    Voting,
}

#[derive(Debug, Default, borsh::BorshDeserialize)]
pub struct ProposalTemplate {
    /// name of proposal template, used on proposal creation
    pub name: String,
    /// link to description used on proposal creation
    pub description_link: String,
    /// owner of this proposal template
    pub admin: Pubkey,
    /// authority that have rights to add and remove options
    /// no authority set (i.e., Pubkey::default) then add/remove authority is permission-less op
    /// when authority is set then only that authority may add/remove options
    pub options_authority: Pubkey,
    /// the governance program id
    pub governance_program: Pubkey,
    /// the governance that this proposal template belongs to
    pub governance: Pubkey,
    /// marinade state account of liquid-staking-program
    pub marinade_state: Pubkey,

    /// only the token owner record defined at initialization has permission
    /// to create the proposal over from this template contract
    pub proposal_token_owner_record: Pubkey,
    /// token mint the proposal is created with; needs to match the governance
    pub governing_token_mint: Pubkey,

    /// proposal that was created; Pubkey::default() considered as unset
    pub active_proposal: Pubkey,
    /// saves the last completed proposal
    pub last_valid_voted_proposal: Pubkey,

    /// Calculation of the epoch timings. Epoch is considered here a period for proposal creation.
    /// A proposal can be created from the proposal template only once at one epoch.
    /// Time is calculated in seconds (like as it's unix timestamp)
    /// epoch label is end of epoch in which the last proposal was created
    pub next_epoch_start_time: i64,
    pub epoch_length_time: i64,

    /// index of last inserted option from template to proposal
    pub inserted_options_index: u32,

    /// bump on PDA creation
    pub bumps: Bumps,

    /// state that the proposal template is currently in
    /// when insertion to a proposal is happening there is not permitted to add or remove options
    pub state: ProposalTemplateState,

    /// how many options were created
    pub created_count: u64,
}

pub fn load_mnde_gauges(
    rpc_client: &RpcClient,
    validator_gauges_program: Pubkey,
) -> anyhow::Result<HashMap<String, u64>> {
    let proposal_template_accounts = rpc_client.get_program_accounts_with_config(
        &validator_gauges_program,
        RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(Memcmp {
                offset: 0,
                bytes: MemcmpEncodedBytes::Binary("discriminator".to_string()),
                encoding: None,
            })]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(rpc_client.commitment()),
                min_context_slot: None,
                data_slice: None,
            },
            with_context: None,
        },
    )?;

    let proposal_templates: Vec<ProposalTemplate> = proposal_template_accounts
        .iter()
        .flat_map(|(_, account)| ProposalTemplate::deserialize(&mut &account.data[8..]))
        .collect();

    let proposal_template = if let Some(proposal_template) = proposal_templates.first() {
        proposal_template
    } else {
        anyhow::bail!("No MNDE gauges proposal tempalate found!");
    };

    let mut proposal_data = if let Some(proposal_data) = rpc_client
        .get_account_with_config(
            &proposal_template.last_valid_voted_proposal,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(rpc_client.commitment()),
                ..RpcAccountInfoConfig::default()
            },
        )?
        .value
    {
        proposal_data
    } else {
        anyhow::bail!("MNDE gauges proposal data could not be fetched!");
    };

    let proposal_info = AccountInfo::new(
        &proposal_template.last_valid_voted_proposal,
        false,
        false,
        &mut proposal_data.lamports,
        &mut proposal_data.data[..],
        &proposal_data.owner,
        false,
        proposal_data.rent_epoch,
    );

    let proposal = get_proposal_data(&proposal_template.governance_program, &proposal_info)?;

    let votes: HashMap<_, _> = proposal
        .options
        .iter()
        .flat_map(|option| match Pubkey::try_from(&option.label[..]) {
            Ok(vote_address) => Some((vote_address.to_string(), option.vote_weight)),
            _ => None,
        })
        .collect();

    Ok(votes)
}
