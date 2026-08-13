use log::info;
use solana_client::rpc_client::RpcClient;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;

/// Unchanged by SIMD-0525, which shortens slots and leaves the epoch's slot count alone.
pub const SLOTS_IN_EPOCH: u64 = 432_000;

/// Agave's pre-SIMD-0525 baseline: no gate governs it, so no account can be read for it.
const BASELINE_SLOT_PARAMS: SlotParams = SlotParams {
    slot_time_ms: 400,
    slots_per_year: 78_892_314.984,
};

/// Transcribed from agave `runtime/src/slot_params.rs`, which stores explicit rows rather than deriving them.
const SLOT_TIME_GATES: [(Pubkey, SlotParams); 4] = [
    (
        Pubkey::from_str_const("iBRL5RuWhw4yqaAZu96RUULHckHTZAoe2b77qaV38JZ"),
        SlotParams {
            slot_time_ms: 350,
            slots_per_year: 90_162_645.696,
        },
    ),
    (
        Pubkey::from_str_const("iBRLL3k18HST852F1Mf3Lv83waTNQmmqvKDxvYGwQFL"),
        SlotParams {
            slot_time_ms: 300,
            slots_per_year: 105_189_753.312,
        },
    ),
    (
        Pubkey::from_str_const("iBRLMc81UjRa8fn8A6eE8bJTnRbgQoPTynM51akENCV"),
        SlotParams {
            slot_time_ms: 250,
            slots_per_year: 126_227_703.974,
        },
    ),
    (
        Pubkey::from_str_const("iBRLjhJnkmDZgNoZRDMW11d8ZV7HvsL3vAyRjZB5npW"),
        SlotParams {
            slot_time_ms: 200,
            slots_per_year: 157_784_629.968,
        },
    ),
];

#[derive(Clone, Copy, Debug)]
struct SlotParams {
    slot_time_ms: u64,
    slots_per_year: f64,
}

/// Reachable only from snapshots that predate the gate read, which are all pre-SIMD-0525 epochs.
pub fn baseline_slots_per_year() -> f64 {
    BASELINE_SLOT_PARAMS.slots_per_year
}

/// Per epoch, never live: agave computes inflation piecewise, so today's regime must not reach a past epoch.
pub fn get_slots_per_year(client: &RpcClient, epoch: Epoch) -> anyhow::Result<f64> {
    let gate_ids: Vec<Pubkey> = SLOT_TIME_GATES.iter().map(|(id, _)| *id).collect();
    let epoch_schedule = client.get_epoch_schedule()?;
    let accounts = client.get_multiple_accounts(&gate_ids)?;

    let mut activations = Vec::with_capacity(SLOT_TIME_GATES.len());
    for ((gate_id, gate_params), account) in SLOT_TIME_GATES.iter().zip(accounts) {
        let slot_time_ms = gate_params.slot_time_ms;
        let activation_epoch = match account {
            // A gate nobody has requested yet has no account at all; that is "not activated", not a failure.
            None => {
                info!("Slot time gate {gate_id} ({slot_time_ms}ms): no account on chain");
                None
            }
            Some(account) => {
                let feature = solana_feature_gate_interface::from_account(&account)
                    .ok_or_else(|| anyhow::anyhow!("Account {gate_id} is not a feature account"))?;
                match feature.activated_at {
                    Some(activated_at) => {
                        let activation_epoch = epoch_schedule.get_epoch(activated_at);
                        info!("Slot time gate {gate_id} ({slot_time_ms}ms): activated in epoch {activation_epoch}");
                        Some(activation_epoch)
                    }
                    None => {
                        info!("Slot time gate {gate_id} ({slot_time_ms}ms): account exists, not activated");
                        None
                    }
                }
            }
        };
        activations.push((*gate_params, activation_epoch));
    }

    let params = select_slot_params(&activations, epoch);
    info!(
        "Epoch {epoch} slot params: {}ms, {} slots/year",
        params.slot_time_ms, params.slots_per_year
    );
    Ok(params.slots_per_year)
}

// SIMD-0525: a gate active in epoch E first applies at the first slot of E+1 (agave `feature_effective_slot`).
fn select_slot_params(activations: &[(SlotParams, Option<Epoch>)], epoch: Epoch) -> SlotParams {
    let mut params = BASELINE_SLOT_PARAMS;
    for (gate_params, activation_epoch) in activations {
        let Some(activation_epoch) = activation_epoch else {
            continue;
        };
        // Gates may activate out of order; agave normalises so slot time never grows.
        if epoch > *activation_epoch && gate_params.slot_time_ms < params.slot_time_ms {
            params = *gate_params;
        }
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agave's numerator, kept out of the table so a mistyped row cannot match a mistyped constant.
    const SECONDS_PER_YEAR: f64 = 31556925.9936;

    const GATE_350MS: SlotParams = SLOT_TIME_GATES[0].1;
    const GATE_200MS: SlotParams = SLOT_TIME_GATES[3].1;

    #[test]
    fn every_row_annualises_to_the_tropical_year() {
        for params in SLOT_TIME_GATES
            .iter()
            .map(|(_, params)| params)
            .chain([&BASELINE_SLOT_PARAMS])
        {
            let derived = SECONDS_PER_YEAR / (params.slot_time_ms as f64 / 1000.0);
            let relative_error = (derived - params.slots_per_year).abs() / derived;
            assert!(
                relative_error < 1e-9,
                "{}ms row is {} slots/year, expected ~{derived}",
                params.slot_time_ms,
                params.slots_per_year
            );
        }
    }

    #[test]
    fn gates_are_ordered_longest_slot_first_and_shorter_than_baseline() {
        let mut previous = BASELINE_SLOT_PARAMS.slot_time_ms;
        for (_, params) in SLOT_TIME_GATES {
            assert!(params.slot_time_ms < previous);
            previous = params.slot_time_ms;
        }
    }

    #[test]
    fn a_gate_that_never_activated_keeps_the_baseline() {
        let activations = [(GATE_350MS, None), (GATE_200MS, None)];
        let params = select_slot_params(&activations, 1000);
        assert_eq!(params.slot_time_ms, BASELINE_SLOT_PARAMS.slot_time_ms);
        assert_eq!(params.slots_per_year, BASELINE_SLOT_PARAMS.slots_per_year);
    }

    #[test]
    fn the_activation_epoch_itself_still_runs_the_previous_params() {
        let activations = [(GATE_350MS, Some(1000))];
        assert_eq!(select_slot_params(&activations, 1000).slot_time_ms, 400);
        assert_eq!(select_slot_params(&activations, 1001).slot_time_ms, 350);
    }

    #[test]
    fn a_past_epoch_never_inherits_a_later_regime() {
        let activations = [(GATE_350MS, Some(1000))];
        assert_eq!(
            select_slot_params(&activations, 999).slots_per_year,
            BASELINE_SLOT_PARAMS.slots_per_year
        );
    }

    #[test]
    fn slot_time_never_grows_when_gates_activate_out_of_order() {
        let activations = [(GATE_350MS, Some(20)), (GATE_200MS, Some(10))];
        assert_eq!(select_slot_params(&activations, 15).slot_time_ms, 200);
        assert_eq!(select_slot_params(&activations, 25).slot_time_ms, 200);
    }

    #[test]
    fn the_shortest_effective_gate_wins() {
        let activations = [(GATE_350MS, Some(10)), (GATE_200MS, Some(10))];
        assert_eq!(select_slot_params(&activations, 11).slot_time_ms, 200);
    }
}
