mod skeleton;

use argh::FromArgs;
use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
use std::future::IntoFuture;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(FromArgs)]
#[argh(description = "http-proxy-ipv6-balancer")]
struct Args {
    #[argh(
        option,
        default = "String::from(\"configs\")",
        description = "config path, eg: --conf ./configs"
    )]
    conf: String,
}

#[tokio::main]
async fn main() {
    let args: Args = argh::from_env();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=trace,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let manager = skeleton::manager(args.conf.as_str()).with_watcher(skeleton::shutdown_signal());
    let config = manager.config();
    let manager_fut = manager.into_future();

    let router = Router::new().route(
        "/",
        get(|| async { (StatusCode::NOT_FOUND, "Not Found").into_response() }),
    );
    let tower_service = skeleton::V6Balancer::new(router);

    let addr = config.read().unwrap().addr.clone();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    tracing::debug!("listening on {}", listener.local_addr().unwrap());

    let serve_fut = skeleton::serve(listener, tower_service)
        .with_graceful_shutdown(skeleton::shutdown_signal())
        .into_future();

    let _ = tokio::join!(serve_fut, manager_fut);
}
