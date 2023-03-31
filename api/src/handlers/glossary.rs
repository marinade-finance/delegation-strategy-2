use crate::context::WrappedContext;
use log::info;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use warp::{http::Response, Reply};

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Glossary",
    path = "/static/glossary.md",
    responses(
        (status = 200)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the glossary");

    let mut file = File::open(&context.read().await.glossary_path)
        .await
        .unwrap();

    let mut contents = vec![];
    file.read_to_end(&mut contents).await.unwrap();

    Ok(Response::builder()
        .header("content-type", "text/markdown")
        .body(contents))
}
