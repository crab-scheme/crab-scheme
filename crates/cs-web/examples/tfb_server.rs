//! Standalone cs-web server matching the competitors'
//! "Hello, World!" `/plain` shape. Used as the cs-web side of
//! the head-to-head benchmark against axum / Go / Node / Python.

use std::net::SocketAddr;

use cs_web::handler::service_fn;
use cs_web::{response, Router, ServerConfig, StatusCode};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let addr_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:0".to_string());
    let addr: SocketAddr = addr_arg.parse().expect("addr");

    let svc = Router::new()
        .get(
            "/plain",
            service_fn(|_| async {
                let mut r = response(StatusCode::OK, "Hello, World!");
                r.headers_mut()
                    .insert("content-type", http::HeaderValue::from_static("text/plain"));
                r
            }),
        )
        .into_service();

    let cfg = ServerConfig {
        addr,
        request_timeout: None,
    };
    let (listener, bound) = cs_web::bind(&cfg).await.expect("bind");
    println!("cs-web on {}", bound);
    cs_web::serve::<futures_util::future::Pending<()>>(listener, svc, None)
        .await
        .expect("serve");
}
