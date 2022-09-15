use env_logger::Env;
use log::{debug, error, info};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(short = "u", long = "url")]
    pub rpc_url: String,

    #[structopt(short = "c", long = "commitment", default_value = "processed")]
    pub commitment: String,
}
