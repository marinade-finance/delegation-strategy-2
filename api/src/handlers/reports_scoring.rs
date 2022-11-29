use crate::context::WrappedContext;
use log::info;
use serde_json::json;
use warp::{http::StatusCode, reply, Reply};

pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring report");
    Ok(warp::reply::with_status(
        reply::json(&json!({
            "reports": {
                "370": [{
                    "created_at": "2022-11-29T17:20:01.123456Z",
                    "link": "https://..../....zip",
                    "md": "Download data for the report: ```bash\nsh -c \"echo Downloaded...\"```\nGenerate the report: ```bash\nsh -c \"echo Generated...\"```\n"
                },{
                    "created_at": "2022-11-28T13:46:02.217154Z",
                    "link": "https://..../....zip",
                    "md": "Download data for the report: ```bash\nsh -c \"echo Downloaded...\"```\nGenerate the report: ```bash\nsh -c \"echo Generated...\"```\n"
                }]
            }
        })),
        StatusCode::OK,
    ))
}
