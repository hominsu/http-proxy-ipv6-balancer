use axum::{extract::Request, response::Response};
use futures_util::{pin_mut, FutureExt};
use hyper::{body::Incoming, server::conn::http1};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use std::{
    convert::Infallible,
    future::{poll_fn, Future, IntoFuture},
    io,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tower::Service;

pub fn serve<S>(tcp_listener: TcpListener, service: S) -> Serve<S>
where
    S: Service<Request<Incoming>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send,
{
    Serve {
        tcp_listener,
        service,
        tcp_nodelay: None,
    }
}

pub struct Serve<S> {
    tcp_listener: TcpListener,
    service: S,
    tcp_nodelay: Option<bool>,
}

impl<S> Serve<S> {
    pub fn with_graceful_shutdown<F>(self, signal: F) -> WithGracefulShutdown<S, F>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        WithGracefulShutdown {
            tcp_listener: self.tcp_listener,
            service: self.service,
            signal,
            tcp_nodelay: self.tcp_nodelay,
        }
    }

    #[allow(dead_code)]
    pub fn tcp_nodelay(self, nodelay: bool) -> Self {
        Self {
            tcp_nodelay: Some(nodelay),
            ..self
        }
    }

    #[allow(dead_code)]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp_listener.local_addr()
    }
}

impl<S> IntoFuture for Serve<S>
where
    S: Service<Request<Incoming>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send,
{
    type Output = io::Result<()>;
    type IntoFuture = private::ServeFuture;

    fn into_future(self) -> Self::IntoFuture {
        private::ServeFuture(Box::pin(async move {
            let Self {
                tcp_listener,
                mut service,
                tcp_nodelay,
            } = self;

            loop {
                let (tcp_stream, remote_addr) = match tcp_accept(&tcp_listener).await {
                    Some(conn) => conn,
                    None => continue,
                };

                if let Some(nodelay) = tcp_nodelay {
                    if let Err(err) = tcp_stream.set_nodelay(nodelay) {
                        tracing::trace!(
                            "failed to set TCP_NODELAY on incoming connection: {:#?}",
                            err
                        );
                    }
                }

                let tcp_stream = TokioIo::new(tcp_stream);

                tracing::trace!("connection {remote_addr} accepted");

                poll_fn(|cx| service.poll_ready(cx))
                    .await
                    .unwrap_or_else(|err| match err {});

                let hyper_service = TowerToHyperService::new(service.clone());

                tokio::task::spawn(async move {
                    match http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(tcp_stream, hyper_service)
                        .with_upgrades()
                        .await
                    {
                        Ok(()) => {}
                        Err(err) => tracing::trace!("failed to serve connection: {:#?}", err),
                    }
                });
            }
        }))
    }
}

pub struct WithGracefulShutdown<S, F> {
    tcp_listener: TcpListener,
    service: S,
    signal: F,
    tcp_nodelay: Option<bool>,
}

impl<S, F> WithGracefulShutdown<S, F> {
    #[allow(dead_code)]
    pub fn tcp_nodelay(self, nodelay: bool) -> Self {
        Self {
            tcp_nodelay: Some(nodelay),
            ..self
        }
    }

    #[allow(dead_code)]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp_listener.local_addr()
    }
}

impl<S, F> IntoFuture for WithGracefulShutdown<S, F>
where
    S: Service<Request<Incoming>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send,
    F: Future<Output = ()> + Send + 'static,
{
    type Output = io::Result<()>;
    type IntoFuture = private::ServeFuture;

    fn into_future(self) -> Self::IntoFuture {
        let Self {
            tcp_listener,
            mut service,
            signal,
            tcp_nodelay,
        } = self;

        let (signal_tx, signal_rx) = watch::channel(());
        let signal_tx = Arc::new(signal_tx);
        tokio::spawn(async move {
            signal.await;
            tracing::trace!("received graceful shutdown signal. Telling tasks to shutdown");
            drop(signal_rx);
        });

        let (close_tx, close_rx) = watch::channel(());

        private::ServeFuture(Box::pin(async move {
            loop {
                let (tcp_stream, remote_addr) = tokio::select! {
                    conn = tcp_accept(&tcp_listener) => {
                        match conn {
                            Some(conn) => conn,
                            None => continue,
                        }
                    }
                    _ = signal_tx.closed() => {
                        tracing::trace!("signal received, not accepting new connections");
                        break;
                    }
                };

                if let Some(nodelay) = tcp_nodelay {
                    if let Err(err) = tcp_stream.set_nodelay(nodelay) {
                        tracing::trace!(
                            "failed to set TCP_NODELAY on incoming connection: {:#?}",
                            err
                        );
                    }
                }

                let tcp_stream = TokioIo::new(tcp_stream);

                tracing::trace!("connection {remote_addr} accepted");

                poll_fn(|cx| service.poll_ready(cx))
                    .await
                    .unwrap_or_else(|err| match err {});

                let hyper_service = TowerToHyperService::new(service.clone());

                let signal_tx = Arc::clone(&signal_tx);

                let close_rx = close_rx.clone();

                tokio::task::spawn(async move {
                    let conn = http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(tcp_stream, hyper_service)
                        .with_upgrades();
                    pin_mut!(conn);

                    let signal_closed = signal_tx.closed().fuse();
                    pin_mut!(signal_closed);

                    loop {
                        tokio::select! {
                            result = conn.as_mut() => {
                                if let Err(err) = result {
                                    tracing::trace!("Failed to serve connection: {:#?}", err);
                                }
                                break;
                            }
                            _ = &mut signal_closed => {
                                tracing::trace!("signal received in task, starting graceful shutdown");
                                conn.as_mut().graceful_shutdown();
                            }
                        }
                    }

                    tracing::trace!("connection {remote_addr} closed");

                    drop(close_rx);
                });
            }

            drop(close_rx);
            drop(tcp_listener);

            tracing::trace!(
                "waiting for {} task(s) to finish",
                close_tx.receiver_count()
            );
            close_tx.closed().await;

            Ok(())
        }))
    }
}

fn is_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

async fn tcp_accept(listener: &TcpListener) -> Option<(TcpStream, SocketAddr)> {
    match listener.accept().await {
        Ok(conn) => Some(conn),
        Err(err) => {
            if is_connection_error(&err) {
                return None;
            }

            tracing::error!("accept error: {:#?}", err);
            tokio::time::sleep(Duration::from_secs(1)).await;
            None
        }
    }
}

mod private {
    use std::{
        future::Future,
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    pub struct ServeFuture(pub(super) futures_util::future::BoxFuture<'static, io::Result<()>>);

    impl Future for ServeFuture {
        type Output = io::Result<()>;

        #[inline]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.as_mut().poll(cx)
        }
    }
}
