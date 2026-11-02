//! Resumable download against a scripted HTTP server (one scripted reply per connection).
//! Fixture-tested only: this is not the camera's lighttpd.
use osmo_core::http::{DownloadError, HttpClient, Outcome, MAX_BARREN};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Reply {
    status: u16,
    body: Vec<u8>,
    /// Content-Length to advertise; larger than `body` simulates a cut connection.
    declared: Option<usize>,
    content_range: Option<String>,
}

impl Reply {
    fn ok(body: &[u8]) -> Self {
        Reply {
            status: 200,
            body: body.to_vec(),
            declared: None,
            content_range: None,
        }
    }
    fn cut(body: &[u8], declared: usize) -> Self {
        Reply {
            status: 200,
            body: body.to_vec(),
            declared: Some(declared),
            content_range: None,
        }
    }
    fn partial(body: &[u8], from: usize, total: usize) -> Self {
        Reply {
            status: 206,
            body: body.to_vec(),
            declared: None,
            content_range: Some(format!("bytes {from}-{}/{total}", total - 1)),
        }
    }
    fn status(code: u16) -> Self {
        Reply {
            status: code,
            body: vec![],
            declared: Some(0),
            content_range: None,
        }
    }
}

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    range: Option<String>,
}

struct Srv {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
}

fn serve(replies: Vec<Reply>) -> Srv {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let q = Arc::new(Mutex::new(VecDeque::from(replies)));
    let reqs: Arc<Mutex<Vec<Req>>> = Arc::default();
    let rl = reqs.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if s.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                head.push(b[0]);
            }
            let text = String::from_utf8_lossy(&head).to_string();
            let mut first = text.lines().next().unwrap_or("").split(' ');
            let (method, path) = (
                first.next().unwrap_or("").to_string(),
                first.next().unwrap_or("").to_string(),
            );
            let range = text
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Range: ")
                        .or_else(|| l.strip_prefix("range: "))
                })
                .map(str::to_string);
            rl.lock().unwrap().push(Req {
                method: method.clone(),
                path,
                range,
            });
            let r = q
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Reply::status(500));
            let mut out = format!(
                "HTTP/1.1 {} X\r\nContent-Length: {}\r\n",
                r.status,
                r.declared.unwrap_or(r.body.len())
            );
            if let Some(cr) = &r.content_range {
                out += &format!("Content-Range: {cr}\r\n");
            }
            out += "Connection: close\r\n\r\n";
            let _ = s.write_all(out.as_bytes());
            if method != "HEAD" {
                let _ = s.write_all(&r.body);
            }
            let _ = s.flush();
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    });
    Srv { base, reqs }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn client(srv: &Srv) -> HttpClient {
    let mut c = HttpClient::with_base(&srv.base);
    c.policy.backoff = Duration::ZERO;
    c
}

const P: &str = "/v2?storage=0&path=DCIM/DJI_001/DJI_X.MP4";

#[test]
fn downloads_whole_file() {
    let body = data(100_000);
    let srv = serve(vec![Reply::ok(&body)]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.mp4");
    let mut last = 0;
    let out = client(&srv)
        .download_resumable(P, &dest, Some(100_000), &mut |n| last = n)
        .unwrap();
    assert_eq!(out, Outcome::Saved);
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(last, 100_000);
    assert_eq!(srv.reqs.lock().unwrap()[0].path, P, "path sent verbatim");
}

#[test]
fn resumes_after_a_cut_connection() {
    let body = data(20_000);
    let srv = serve(vec![
        Reply::cut(&body[..8000], 20_000),
        Reply::partial(&body[8000..], 8000, 20_000),
    ]);
    let dest = tempfile::tempdir().unwrap().keep().join("a.mp4");
    let out = client(&srv)
        .download_resumable(P, &dest, Some(20_000), &mut |_| {})
        .unwrap();
    assert_eq!(out, Outcome::Saved);
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let reqs = srv.reqs.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].range, None);
    assert_eq!(reqs[1].range.as_deref(), Some("bytes=8000-"));
}

