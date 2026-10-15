//! Integration tests: a real server on a loopback port, real clients.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;

use bytes::Bytes;
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tinymold_server::{Config, Error, Server, TlsFiles};
use tinymold_store::Sha256;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

struct Running {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<tinymold_server::Result<()>>,
    _dir: tempfile::TempDir,
}

impl Running {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap().unwrap();
    }
}

fn config(dir: &tempfile::TempDir) -> Config {
    let mut config = Config::new(dir.path().join("store"));
    config.listen = "127.0.0.1:0".parse().unwrap();
    config
}

fn start() -> Running {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::bind(&config(&dir)).unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        addr,
        stop: Some(stop),
        task,
        _dir: dir,
    }
}

async fn http1(
    addr: SocketAddr,
    method: Method,
    path: &str,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(http::header::HOST, addr.to_string())
        .body(Full::new(Bytes::from(body)))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    (
        status,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_http1() {
    let server = start();

    let (status, body) = http1(server.addr, Method::GET, "/healthz", Vec::new()).await;

    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"ok\n".as_slice())
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_plaintext_http2() {
    let server = start();
    let stream = TcpStream::connect(server.addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(connection);

    let request = Request::builder()
        .uri(format!("http://{}/healthz", server.addr))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), http::Version::HTTP_2);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_blob_streams_both_ways() {
    let server = start();
    let blob: Vec<u8> = (0..3 << 20).map(|i: u32| (i % 253) as u8).collect();
    let path = format!("/cas/{}", Sha256::of_bytes(&blob));

    let (status, _) = http1(server.addr, Method::PUT, &path, blob.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = http1(server.addr, Method::GET, &path, Vec::new()).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body == blob, "downloaded blob differs");
    server.stop().await;
}

#[test]
fn an_open_server_refuses_a_non_loopback_address() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&dir);
    config.listen = "0.0.0.0:0".parse().unwrap();

    assert!(matches!(
        Server::bind(&config),
        Err(Error::OpenNonLoopback(_))
    ));
}

#[test]
fn a_non_loopback_address_is_allowed_with_a_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, "ci *:write\n").unwrap();
    let mut config = config(&dir);
    config.listen = "0.0.0.0:0".parse().unwrap();
    config.tokens = Some(tokens);

    assert!(Server::bind(&config).is_ok());
}

#[test]
fn bad_configuration_files_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("garbage.pem");
    std::fs::write(&garbage, "not pem at all\n").unwrap();

    let mut missing_tokens = config(&dir);
    missing_tokens.tokens = Some(dir.path().join("absent"));
    assert!(matches!(
        Server::bind(&missing_tokens),
        Err(Error::ReadFile { .. })
    ));

    let mut bad_tokens = config(&dir);
    std::fs::write(dir.path().join("bad"), "token turbo:own\n").unwrap();
    bad_tokens.tokens = Some(dir.path().join("bad"));
    assert!(matches!(
        Server::bind(&bad_tokens),
        Err(Error::TokenFile { line: 1, .. })
    ));

    let mut bad_cert = config(&dir);
    bad_cert.tls = Some(TlsFiles {
        cert: garbage.clone(),
        key: garbage.clone(),
    });
    assert!(matches!(Server::bind(&bad_cert), Err(Error::TlsPem { .. })));

    let mut missing_cert = config(&dir);
    missing_cert.tls = Some(TlsFiles {
        cert: dir.path().join("absent.pem"),
        key: garbage,
    });
    assert!(matches!(
        Server::bind(&missing_cert),
        Err(Error::ReadFile { .. })
    ));
}

#[test]
fn an_address_in_use_is_a_bind_error() {
    let dir = tempfile::tempdir().unwrap();
    let first = Server::bind(&config(&dir)).unwrap();
    let mut taken = config(&dir);
    taken.listen = first.local_addr();

    assert!(matches!(Server::bind(&taken), Err(Error::Bind { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_server_trims_the_store_to_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&dir);
    config.max_size = Some(1024);
    let server = Server::bind(&config).unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    // Three 1 KiB action results against a 1 KiB budget.
    for name in [b"one".as_slice(), b"two", b"three"] {
        let path = format!("/ac/{}", Sha256::of_bytes(name));
        let mut body = vec![0_u8; 1024];
        body[0] = name[0];
        body[1] = name[1];
        assert_eq!(http1(addr, Method::PUT, &path, body).await.0, StatusCode::OK);
    }
    let _ = stop.send(());
    task.await.unwrap().unwrap();

    let store = tinymold_store::Store::open(&config.store).unwrap();
    let bytes: u64 = store.cas().objects().unwrap().iter().map(|object| object.size).sum();
    assert!(bytes <= 1024, "{bytes} bytes kept, over the 1024-byte budget");
}
