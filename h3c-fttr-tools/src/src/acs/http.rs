//! HTTP framing belongs to Hyper. This module only enforces application limits.
use super::{actor, session::Session, Server};
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Incoming, header, server::conn::http1, service::service_fn, Method};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io;
use std::sync::{atomic::Ordering, Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;

pub(super) const MAX_BODY: usize = 2 * 1024 * 1024;
pub(super) struct Request {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Bytes,
}

pub(super) struct Reply {
    pub code: u16,
    pub body: String,
    pub cookie: Option<String>,
    pub close: bool,
}

impl Reply {
    pub(super) fn empty(code: u16) -> Self {
        Self {
            code,
            body: String::new(),
            cookie: None,
            close: code != 401,
        }
    }

    fn into_http(self) -> hyper::Response<Full<Bytes>> {
        let mut response = hyper::Response::builder()
            .status(self.code)
            .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .header(header::CONTENT_LENGTH, self.body.len())
            .header(
                header::CONNECTION,
                if self.close { "close" } else { "keep-alive" },
            );
        if let Some(cookie) = self.cookie {
            response = response.header(header::SET_COOKIE, cookie);
        }
        if self.code == 401 {
            response = response.header(header::WWW_AUTHENTICATE, "Basic realm=\"FTTR ACS\"");
        }
        response
            .body(Full::new(Bytes::from(self.body)))
            .expect("validated HTTP response")
    }
}

pub(super) fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

async fn request(req: hyper::Request<Incoming>) -> Result<Request, Reply> {
    if req.method() != Method::POST || req.headers().contains_key(header::CONTENT_ENCODING) {
        return Err(Reply::empty(400));
    }
    if req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Reply::empty(413));
    }
    let path = req
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_owned();
    let mut headers = BTreeMap::new();
    for (name, value) in req.headers() {
        let value = value.to_str().map_err(|_| Reply::empty(400))?;
        if headers.insert(name.to_string(), value.to_owned()).is_some() {
            return Err(Reply::empty(400));
        }
    }
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        Limited::new(req.into_body(), MAX_BODY).collect(),
    )
    .await
    .map_err(|_| Reply::empty(408))?
    .map_err(|error| {
        Reply::empty(if error.is::<http_body_util::LengthLimitError>() {
            413
        } else {
            400
        })
    })?
    .to_bytes();
    Ok(Request {
        path,
        headers,
        body,
    })
}

pub(super) async fn serve(server: Arc<Server>, stream: TcpStream) -> io::Result<()> {
    let peer = stream.peer_addr()?.ip().to_string();
    let mut stream = tokio_io_timeout::TimeoutStream::new(stream);
    stream.set_read_timeout(Some(Duration::from_secs(30)));
    stream.set_write_timeout(Some(Duration::from_secs(10)));
    let stream = Box::pin(stream);
    let session = Arc::new(Mutex::new(Session::new(peer)));
    let policy = server.clone();
    let service = service_fn(move |req| {
        let server = policy.clone();
        let session = session.clone();
        async move {
            let reply = match request(req).await {
                Ok(req) => actor::call(move || session.lock().unwrap().handle(&server, req))
                    .await
                    .unwrap_or_else(|_| Reply::empty(503)),
                Err(reply) => reply,
            };
            Ok::<_, Infallible>(reply.into_http())
        }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .max_buf_size(32 * 1024)
        .max_headers(64)
        .title_case_headers(true)
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    loop {
        tokio::select! {
            result = &mut connection => return result.map_err(io::Error::other),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let expired = server.drain_deadline.lock().unwrap()
                    .is_some_and(|at| std::time::Instant::now() >= at);
                if expired { return Ok(()); }
                if server.stopping.load(Ordering::Acquire) {
                    connection.as_mut().graceful_shutdown();
                }
            }
        }
    }
}
