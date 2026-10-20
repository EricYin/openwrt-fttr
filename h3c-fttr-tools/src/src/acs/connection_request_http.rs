use crate::provisioning::model::ConnectionRequest;
use serde::Serialize;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Outcome {
    Accepted { http_status: u16 },
    Rejected { http_status: u16 },
    InvalidUrl,
    MissingCredentials,
    DefaultCredentialsUnavailable,
    TransportError,
    ResponseTooLarge,
    Requesting,
    Unauthenticated,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    scheme: String,
    address: SocketAddr,
    suffix: String,
}

impl Target {
    pub(super) fn effective(mut self, config: &ConnectionRequest) -> Self {
        self.address.set_port(config.port.get());
        self
    }
    pub(super) fn parse(url: &str, peer: &str) -> Result<Self, Outcome> {
        if url.len() > 2048
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
            || url.contains(['@', '#', '\\'])
        {
            return Err(Outcome::InvalidUrl);
        }
        let (scheme, rest) = url.split_once("://").ok_or(Outcome::InvalidUrl)?;
        let default_port = match scheme {
            "http" => 80,
            "https" => 443,
            _ => return Err(Outcome::InvalidUrl),
        };
        let boundary = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..boundary];
        let address = match authority.parse::<SocketAddr>() {
            Ok(address) => address,
            Err(_) => {
                let host = authority.strip_prefix('[')
                    .and_then(|h| h.strip_suffix(']')).unwrap_or(authority);
                let ip = host.parse::<IpAddr>().map_err(|_| Outcome::InvalidUrl)?;
                if ip.is_ipv6() && !authority.starts_with('[') {
                    return Err(Outcome::InvalidUrl);
                }
                SocketAddr::new(ip, default_port)
            }
        };
        if address.port() == 0 || peer.parse::<IpAddr>().ok() != Some(address.ip()) {
            return Err(Outcome::InvalidUrl);
        }
        let suffix = &rest[boundary..];
        let slash = if suffix.starts_with('/') { "" } else { "/" };
        Ok(Self { scheme: scheme.into(), address, suffix: format!("{slash}{suffix}") })
    }
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(super) fn request(target: &Target, credentials: &ConnectionRequest) -> Outcome {
    if credentials.defaults {
        return Outcome::DefaultCredentialsUnavailable;
    }
    if !credentials.credentials_available()
        || !credentials.authorization.load(std::sync::atomic::Ordering::Acquire) {
        return Outcome::MissingCredentials;
    }
    let mut address = target.address;
    address.set_port(credentials.port.get());
    let url = format!("{}://{}{}", target.scheme, address, target.suffix);
    let config = format!("url = {}\nuser = {}\n", quoted(&url),
        quoted(&format!("{}:{}", credentials.username, credentials.password)));
    let child = Command::new("curl")
        .args(["-q", "--config", "-", "--digest", "--silent", "--globoff",
            "--proxy", "", "--noproxy", "*", "--proto", "=http,https",
            "--proto-redir", "=http,https",
            "--connect-timeout", "2", "--max-time", "5", "--max-filesize", "8192",
            "--dump-header", "-", "--output", if cfg!(windows) { "NUL" } else { "/dev/null" },
            "--write-out", "\n%{http_code}"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = child else { return Outcome::TransportError; };
    let written = child.stdin.take().is_some_and(|mut input| input.write_all(config.as_bytes()).is_ok());
    let mut bytes = Vec::new();
    let read = written && child.stdout.take().is_some_and(|output|
        output.take(8197).read_to_end(&mut bytes).is_ok());
    if !read || bytes.len() >= 8197 {
        let _ = child.kill();
        let _ = child.wait();
        return if read { Outcome::ResponseTooLarge } else { Outcome::TransportError };
    }
    match child.wait() {
        Ok(exit) if exit.success() => {
            let status = bytes.rsplit(|b| *b == b'\n').next()
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.parse::<u16>().ok());
            match status {
                Some(http_status @ (200 | 204)) => {
                    let headers = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                    let challenged = headers.split("\r\n\r\n").any(|block|
                        block.lines().next().is_some_and(|line| line.starts_with("http/") && line.split_whitespace().nth(1) == Some("401"))
                        && block.lines().any(|line| line.starts_with("www-authenticate: digest ")));
                    if challenged { Outcome::Accepted { http_status } } else { Outcome::Unauthenticated }
                },
                Some(http_status) => Outcome::Rejected { http_status },
                None => Outcome::TransportError,
            }
        }
        Ok(exit) if exit.code() == Some(63) => Outcome::ResponseTooLarge,
        Ok(_) | Err(_) => Outcome::TransportError,
    }
}
