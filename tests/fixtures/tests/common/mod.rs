//! Minimal raw-TCP HTTP/1.1 test client (no HTTP client crate is declared for the fixture).

#![allow(dead_code)]

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A parsed response; `complete` is false when the body was cut short.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub complete: bool,
}

impl Response {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One keep-alive connection.
pub struct Client {
    reader: BufReader<TcpStream>,
}

/// Strips the scheme from a base URL, leaving `host:port`.
pub fn host_port(base: &str) -> &str {
    base.strip_prefix("http://").unwrap_or(base)
}

/// One-shot request on a fresh connection.
pub async fn get(base: &str, target: &str, headers: &[(&str, &str)]) -> Response {
    Client::connect(base)
        .await
        .send("GET", target, headers)
        .await
}

impl Client {
    pub async fn connect(base: &str) -> Client {
        let stream = TcpStream::connect(host_port(base)).await.expect("connect");
        Client {
            reader: BufReader::new(stream),
        }
    }

    pub async fn send(&mut self, method: &str, target: &str, headers: &[(&str, &str)]) -> Response {
        let mut req = format!("{method} {target} HTTP/1.1\r\nHost: fixture\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        self.reader
            .get_mut()
            .write_all(req.as_bytes())
            .await
            .expect("write");
        self.read_response(method == "HEAD").await
    }

    async fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end().to_owned()),
        }
    }

    async fn read_response(&mut self, head_only: bool) -> Response {
        let status_line = self.read_line().await.expect("status line");
        let status: u16 = status_line
            .split(' ')
            .nth(1)
            .expect("status")
            .parse()
            .expect("code");
        let mut headers = Vec::new();
        while let Some(line) = self.read_line().await {
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').expect("header colon");
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
        let mut resp = Response {
            status,
            headers,
            body: Vec::new(),
            complete: true,
        };
        if head_only || status == 204 || status == 304 {
            return resp;
        }
        let chunked = resp
            .header("transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
        let len = resp
            .header("content-length")
            .and_then(|v| v.parse::<usize>().ok());
        if chunked {
            self.read_chunked(&mut resp).await;
        } else if let Some(n) = len {
            self.read_exact_lenient(&mut resp, n).await;
        } else {
            let _ = self.reader.read_to_end(&mut resp.body).await;
        }
        resp
    }

    /// Reads until `resp.body` holds `target_len` bytes; marks the response incomplete on EOF/error.
    async fn read_exact_lenient(&mut self, resp: &mut Response, target_len: usize) {
        let mut buf = vec![0u8; 64 * 1024];
        while resp.body.len() < target_len {
            let want = (target_len - resp.body.len()).min(buf.len());
            match self.reader.read(&mut buf[..want]).await {
                Ok(0) | Err(_) => {
                    resp.complete = false;
                    return;
                }
                Ok(got) => resp.body.extend_from_slice(&buf[..got]),
            }
        }
    }

    async fn read_chunked(&mut self, resp: &mut Response) {
        loop {
            let Some(size_line) = self.read_line().await else {
                resp.complete = false;
                return;
            };
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
                .unwrap_or(0);
            if size == 0 {
                let _ = self.read_line().await;
                return;
            }
            let before = resp.body.len();
            self.read_exact_lenient(resp, before + size).await;
            if !resp.complete || self.read_line().await.is_none() {
                resp.complete = false;
                return;
            }
        }
    }
}
