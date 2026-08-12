use log::info;
use solana_client::rpc_client::RpcClient;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Agave's pre-SIMD-0525 baseline: no gate governs it, so no account can be read for it.
const BASELINE_SLOT_PARAMS: SlotParams = SlotParams {
    slot_time_ms: 400,
    slots_per_year: 78_892_314.984,
};

/// Transcribed from agave `runtime/src/slot_params.rs`, which stores explicit rows rather than deriving them.
const SLOT_TIME_GATES: [(&str, SlotParams); 4] = [
    (
        "iBRL5RuWhw4yqaAZu96RUULHckHTZAoe2b77qaV38JZ",
        SlotParams {
            slot_time_ms: 350,
            slots_per_year: 90_162_645.696,
        },
    ),
    (
        "iBRLL3k18HST852F1Mf3Lv83waTNQmmqvKDxvYGwQFL",
        SlotParams {
            slot_time_ms: 300,
            slots_per_year: 105_189_753.312,
        },
    ),
    (
        "iBRLMc81UjRa8fn8A6eE8bJTnRbgQoPTynM51akENCV",
        SlotParams {
            slot_time_ms: 250,
            slots_per_year: 126_227_703.974,
        },
    ),
    (
        "iBRLjhJnkmDZgNoZRDMW11d8ZV7HvsL3vAyRjZB5npW",
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

/// Per epoch, never live: agave computes inflation piecewise, so today's regime must not reach a past epoch.
pub fn get_slots_per_year(client: &RpcClient, epoch: Epoch) -> anyhow::Result<f64> {
    let gate_ids: Vec<Pubkey> = SLOT_TIME_GATES
        .iter()
        .map(|(id, _)| Pubkey::from_str(id))
        .collect::<Result<_, _>>()?;
    let epoch_schedule = client.get_epoch_schedule()?;
    let accounts = client.get_multiple_accounts(&gate_ids)?;

    let mut params = BASELINE_SLOT_PARAMS;
    for ((gate_id, gate_params), account) in SLOT_TIME_GATES.iter().zip(accounts) {
        // A gate nobody has requested yet has no account at all; that is "not activated", not a failure.
        let Some(account) = account else { continue };
        let feature = solana_feature_gate_interface::from_account(&account)
            .ok_or_else(|| anyhow::anyhow!("Account {gate_id} is not a feature account"))?;
        let Some(activated_at) = feature.activated_at else {
            continue;
        };
        let effective_from = epoch_schedule.get_epoch(activated_at).saturating_add(1);
        // Gates may activate out of order; agave normalises so slot time never grows.
        if epoch >= effective_from && gate_params.slot_time_ms < params.slot_time_ms {
            params = *gate_params;
        }
    }

    info!(
        "Epoch {epoch} slot params: {}ms, {} slots/year",
        params.slot_time_ms, params.slots_per_year
    );
    Ok(params.slots_per_year)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agave's numerator, kept out of the table so a mistyped row cannot match a mistyped constant.
    const SECONDS_PER_YEAR: f64 = 31556925.9936;

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
    fn gate_pubkeys_parse() {
        for (id, _) in SLOT_TIME_GATES {
            Pubkey::from_str(id).unwrap();
        }
    }
}
