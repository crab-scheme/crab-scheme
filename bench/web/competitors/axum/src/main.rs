use axum::{routing::get, Router};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let app = Router::new().route(
        "/plain",
        get(|| async {
            (
                [("content-type", "text/plain")],
                "Hello, World!",
            )
        }),
    );
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("axum on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
