use log::info;
use warp::{http::Response, Reply};

const HTML: &str = "<!doctype html>
<html>
<head>
  <meta charset=\"UTF-8\"/>
  <title>Marinade's Delegation Strategy API</title>
  <link rel=\"stylesheet\" type=\"text/css\" href=\"https://public.marinade.finance/swagger-ui.v5.31.0.css\">
</head>
<body>
  <div id=\"swagger-ui\"></div>
  <script src=\"https://public.marinade.finance/swagger-ui.v5.31.0.js\"></script>
  <script>
    window.onload = function() {
      SwaggerUIBundle({
        url: '/docs.json',
        dom_id: '#swagger-ui',
        presets: [SwaggerUIBundle.presets.apis, SwaggerUIBundle.SwaggerUIStandalonePreset],
        layout: 'StandaloneLayout'
      });
    };
  </script>
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
