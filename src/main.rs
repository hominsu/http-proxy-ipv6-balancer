mod skeleton;

use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=trace,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let router = Router::new().route(
        "/",
        get(|| async { (StatusCode::NOT_FOUND, "Not Found").into_response() }),
    );
    let tower_service = skeleton::V6Balancer::new(router);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());

    skeleton::serve(listener, tower_service)
        .with_graceful_shutdown(skeleton::shutdown_signal())
        .await
        .unwrap();
}
