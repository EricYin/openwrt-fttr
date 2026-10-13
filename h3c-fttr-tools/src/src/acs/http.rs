use std::io::{self, BufRead, Read, Write};
use std::collections::BTreeMap;
// Recursive parameter-name responses on HL202/V152 exceed 512 KiB.
// Keep a finite bound, including for chunked bodies.
const MAX_BODY: usize = 2 * 1024 * 1024;
pub struct Request { pub path: String, pub headers: BTreeMap<String, String>, pub body: Vec<u8> }
fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid or oversized HTTP request") }
fn line<R: BufRead>(reader: &mut R) -> io::Result<String> {
    let mut data = Vec::new();
    reader.take(8193).read_until(b'\n', &mut data)?;
    if data.len() > 8192 || !data.ends_with(b"\r\n") { return Err(invalid()); }
    String::from_utf8(data).map_err(|_| invalid())
}
pub fn read<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<Request> {
    let first = line(reader)?;
    let parts: Vec<_> = first.split_whitespace().collect();
    if parts.len() != 3 || parts[0] != "POST" || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") { return Err(invalid()); }
    let mut headers = BTreeMap::new();
    let mut size = 0;
    loop {
        let h = line(reader)?; size += h.len(); if size > 32768 { return Err(invalid()); }
        if h == "\r\n" { break; }
        let (key, value) = h.split_once(':').ok_or_else(invalid)?;
        if headers.insert(key.to_ascii_lowercase(), value.trim().to_owned()).is_some() { return Err(invalid()); }
    }
    if headers.get("expect").is_some_and(|s| s.eq_ignore_ascii_case("100-continue")) { writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?; writer.flush()?; }
    if headers.contains_key("content-encoding") { return Err(invalid()); }
    let mut body = Vec::new();
    if let Some(encoding) = headers.get("transfer-encoding") {
        if !encoding.eq_ignore_ascii_case("chunked") || headers.contains_key("content-length") { return Err(invalid()); }
        loop {
            let chunk = line(reader)?;
            let length = usize::from_str_radix(chunk.trim().split(';').next().unwrap_or(""), 16).map_err(|_| invalid())?;
            if length == 0 {
                let mut trailers = 0;
                loop { let h = line(reader)?; trailers += h.len(); if trailers > 8192 { return Err(invalid()); } if h == "\r\n" { break; } }
                break;
            }
            if length > MAX_BODY - body.len() { return Err(invalid()); }
            let offset = body.len(); body.resize(offset + length, 0); reader.read_exact(&mut body[offset..])?;
            let mut end = [0; 2]; reader.read_exact(&mut end)?; if end != *b"\r\n" { return Err(invalid()); }
        }
    } else {
        let length: usize = headers.get("content-length").map(String::as_str).unwrap_or("0").parse().map_err(|_| invalid())?;
        if length > MAX_BODY { return Err(io::Error::new(io::ErrorKind::InvalidData, format!("HTTP body {length} bytes exceeds limit {MAX_BODY}"))); } body.resize(length, 0); reader.read_exact(&mut body)?;
    }
    Ok(Request { path: parts[1].into(), headers, body })
}
pub fn reply<W: Write>(writer: &mut W, code: u16, body: &str, extra: &str, close: bool) -> io::Result<()> {
    let reason = match code { 200 => "OK", 204 => "No Content", 401 => "Unauthorized", 404 => "Not Found", 503 => "Service Unavailable", _ => "Bad Request" };
    write!(writer, "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nContent-Type: text/xml; charset=utf-8\r\nConnection: {}\r\n{extra}\r\n{body}", body.len(), if close { "close" } else { "keep-alive" })?;
    writer.flush()
}
pub fn basic(user: &str, password: &str) -> String {
    let bytes = format!("{user}:{password}").into_bytes(); let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let mut result = String::from("Basic ");
    for group in bytes.chunks(3) { let n = ((group[0] as u32) << 16) | ((group.get(1).copied().unwrap_or(0) as u32) << 8) | group.get(2).copied().unwrap_or(0) as u32;
        result.push(chars[(n >> 18) as usize] as char); result.push(chars[((n >> 12) & 63) as usize] as char);
        result.push(if group.len() > 1 { chars[((n >> 6) & 63) as usize] as char } else { '=' }); result.push(if group.len() > 2 { chars[(n & 63) as usize] as char } else { '=' }); }
    result
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn bounded_chunked_and_continue() {
        let wire = b"POST /cwmp HTTP/1.1\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        let mut output = Vec::new(); let req = read(&mut &wire[..], &mut output).unwrap(); assert_eq!(req.body, b"abcde"); assert!(output.starts_with(b"HTTP/1.1 100"));
        assert!(read(&mut &b"POST / HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n"[..], &mut output).is_err());
        assert!(read(&mut &b"POST / HTTP/1.1\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n"[..], &mut output).is_err());
        assert_eq!(basic("user", "password"), "Basic dXNlcjpwYXNzd29yZA==");
    }
    #[test] fn large_parameter_response_and_body_limit() {
        let body = "x".repeat(700_000);
        let wire = format!("POST /cwmp HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut output = Vec::new();
        assert_eq!(read(&mut wire.as_bytes(), &mut output).unwrap().body.len(), body.len());
        let oversized = format!("POST /cwmp HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert!(read(&mut oversized.as_bytes(), &mut output).is_err());
        let chunked = format!("POST /cwmp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", MAX_BODY + 1);
        assert!(read(&mut chunked.as_bytes(), &mut output).is_err());
    }
}
