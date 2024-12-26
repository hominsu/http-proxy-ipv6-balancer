use crate::skeleton::{ipv6::random_address_in_cidr, Config};

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
    net::{IpAddr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
};
use tokio::net::{TcpSocket, TcpStream};
use tower::{Service, ServiceExt};

#[derive(Clone)]
pub struct V6Balancer {
    pub router: Router,
    pub config: Arc<RwLock<Config>>,
}

impl V6Balancer {
    pub fn new(router: Router, config: Arc<RwLock<Config>>) -> Self {
        Self { router, config }
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
        let c = self.config.read().unwrap();
        let bind_local = random_address_in_cidr(&c.cidr, &c.excludes);

        let router = self.router.clone();
        let req = req.map(Body::new);

        Box::pin(async move {
            match *req.method() {
                Method::CONNECT => proxy(req, bind_local).await,
                _ => router.oneshot(req).await.map_err(|err| match err {}),
            }
        })
    }
}

async fn proxy(req: Request, bind_local: Option<Ipv6Addr>) -> Result<Response, Infallible> {
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
                if let Err(err) = tunnel(upgraded, host_addr, bind_local).await {
                    tracing::warn!("server io error: {:#?}", err);
                };
            }
            Err(err) => tracing::warn!("upgrade error: {:#?}", err),
        }
    });

    Ok(Response::new(Body::empty()))
}

async fn tunnel(
    upgraded: Upgraded,
    remote: String,
    bind_local: Option<Ipv6Addr>,
) -> io::Result<()> {
    let mut server = match connect(&remote, bind_local).await {
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

async fn connect<A: ToSocketAddrs>(addr: A, bind_local: Option<Ipv6Addr>) -> io::Result<TcpStream> {
    let bind_local = match bind_local {
        Some(addr) => addr,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no bind address",
            ))
        }
    };

    let addrs = addr.to_socket_addrs()?;

    let mut last_err = None;

    for addr in addrs {
        let socket = TcpSocket::new_v6()?;
        socket.bind(SocketAddr::new(IpAddr::from(bind_local), 0))?;
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
