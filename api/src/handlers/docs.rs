use log::info;
use warp::{http::Response, Reply};

const HTML: &str = "<html>
<body>
  <redoc spec-url=\"/docs.json\" native-scrollbars></redoc>
  <script src=\"https://public.marinade.finance/redoc.v2.0.0.standalone.js\"></script>
</body>
</html>";

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Docs",
    path = "/docs",
    responses(
        (status = 200)
    )
)]
pub async fn handler() -> Result<impl Reply, warp::Rejection> {
    info!("Serving the docs");
    Ok(Response::builder()
        .header("content-type", "text/html")
        .body(HTML))
}
