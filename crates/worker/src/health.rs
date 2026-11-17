// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two-phase health probe: TCP connect (the port check) then an HTTP GET (the
//! protocol probe). It speaks HTTP itself and reads nothing from the
//! environment, so it cannot be redirected by proxy settings.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// TCP connect + `GET {path}`; healthy iff a 200 status line arrives. The
/// caller bounds the wait. `bearer` is sent as an `Authorization` header.
pub async fn probe_http(addr: SocketAddr, path: &str, bearer: Option<&str>) -> bool {
    let Ok(mut stream) = TcpStream::connect(addr).await else {
        return false;
    };
    let auth = match bearer {
        Some(key) => format!("Authorization: Bearer {key}\r\n"),
        None => String::new(),
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            Err(_) => return false,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn probes_a_healthy_server_true_and_a_dead_port_false() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let n = sock.read(&mut buf).await.unwrap();
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        assert!(probe_http(addr, "/health", Some("s3cret")).await);
        let request = seen.await.unwrap();
        assert!(request.starts_with("GET /health HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer s3cret\r\n"));

        // A port with nothing listening: allocate one, then close it.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        assert!(!probe_http(dead_addr, "/health", None).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_is_still_loading_is_not_healthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        assert!(!probe_http(addr, "/health", None).await);
    }
}