#[test]
fn range_ignored_restarts_from_zero_instead_of_splicing() {
    let body = data(20_000);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.mp4");
    std::fs::write(&dest, &body[..5000]).unwrap();
    let srv = serve(vec![Reply::ok(&body), Reply::ok(&body)]);
    let out = client(&srv)
        .download_resumable(P, &dest, Some(20_000), &mut |_| {})
        .unwrap();
    assert_eq!(out, Outcome::Saved);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "must not be partial bytes + a full body"
    );
    let reqs = srv.reqs.lock().unwrap();
    assert_eq!(reqs[0].range.as_deref(), Some("bytes=5000-"));
    assert_eq!(reqs[1].range, None);
}

#[test]
fn transient_404_then_500_then_ok() {
    let body = data(3000);
    let srv = serve(vec![
        Reply::status(404),
        Reply::status(500),
        Reply::ok(&body),
    ]);
    let dest = tempfile::tempdir().unwrap().keep().join("a.mp4");
    assert_eq!(
        client(&srv)
            .download_resumable(P, &dest, Some(3000), &mut |_| {})
            .unwrap(),
        Outcome::Saved
    );
    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[test]
fn gives_up_after_consecutive_barren_attempts() {
    let srv = serve(vec![]);
    let dest = tempfile::tempdir().unwrap().keep().join("a.mp4");
    let err = client(&srv)
        .download_resumable(P, &dest, Some(3000), &mut |_| {})
        .unwrap_err();
    assert!(matches!(err, DownloadError::Stalled { .. }), "{err:?}");
    assert_eq!(srv.reqs.lock().unwrap().len(), MAX_BARREN as usize);
}

#[test]
fn existing_partial_file_is_resumed() {
    let body = data(20_000);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.mp4");
    std::fs::write(&dest, &body[..6000]).unwrap();
    let srv = serve(vec![Reply::partial(&body[6000..], 6000, 20_000)]);
    assert_eq!(
        client(&srv)
            .download_resumable(P, &dest, Some(20_000), &mut |_| {})
            .unwrap(),
        Outcome::Saved
    );
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(
        srv.reqs.lock().unwrap()[0].range.as_deref(),
        Some("bytes=6000-")
    );
}

#[test]
fn existing_complete_file_reports_already_complete_without_a_request() {
    let body = data(20_000);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.mp4");
    std::fs::write(&dest, &body).unwrap();
    let srv = serve(vec![]);
    assert_eq!(
        client(&srv)
            .download_resumable(P, &dest, Some(20_000), &mut |_| {})
            .unwrap(),
        Outcome::AlreadyComplete
    );
    assert!(srv.reqs.lock().unwrap().is_empty());
}

#[test]
fn head_returns_content_length_or_none() {
    let srv = serve(vec![
        Reply {
            status: 200,
            body: vec![],
            declared: Some(1234),
            content_range: None,
        },
        Reply::status(404),
    ]);
    let c = client(&srv);
    assert_eq!(c.head(P), Some(1234));
    assert_eq!(c.head(P), None);
    assert_eq!(srv.reqs.lock().unwrap()[0].method, "HEAD");
}

#[test]
fn unreachable_camera_fails_in_bounded_time() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    let mut c = HttpClient::with_base(&base);
    c.policy.backoff = Duration::ZERO;
    let dest = tempfile::tempdir().unwrap().keep().join("a.mp4");
    let t = std::time::Instant::now();
    assert!(matches!(
        c.download_resumable(P, &dest, None, &mut |_| {}),
        Err(DownloadError::Stalled { .. })
    ));
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn cut_connection_is_detected_even_when_the_remote_size_is_unknown() {
    let body = data(20_000);
    let srv = serve(vec![
        Reply::cut(&body[..8000], 20_000),
        Reply::partial(&body[8000..], 8000, 20_000),
    ]);
    let dest = tempfile::tempdir().unwrap().keep().join("a.mp4");
    let out = client(&srv)
        .download_resumable(P, &dest, None, &mut |_| {})
        .unwrap();
    assert_eq!(out, Outcome::Saved);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "a short body must not be taken as complete"
    );
}
