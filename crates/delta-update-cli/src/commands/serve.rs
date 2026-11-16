use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

use crate::cli::ServeArgs;

/// Cap on request headers so a broken client cannot grow the heap unbounded.
const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Drop idle connections instead of parking a thread forever.
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Serve a release directory over HTTP for local demos.
///
/// Loopback only by default. This is a diagnostic/demo server, not a
/// production CDN: it has no TLS, caching, or authentication.
pub fn run(args: &ServeArgs) -> anyhow::Result<()> {
    if !args.dir.is_dir() {
        bail!("--dir is not a directory: {}", args.dir.display());
    }
    let root = std::fs::canonicalize(&args.dir)
        .with_context(|| format!("canonicalize {}", args.dir.display()))?;
    let listener = TcpListener::bind((args.host.as_str(), args.port))
        .with_context(|| format!("bind {}:{}", args.host, args.port))?;
    let address = listener.local_addr()?;
    // A wildcard bind is not a usable URL; loopback is what the client accepts
    // for plain HTTP anyway, so that is what gets printed.
    let host = match args.host.as_str() {
        "0.0.0.0" | "[::]" | "::" => "127.0.0.1",
        other => other,
    };
    println!("serving {} at http://{address}", root.display());
    println!(
        "endpoint: http://{host}:{}/manifests/latest.json",
        address.port()
    );
    println!("客户端本地联调需要 allow_insecure_http: true（只放行回环地址）。");
    if host != args.host {
        crate::output::warn(
            "this server has no TLS; clients only accept plain HTTP on loopback, so use it from this machine or put a TLS proxy in front",
        );
    }
    serve_on(listener, root);
    Ok(())
}

/// Accept connections forever. Split out so tests and the e2e harness can
/// bind port 0 and point a real `Engine` at the result.
pub fn serve_on(listener: TcpListener, root: PathBuf) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle(stream, &root) {
                        eprintln!("serve: {error}");
                    }
                });
            }
            Err(error) => eprintln!("serve: accept: {error}"),
        }
    }
}

struct Request {
    method: String,
    path: String,
    range: Option<String>,
}

fn handle(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    let (status, response) = respond(&request, root);
    println!("{} {} -> {status}", request.method, request.path);
    stream.write_all(&response)?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > MAX_REQUEST_BYTES {
            return Ok(None);
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.split("\r\n");
    let Some(line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    let range = lines.take_while(|line| !line.is_empty()).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("range")
            .then(|| value.trim().to_string())
    });
    Ok(Some(Request {
        method: method.to_string(),
        path: target.split('?').next().unwrap_or("/").to_string(),
        range,
    }))
}

fn respond(request: &Request, root: &Path) -> (u16, Vec<u8>) {
    let head_only = request.method == "HEAD";
    if request.method != "GET" && !head_only {
        return simple(405, "Method Not Allowed", b"method not allowed", false);
    }
    let Some(path) = resolve(root, &request.path) else {
        return simple(404, "Not Found", b"not found", head_only);
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return simple(404, "Not Found", b"not found", head_only);
    };
    match parse_range(request.range.as_deref(), bytes.len() as u64) {
        RangeSpec::Full => full(200, "OK", &path, &bytes, head_only),
        RangeSpec::Partial(start, end) => {
            let slice = &bytes[start as usize..=end as usize];
            let mut response = header(
                206,
                "Partial Content",
                content_type(&path),
                slice.len() as u64,
            );
            response.extend_from_slice(
                format!("Content-Range: bytes {start}-{end}/{}\r\n", bytes.len()).as_bytes(),
            );
            (206, finish(response, slice, head_only))
        }
        RangeSpec::Unsatisfiable => {
            let mut response = header(416, "Range Not Satisfiable", "text/plain", 0);
            response.extend_from_slice(
                format!("Content-Range: bytes */{}\r\n", bytes.len()).as_bytes(),
            );
            (416, finish(response, b"", true))
        }
    }
}

fn full(status: u16, reason: &str, path: &Path, body: &[u8], head_only: bool) -> (u16, Vec<u8>) {
    let response = header(status, reason, content_type(path), body.len() as u64);
    (status, finish(response, body, head_only))
}

fn simple(status: u16, reason: &str, body: &[u8], head_only: bool) -> (u16, Vec<u8>) {
    let response = header(
        status,
        reason,
        "text/plain; charset=utf-8",
        body.len() as u64,
    );
    (status, finish(response, body, head_only))
}

fn header(status: u16, reason: &str, content_type: &str, content_length: u64) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {content_length}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n"
    )
    .into_bytes()
}

fn finish(mut response: Vec<u8>, body: &[u8], head_only: bool) -> Vec<u8> {
    response.extend_from_slice(b"\r\n");
    if !head_only {
        response.extend_from_slice(body);
    }
    response
}

/// Resolve a URL path under `root`, rejecting traversal and absolute paths.
fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let decoded = percent_decode(target)?;
    if decoded.contains('\0') {
        return None;
    }
    let mut path = root.to_path_buf();
    for segment in decoded.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains('\\') || segment.contains(':') {
            return None;
        }
        path.push(segment);
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    (canonical.starts_with(root) && canonical.is_file()).then_some(canonical)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

enum RangeSpec {
    Full,
    Partial(u64, u64),
    Unsatisfiable,
}

/// Single-range only. Multiple ranges are ignored (full response is legal).
fn parse_range(header: Option<&str>, length: u64) -> RangeSpec {
    let Some(header) = header else {
        return RangeSpec::Full;
    };
    let Some(spec) = header.strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    if spec.contains(',') {
        return RangeSpec::Full;
    }
    if length == 0 {
        return RangeSpec::Unsatisfiable;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return RangeSpec::Full;
    };
    let start = start.trim();
    let end = end.trim();
    if start.is_empty() {
        let Some(suffix) = end.parse::<u64>().ok().filter(|value| *value > 0) else {
            return RangeSpec::Unsatisfiable;
        };
        if suffix >= length {
            return RangeSpec::Partial(0, length.saturating_sub(1));
        }
        return RangeSpec::Partial(length - suffix, length - 1);
    }
    let Ok(start) = start.parse::<u64>() else {
        return RangeSpec::Full;
    };
    if start >= length {
        return RangeSpec::Unsatisfiable;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        match end.parse::<u64>() {
            Ok(value) => value.min(length - 1),
            Err(_) => return RangeSpec::Full,
        }
    };
    if end < start {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Partial(start, end)
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => "application/json",
        Some("txt") | Some("minisig") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("exe") => "application/vnd.microsoft.portable-executable",
        _ => "application/octet-stream",
    }
}
