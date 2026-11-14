//! A fake model server: the HTTP surface of the llama.cpp server that this
//! workspace uses (`/health`, `/v1/chat/completions` as an event stream,
//! `/v1/embeddings`), running in the test process on a loopback port. A chat
//! request that turns reasoning on (`chat_template_kwargs.enable_thinking`)
//! is answered with a reasoning delta first.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// One request the fake server received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// Method and path, for example `POST /v1/chat/completions`.
    pub request: String,
    /// Value of the `Authorization` header, if one was sent.
    pub authorization: Option<String>,
    /// The request body.
    pub body: String,
}

struct State {
    hung: AtomicBool,
    hold_stream: AtomicBool,
    require_bearer: AtomicBool,
    release: watch::Sender<u64>,
    requests: Mutex<Vec<Recorded>>,
}

/// Handle to a running fake server. Dropping it stops the server.
pub struct FakeModelServer {
    addr: SocketAddr,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeModelServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (release, _) = watch::channel(0);
        let state = Arc::new(State {
            hung: AtomicBool::new(false),
            hold_stream: AtomicBool::new(false),
            require_bearer: AtomicBool::new(false),
            release,
            requests: Mutex::new(Vec::new()),
        });
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(sock, Arc::clone(&shared)));
            }
        });
        Self { addr, state, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// While hung, the server accepts connections and never answers.
    pub fn set_hung(&self, hung: bool) {
        self.state.hung.store(hung, Ordering::SeqCst);
        if !hung {
            self.state.release.send_modify(|n| *n += 1);
        }
    }

    /// Answer 401 to `/v1/*` requests that carry no bearer token, as the real
    /// server does when started with an API key.
    pub fn require_bearer(&self, required: bool) {
        self.state.require_bearer.store(required, Ordering::SeqCst);
    }

    /// Make the next chat stream stop after its first event until
    /// [`release_stream`](Self::release_stream) is called.
    pub fn hold_stream_after_first_event(&self) {
        self.state.hold_stream.store(true, Ordering::SeqCst);
    }

    pub fn release_stream(&self) {
        self.state.hold_stream.store(false, Ordering::SeqCst);
        self.state.release.send_modify(|n| *n += 1);
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.state.requests.lock().unwrap().clone()
    }

    /// The requests whose method and path equal `request`.
    pub fn requests_to(&self, request: &str) -> Vec<Recorded> {
        let all = self.requests();
        all.into_iter().filter(|r| r.request == request).collect()
    }
}

impl Drop for FakeModelServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const SSE_REASONING: &str = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Thinking.\"},\"finish_reason\":null}]}\n\n";
const SSE_FIRST: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n";
const SSE_REST: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Wait until `flag` is clear.
async fn wait_clear(state: &State, flag: &AtomicBool) {
    let mut released = state.release.subscribe();
    while flag.load(Ordering::SeqCst) {
        if released.changed().await.is_err() {
            return;
        }
    }
}

async fn serve(mut sock: TcpStream, state: Arc<State>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 65536 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    };
    let content_length = header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() - header_end < content_length {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let request_line = head.lines().next().unwrap_or_default();
    let request = request_line
        .rsplit_once(' ')
        .map(|(left, _)| left.to_string())
        .unwrap_or_default();
    let authorization = header("authorization");
    state.requests.lock().unwrap().push(Recorded {
        request: request.clone(),
        authorization: authorization.clone(),
        body: String::from_utf8_lossy(&buf[header_end..]).to_string(),
    });

    if state.hung.load(Ordering::SeqCst) {
        wait_clear(&state, &state.hung).await;
        return;
    }

    let has_bearer = authorization.is_some_and(|v| v.starts_with("Bearer ") && v.len() > 7);
    if request.contains(" /v1/") && state.require_bearer.load(Ordering::SeqCst) && !has_bearer {
        let body = "{\"error\":{\"code\":401,\"message\":\"Invalid API Key\"}}";
        let _ = sock
            .write_all(json_response("401 Unauthorized", body).as_bytes())
            .await;
        return;
    }

    match request.as_str() {
        "GET /health" => {
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        }
        "POST /v1/chat/completions" => {
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                        Connection: close\r\n\r\n";
            let _ = sock.write_all(head.as_bytes()).await;
            // Like the real server: a request that turns reasoning on gets
            // the model's reasoning first, in its own field.
            let body = String::from_utf8_lossy(&buf[header_end..]);
            if body.replace(' ', "").contains("\"enable_thinking\":true") {
                let _ = sock.write_all(SSE_REASONING.as_bytes()).await;
            }
            let _ = sock.write_all(SSE_FIRST.as_bytes()).await;
            let _ = sock.flush().await;
            wait_clear(&state, &state.hold_stream).await;
            let _ = sock.write_all(SSE_REST.as_bytes()).await;
        }
        "POST /v1/embeddings" => {
            let body = "{\"data\":[{\"embedding\":[0.1,0.2]}]}";
            let _ = sock
                .write_all(json_response("200 OK", body).as_bytes())
                .await;
        }
        _ => {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    }
}

fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}
