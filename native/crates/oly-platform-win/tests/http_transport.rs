#![cfg(windows)]
use oly_engine::net::{HttpError, HttpRequest, Transport};
use oly_platform_win::http::NativeHttp;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};
fn server(
    response: Vec<u8>,
    delay: Duration,
) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/test?q=one%20two", socket.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = socket.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + len {
                    break;
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
        thread::sleep(delay);
        let _ = stream.write_all(&response);
    });
    (address, rx, handle)
}
#[test]
fn sends_post_body_and_preserves_url_headers_status_and_content_type() {
    let (url, request, handle) = server(b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}".to_vec(), Duration::ZERO);
    let response = NativeHttp
        .send(&HttpRequest::post(url, b"hello".to_vec(), 3000).header("X-Test", "yes"))
        .unwrap();
    assert_eq!(response.status, 201);
    assert_eq!(response.content_type, "application/json");
    assert_eq!(response.text(), "{\"ok\":true}");
    let sent = request.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(sent.starts_with("POST /test?q=one%20two HTTP/1.1"));
    assert!(sent.to_ascii_lowercase().contains("x-test: yes"));
    assert!(sent.ends_with("hello"));
    handle.join().unwrap();
}
#[test]
fn returns_http_error_status_without_converting_it_to_a_transport_failure() {
    let (url, _, handle) = server(
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        Duration::ZERO,
    );
    assert_eq!(
        NativeHttp
            .send(&HttpRequest::get(url, 3000))
            .unwrap()
            .status,
        503
    );
    handle.join().unwrap();
}
#[test]
fn times_out_a_stalled_response() {
    let (url, _, handle) = server(Vec::new(), Duration::from_millis(2000));
    let started = std::time::Instant::now();
    let result = NativeHttp.send(&HttpRequest::get(url, 30));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "total deadline was not enforced"
    );
    assert!(matches!(result, Err(HttpError::Timeout)), "{result:?}");
    handle.join().unwrap();
}
#[test]
fn caps_decoded_response_size_and_rejects_non_http_urls() {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        8 * 1024 * 1024 + 1
    )
    .into_bytes();
    response.resize(response.len() + 8 * 1024 * 1024 + 1, b'x');
    let (url, _, handle) = server(response, Duration::ZERO);
    assert!(
        matches!(NativeHttp.send(&HttpRequest::get(url, 3000)), Err(HttpError::Transport(s)) if s.contains("8 MiB"))
    );
    handle.join().unwrap();
    assert!(
        NativeHttp
            .send(&HttpRequest::get("file:///c:/test", 10))
            .is_err()
    );
}
