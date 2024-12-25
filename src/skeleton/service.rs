use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use hyper::{body::Incoming, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    future::Future,
    io,
    net::ToSocketAddrs,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::{TcpSocket, TcpStream};
use tower::{Service, ServiceExt};

#[derive(Clone)]
pub struct V6Balancer {
    pub router: Router,
}

impl V6Balancer {
    pub fn new(router: Router) -> Self {
        Self { router }
    }
}

impl Service<Request<Incoming>> for V6Balancer {
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let router = self.router.clone();
        let req = req.map(Body::new);

        Box::pin(async move {
            match *req.method() {
                Method::CONNECT => proxy(req).await,
                _ => router.oneshot(req).await.map_err(|err| match err {}),
            }
        })
    }
}

async fn proxy(req: Request) -> Result<Response, Infallible> {
    tracing::trace!(?req);

    let Some(host_addr) = req.uri().authority().map(|auth| auth.to_string()) else {
        tracing::warn!("CONNECT host is not socket addr: {:#?}", req.uri());
        let resp = (
            StatusCode::BAD_REQUEST,
            "CONNECT must be to a socket address",
        )
            .into_response();
        return Ok(resp);
    };

    tokio::task::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                if let Err(err) = tunnel(upgraded, host_addr).await {
                    tracing::warn!("server io error: {:#?}", err);
                };
            }
            Err(err) => tracing::warn!("upgrade error: {:#?}", err),
        }
    });

    Ok(Response::new(Body::empty()))
}

async fn tunnel(upgraded: Upgraded, remote: String) -> io::Result<()> {
    let mut server = match connect(&remote).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::error!("could not connect to remote via IPv6: {:#?}", err);
            TcpStream::connect(&remote).await? // fallback
        }
    };
    let mut client = TokioIo::new(upgraded);

    let (from_client, from_server) =
        tokio::io::copy_bidirectional(&mut client, &mut server).await?;

    tracing::debug!(
        "client wrote {} bytes and received {} bytes",
        from_client,
        from_server
    );

    Ok(())
}

async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    let addrs = addr.to_socket_addrs()?;

    let mut last_err = None;

    for addr in addrs {
        let socket = TcpSocket::new_v6()?;
        socket.bind("[::1]:0".parse().unwrap())?;
        match socket.connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}
