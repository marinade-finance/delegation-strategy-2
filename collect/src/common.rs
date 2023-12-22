use std::{thread, time::Duration};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(short = "u", long = "url")]
    pub rpc_url: String,

    #[structopt(short = "c", long = "commitment", default_value = "finalized")]
    pub commitment: String,
}

pub fn retry_blocking<F, T, E, ErrorCallback>(
    make_call: F,
    backoff_strategy: impl Iterator<Item = Duration>,
    on_error: ErrorCallback
) -> Result<T, E>
where
    F: Fn() -> Result<T, E>,
    E: std::fmt::Debug,
    ErrorCallback: Fn(E, usize, Duration) -> ()
{
    for (attempt_index, backoff) in backoff_strategy.enumerate() {
        match make_call() {
            Ok(result) => return Ok(result),
            Err(err) => {
                on_error(err, attempt_index + 1, backoff);
                thread::sleep(backoff);
            }
        }
    }
    make_call()
}

pub struct QuadraticBackoffStrategy;

impl QuadraticBackoffStrategy {
    pub fn new(max_attempts: usize) -> impl Iterator<Item = Duration> {
        (1..=max_attempts)
            .into_iter()
            .map(|attempt| Duration::from_secs((attempt as u64).pow(2)))
    }
}
