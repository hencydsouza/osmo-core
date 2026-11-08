//! A `RangeSource` over the camera's HTTP file API, so `mp4::trim` can cut a clip on the camera
//! without downloading all of it.
use osmo_core::http::HttpClient;
use osmo_core::mp4::{Mp4Error, RangeSource};
use std::time::Duration;

/// Attempts per request: the camera drops long connections, so one cut is expected, not fatal.
const ATTEMPTS: u32 = 3;

pub struct HttpRangeSource {
    http: HttpClient,
    url_path: String,
    total: Option<u64>,
    /// Pause between attempts after a cut connection.
    pub backoff: Duration,
}

impl HttpRangeSource {
    pub fn new(http: HttpClient, url_path: &str) -> Self {
        HttpRangeSource {
            http,
            url_path: url_path.to_string(),
            total: None,
            backoff: Duration::from_millis(400),
        }
    }

    fn pause(&self, attempt: u32) {
        if attempt + 1 < ATTEMPTS && !self.backoff.is_zero() {
            std::thread::sleep(self.backoff);
        }
    }
}

impl RangeSource for HttpRangeSource {
    fn len(&mut self) -> Result<u64, Mp4Error> {
        if let Some(n) = self.total {
            return Ok(n);
        }
        for attempt in 0..ATTEMPTS {
            let n = self
                .http
                .get_range_total(&self.url_path, 0, 0)
                .and_then(|(_, total)| total)
                .or_else(|| self.http.head(&self.url_path));
            if let Some(n) = n {
                self.total = Some(n);
                return Ok(n);
            }
            self.pause(attempt);
        }
        Err(Mp4Error::Io(
            "could not read the file's length from the camera".to_string(),
        ))
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Mp4Error> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(len as u64 - 1)
            .ok_or(Mp4Error::Truncated)?;
        for attempt in 0..ATTEMPTS {
            if let Some((bytes, total)) = self.http.get_range_total(&self.url_path, offset, end) {
                if bytes.len() == len {
                    return Ok(bytes);
                }
                // A short body at the end of the file is the file ending, not a cut.
                if let Some(t) = total.or(self.total) {
                    if end >= t {
                        return Err(Mp4Error::Truncated);
                    }
                }
            }
            self.pause(attempt);
        }
        Err(Mp4Error::Io(format!(
            "the camera cut the connection {ATTEMPTS} times reading {len} bytes at {offset}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy)]
    enum Mode {
        Ok,
        /// Advertise the whole range but send half of it, then close.
        Cut,
    }

    struct Srv {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    /// Serves `body` with `Range` support. Each connection takes the next scripted mode; after the
    /// script runs out, `rest` applies.
    fn serve(body: Vec<u8>, script: Vec<Mode>, rest: Mode) -> Srv {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let q = Arc::new(Mutex::new(VecDeque::from(script)));
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = requests.clone();
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
                let range = text
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("Range: bytes=")
                            .or(l.strip_prefix("range: bytes="))
                    })
                    .unwrap_or("0-")
                    .to_string();
                log.lock().unwrap().push(range.clone());
                let (a, z) = range.split_once('-').unwrap();
                let a: usize = a.parse().unwrap();
                let z: usize = z
                    .parse::<usize>()
                    .map_or(body.len() - 1, |z| z.min(body.len() - 1));
                let part = &body[a..=z];
                let mode = q.lock().unwrap().pop_front().unwrap_or(rest);
                let sent = match mode {
                    Mode::Ok => part.len(),
                    Mode::Cut => part.len() / 2,
                };
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{z}/{}\r\nConnection: close\r\n\r\n",
                    part.len(),
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&part[..sent]);
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        });
        Srv { base, requests }
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn source(srv: &Srv) -> HttpRangeSource {
        let mut s =
            HttpRangeSource::new(HttpClient::with_base(&srv.base), "/v2?storage=0&path=X.MP4");
        s.backoff = Duration::ZERO;
        s
    }

    #[test]
    fn http_range_reads_exact_ranges_and_reports_length_from_content_range() {
        let body = data(10_000);
        let srv = serve(body.clone(), vec![], Mode::Ok);
        let mut src = source(&srv);
        assert_eq!(src.len().unwrap(), 10_000);
        assert_eq!(src.read_at(100, 50).unwrap(), body[100..150]);
        assert_eq!(src.read_at(9_990, 10).unwrap(), body[9_990..]);
        assert!(src.read_at(0, 0).unwrap().is_empty());
        let seen = srv.requests.lock().unwrap().clone();
        assert_eq!(seen, ["0-0", "100-149", "9990-9999"]);
        // The length is cached.
        src.len().unwrap();
        assert_eq!(srv.requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn http_range_reads_past_the_end_are_truncated_not_retried() {
        let srv = serve(data(1_000), vec![], Mode::Ok);
        let mut src = source(&srv);
        assert!(matches!(src.read_at(990, 50), Err(Mp4Error::Truncated)));
        assert_eq!(srv.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn http_range_retries_a_cut_connection_then_succeeds() {
        let body = data(8_000);
        let srv = serve(body.clone(), vec![Mode::Cut, Mode::Cut], Mode::Ok);
        let mut src = source(&srv);
        assert_eq!(src.read_at(1_000, 4_000).unwrap(), body[1_000..5_000]);
        assert_eq!(srv.requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn http_range_fails_cleanly_after_three_cuts() {
        let srv = serve(data(8_000), vec![], Mode::Cut);
        let mut src = source(&srv);
        let err = src.read_at(1_000, 4_000).unwrap_err();
        assert!(
            matches!(err, Mp4Error::Io(ref m) if m.contains("3 times")),
            "{err:?}"
        );
        assert_eq!(srv.requests.lock().unwrap().len(), 3);
        // The length request retries the same way and also fails cleanly.
        let srv = serve(data(8_000), vec![], Mode::Cut);
        let mut src = source(&srv);
        assert!(src.read_at(0, 10).is_err());
    }
}
