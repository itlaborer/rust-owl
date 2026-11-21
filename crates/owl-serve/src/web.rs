//! Web 控制台静态页面（自包含单页，xterm.js 走 CDN）。

use axum::response::Html;

const INDEX_HTML: &str = include_str!("../assets/index.html");

pub async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}
