use lazy_static::lazy_static;
use log::error;
use prometheus::{register_int_counter, Encoder, IntCounter, TextEncoder};
use warp::Filter;

lazy_static! {
    pub static ref REQUEST_CLUSTER_STATS: IntCounter = register_int_counter!(
        "request_count_cluster_stats",
        "How many times /cluster-stats endpoint was requested"
    )
    .unwrap();
    pub static ref REQUEST_COUNT_VALIDATORS: IntCounter = register_int_counter!(
        "request_count_validators",
        "How many times /validators endpoint was requested"
    )
    .unwrap();
    pub static ref REQUEST_COUNT_COMMISSIONS: IntCounter = register_int_counter!(
        "request_count_commissions",
        "How many times /commissions endpoint was requested"
    )
    .unwrap();
    pub static ref REQUEST_COUNT_VERSIONS: IntCounter = register_int_counter!(
        "request_count_versions",
        "How many times /versions endpoint was requested"
    )
    .unwrap();
    pub static ref REQUEST_COUNT_UPTIMES: IntCounter = register_int_counter!(
        "request_count_uptimes",
        "How many times /uptimes endpoint was requested"
    )
    .unwrap();
    pub static ref REQUEST_COUNT_MNDE_GAUGES: IntCounter = register_int_counter!(
        "request_count_mnde_gauges",
        "How many times /mnde-gauges endpoint was requested"
    )
    .unwrap();
}

fn collect_metrics() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();

    encoder.encode(&prometheus::gather(), &mut buffer).unwrap();

    String::from_utf8(buffer.clone()).unwrap()
}

pub fn spawn_server() {
    tokio::spawn(async move {
        let route_metrics = warp::path!("metrics")
            .and(warp::path::end())
            .and(warp::get())
            .map(|| collect_metrics());
        warp::serve(route_metrics).run(([0, 0, 0, 0], 9000)).await;
        error!("Metrics server is dead.");
        std::process::exit(1);
    });
}
