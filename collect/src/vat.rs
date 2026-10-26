use log::info;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::clock::Epoch;

use crate::slot_params::{select_slot_params, SlotParams};

/// agave `VoteStateV4::size_of()`, the size agave takes the rent of.
const VOTE_STATE_V4_SIZE: usize = 3762;

pub fn get_vote_state_rent(client: &RpcClient) -> anyhow::Result<u64> {
    Ok(client.get_minimum_balance_for_rent_exemption(VOTE_STATE_V4_SIZE)?)
}

/// The VAT agave burns at the end of `epoch`.
pub fn vat_at_end_of_epoch(
    slot_time_activations: &[(SlotParams, Option<Epoch>)],
    epoch: Epoch,
) -> u64 {
    // agave burns the VAT with the slot params of the first slot after the boundary.
    select_slot_params(slot_time_activations, epoch + 1).vat_lamports_per_epoch
}

/// agave `minimum_vote_account_balance_for_vat` at the end of `epoch`, with the VAT counted
/// also before Alpenglow activates.
pub fn get_vat_minimum_balance(
    client: &RpcClient,
    slot_time_activations: &[(SlotParams, Option<Epoch>)],
    epoch: Epoch,
) -> anyhow::Result<u64> {
    let rent = get_vote_state_rent(client)?;
    let vat = vat_at_end_of_epoch(slot_time_activations, epoch);
    info!("VAT minimum balance at the end of epoch {epoch}: rent {rent} + VAT {vat}");
    Ok(rent + vat)
}
