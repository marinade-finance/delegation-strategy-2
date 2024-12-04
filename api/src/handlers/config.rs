use crate::context::WrappedContext;
use serde::{Deserialize, Serialize};
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResponseConfig {
    stakes: ConfigStakes,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ConfigStakes {
    delegation_authorities: Vec<StakeDelegationAuthorityRecord>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct StakeDelegationAuthorityRecord {
    delegation_authority: String,
    name: String,
}

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Show configuration of the API",
    path = "/static/config",
    responses(
        (status = 200, body = ResponseConfig)
    )
)]
pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    log::info!("Serving the configuration data");
    Ok(warp::reply::with_status(
        reply::json(&ResponseConfig {
            stakes: ConfigStakes {
                delegation_authorities: vec![
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk".into(),
                        name: "Marinade Liquid".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "stWirqFCf2Uts1JBL1Jsd3r6VBWhgnpdPxCTe1MFjrq".into(),
                        name: "Marinade Native".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "STNi1NHDUi6Hvibvonawgze8fM83PFLeJhuGMEXyGps".into(),
                        name: "Marinade Institutional".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5".into(),
                        name: "Solana Foundation".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6iQKfEyhr3bZMotVkW6beNZz5CPAkiwvgV2CTje9pVSS".into(),
                        name: "Jito".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "W1ZQRwUfSkDKy2oefRBUWph82Vr2zg9txWMA8RQazN5".into(),
                        name: "Lido".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "HbJTxftxnXgpePCshA8FubsRj9MW4kfPscfuUfn44fnt".into(),
                        name: "Jpool".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6WecYymEARvjG5ZyqkrVQ6YkhPfujNzWpSPwNKXHCbV2".into(),
                        name: "Blaze Stake".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "FZEaZMmrRC3PDPFMzqooKLS2JjoyVkKNd2MkHjr7Xvyq".into(),
                        name: "Edgevana".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "5LimmCEyVpxPz2FieDU8SBPQNKcWgiyffWqq1bgT4r6B".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "Au1zoNuqww4P6gwMWaVUPE9ycGgbacvdEAhRqwTJ8Ds".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "GJBHoP4xCyADCxvHna43JzUwLaSYFNwGvQEzXZNgknTu".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "CKxHxnS3UrVHBvZbSBjYPLeAW154HodQone1ds64sRh8".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6PgNvFjPeRfQ1n8hnppGfuP9eY8Awiiu3TepcmF5Dj2w".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "97wZQHcTVggmmCmske39KL1AZ9sYpmBQB4fBpoYw9GkA".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "AbW22oiVMJu8ed1UdENfFuFj7Tq2iWh2RBDE6vRxdF3j".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "GgwpwMuAMHqfoKg6VWtvTQchY7KQ5RxSv3TBd8UTnbnc".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4Vw2k7U9Rc7uUAhxzTetBX6nUuMLzs26bZddXBLyu8Fv".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BbspKrYQbXEFkPP3tNFRQfjM1sHnqRQKvw5QZtCCbzKA".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "HeiSZ9EAwkaPNWSSEdg1Us76Q9pK4Q6KSg3cVgPc8Ham".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "FZyTcYhUmfXbSbKeiabZXqPAfCAxQv6fhQsrWS1KJbTB".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4VkZGaxNK3vsY7WqahXJ276TfoXLdmbcpZDAUBRY5dco".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "2z2kU3Yri2ZNAq47YQNR67BkwNb5ErErkPS2waUFUMpz".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BXSpM6riPZcEoQ81L9XcxSfnUcmmgJEpfeQnwXfwTEP4".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "X1uNWxTp4fJsyXEfuRvfnpMmmMs9UxtsKQcHC9f1gJz".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "iu2PVhac9YoU9VxdFPWzrtnf4urwnoAYcuFn6PaWrvc".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "98eFuNynKYMixPS3rv5NYoZ5JmmNRGXD6ye7bd5bKmDX".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "AnAWyVA54LndUPDEHYQfvaHDPLfAMVBM6M3FqQ3eUiBu".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "75FJVW2YpPc6zfnJyD1PfbiioQ3kcdBz5phykfunhzVE".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4qCuUi9e5cx4SH2SivTf4aiEQiARYQAcDUWEKAsGbkdy".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "12Euh1pmCycRLq4grkyY7VDubawWJKXxbX9FJQAs4hZy".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "7zcgtavS6poA5kT8XLbiYuDxCnVaAyxTYL77i7NfeTyW".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "2pd8iFJPb1ThdVzoqDQXphYbwrKBfVA5yrXo8D24qjfD".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BG2AcN8PvWWibemM3Qr8UUCmiBzhuu4uH63HRXRYPbSw".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "GBWvUvt6AFdZkcL7X3yZFjkjjQjbzZHkaHozdwE9ZbEw".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "86CYtc913SJFS2tuA6CpXEej6ekz8mMdBhtGwvKUUWRB".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6RDS19fkTJ3BtTx78NswVm2VSPRmwM9tEa6kGuadAPKS".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "CbyRjng6xYpTsWwTfpw8fn5bPnWHpJVRqGfWZk64jZV3".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "3VDVh3rHTLkNJp6FVYbuFcaihYBFCQX5VSBZk23ckDGV".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "HSf2eHjgvQLrR9nq1zj7ML8RUfeo3aJaXLLfVQcKAsG4".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "7ffVUjjLPs9fSGKKsSdtCC1sWpcVRHh9CHycqDWr1XYe".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "447YEohqKbW9S2WjeaJtcCHLx8RhsgWRktcpnr5Dsp5A".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BBiUde7cW6KX6A2PMS24EZh963YRk33kaBJ91H7rQaVq".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "FXoUvq28VAuLQ1tgG9rqeho3Z3KU7oAhTgWYTenmbGtP".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "EdyMCC9Zw68g7TFTN3tLgQUpoHdn58ysVWNTLADXNg1u".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "3ojTYpjgS1DkxSPqvgwA99Fjq3WMC4spm5UvVLt5DXfS".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "J2i2UaDmq3qsSB1tYtaczq7rGWiW2CHrfDdVqXrKciiG".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "AaPrBNcCY9saou6anXupJgDhgYKUfGFAoxWjDKZ8UDLb".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "9LNdmia6LRvQjFnaHdbZJ7R5yMSpUmHp4g55SXvq3yEC".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "5W8RNr2kGgmcZFKWR3Aw7JqzM5V4njVRJyV8SL7Bo8U7".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "HqV5MjKvdJrtTyZ7xWELb7DNhvKa6Agq4qMAUYL2S28G".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "7wFskoRiLCAm8EDkKAtBxsTJmCgypFDJZbGcG9d9fX8L".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "5vU9pfyg7KB74AWtYdBZrCkhRZJ6PyFrsBQXEbrBNRMV".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "AqEEj4QEUgmg8wMnhpYQ9YMVrCQUyYXKhFdF4h5627eV".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4RjoApub7yoEXhRbXokjr47BSmuLtqvoPpWh3QwHtdqk".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4HHVXiGLx3cwu7HwiMfx679ics8SbW1CVXkJKmnyYhEk".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "2Y4NWcDk1F3iiNUTiFfRJxKPJBoNGJPY6rZDJN5sAsoP".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "Gd1mEr5feyKuDnLoHkM6GoWs8ESBzAPax3z5yvh7kEmw".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "DxNBWrP6YNcqVDkZsDhT9ZNcBFjYgyAAmfzBBk5w45C1".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "2Ttqwjmw7PJDNuEQ1K1ZbKsfXhcsEQG4AWLtwfWDtxvc".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BK4rQKUpmZfyZickT3ifM9EFsEeqTvMuEA4vvETDzphN".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "8YwntmVByitV71NPUMZDdywwMdGFWEskDusevdt5LQMz".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "DYKhQ9HwJHaMmeUwbbczskJHtZiXGBJzTqkH2HHp98oQ".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "56JDUP9GezS6F5YrdtjMW9vGFta6saJixFpvPxNFrMer".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "CFonCX4XB6JDWYfUkRYYcskCkJSxbtjznGrNUedjxpRS".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "3vcqkxH2WQuage8oTPY1zTirV2kEkqCs7szwBx3dgmV8".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "5nQmG1FHCbww5pBExSHsHp7R6BqNo14s9ZXcQCgKdPML".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "FNsitHktYun9mksohjdDgE5YGaGByDyJJAeFEeJHSHTv".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "AmDTzgxzRNnfNYfmbdoNYcsCL2EYBPA5bwjqLUwyTv4h".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "GoeTbz3kfQrkk6tdNLoEWrHiLn7emiqvwcXYR8TJEHKE".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "BYFQqmQ4mN4z5vNtAt5H7WyVhwj4DY91iLFh25oEtbcY".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "ESgXhKWxbvMMW8XTcCsEJU1geVYqmiRq1UKvFLfrcpMb".into(),
                        name: "Alameda".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "9vm2b5tzEpv6SsNZrAo3ms393xCGKk7Sz1AzxF1qjr6o".into(),
                        name: "Alameda".into(),
                    },
                ],
            },
        }),
        StatusCode::OK,
    ))
}
