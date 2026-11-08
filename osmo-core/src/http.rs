//! Client for the camera's lighttpd `/v2` file API and a resumable download loop. Ported from Osmosis
//! `HttpClient.kt` / `MediaDownloader.downloadOne`. The camera closes long transfers every ~10 s and
//! answers 404/500 when merely busy, so a download is a loop of ranged requests that only gives up
//! after several attempts in a row move no bytes.
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Total attempts per file, productive or not: a runaway guard.
pub const MAX_RESUMES: u32 = 20;
/// Consecutive zero-byte attempts before a transfer counts as stalled.
pub const MAX_BARREN: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// The whole (remaining) body arrived and the connection closed cleanly.
    Done,
    /// The transfer started but ended early; whatever arrived is valid.
    Interrupted,
    /// A ranged request got a 200: the body starts at byte 0 and must not be appended.
    RangeIgnored,
    /// A non-2xx status.
    Failed,
    /// 416 on a resume: the file is already at least as long as the camera's, so there is nothing to fetch.
    RangeNotSatisfiable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Saved,
    AlreadyComplete,
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("download stalled at {at} bytes; run it again to resume")]
    Stalled { at: u64 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_resumes: u32,
    pub max_barren: u32,
    /// Pause before a resume, multiplied by the barren-attempt count (the camera needs a moment to
    /// let go of the previous transfer; reconnecting instantly moves zero bytes).
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_resumes: MAX_RESUMES,
            max_barren: MAX_BARREN,
            backoff: Duration::from_millis(750),
        }
    }
}

type Logger = Box<dyn FnMut(&str) + Send>;

pub struct HttpClient {
    base: String,
    pub policy: RetryPolicy,
    logger: RefCell<Option<Logger>>,
    cancel: Option<Arc<AtomicBool>>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .build()
}

impl HttpClient {
    /// Client for the camera at `ip` (port 80).
    pub fn new(ip: &str) -> Self {
        Self::with_base(&format!("http://{ip}"))
    }

    pub fn with_base(base: &str) -> Self {
        HttpClient {
            base: base.trim_end_matches('/').to_string(),
            policy: RetryPolicy::default(),
            logger: RefCell::new(None),
            cancel: None,
        }
    }

    /// Stop transfers when `flag` becomes true: checked once per received chunk and before each resume.
    pub fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        self.cancel = Some(flag);
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }

    /// Receive one line per download attempt (offset, outcome, bytes on disk, backoff).
    pub fn set_logger(&self, f: impl FnMut(&str) + Send + 'static) {
        *self.logger.borrow_mut() = Some(Box::new(f));
    }

    fn log(&self, msg: String) {
        if let Some(l) = self.logger.borrow_mut().as_mut() {
            l(&msg);
        }
    }

    fn read_limited(&self, req: ureq::Request, limit: u64) -> Option<Vec<u8>> {
        let resp = req.call().ok()?;
        let mut out = Vec::new();
        resp.into_reader().take(limit).read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// A small file whole (a thumbnail), at most 16 MiB; `None` on any non-2xx or error.
    pub fn get_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let req = agent()
            .get(&self.url(path))
            .set("Accept", "*/*")
            .set("Connection", "close");
        self.read_limited(req, 16 * 1024 * 1024)
    }

    /// An inclusive byte range. Reads at most `end - start + 1` bytes even if the server ignores the range and
    /// answers 200 with the whole file.
    pub fn get_range(&self, path: &str, start: u64, end: u64) -> Option<Vec<u8>> {
        let req = agent()
            .get(&self.url(path))
            .set("Accept", "*/*")
            .set("Connection", "close")
            .set("Range", &format!("bytes={start}-{end}"));
        self.read_limited(req, end.checked_sub(start)? + 1)
    }

    /// An inclusive byte range plus the file's total length from `Content-Range` (`bytes a-b/total`), if the
    /// server sent one. Same limits as `get_range`.
    pub fn get_range_total(
        &self,
        path: &str,
        start: u64,
        end: u64,
    ) -> Option<(Vec<u8>, Option<u64>)> {
        let resp = agent()
            .get(&self.url(path))
            .set("Accept", "*/*")
            .set("Connection", "close")
            .set("Range", &format!("bytes={start}-{end}"))
            .call()
            .ok()?;
        let total = resp
            .header("Content-Range")
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.trim().parse().ok());
        let mut out = Vec::new();
        resp.into_reader()
            .take(end.checked_sub(start)? + 1)
            .read_to_end(&mut out)
            .ok()?;
        Some((out, total))
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Content-Length of a file, `None` if missing or on any error.
    pub fn head(&self, path: &str) -> Option<u64> {
        let r = agent()
            .head(&self.url(path))
            .set("Accept", "*/*")
            .call()
            .ok()?;
        r.header("Content-Length")?.parse().ok()
    }

    /// Stream `path` into `out` starting at `range_start` (0 = whole file); `on_progress` gets the
    /// running total including `range_start`. A new connection per call, `Connection: close`, so a
    /// cut transfer's socket is never reused.
    pub fn download(
        &self,
        path: &str,
        out: &mut dyn Write,
        range_start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Fetch {
        let mut req = agent()
            .get(&self.url(path))
            .set("Accept", "*/*")
            .set("Connection", "close");
        if range_start > 0 {
            req = req.set("Range", &format!("bytes={range_start}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(416, _)) if range_start > 0 => {
                return Fetch::RangeNotSatisfiable
            }
            Err(ureq::Error::Status(..)) => return Fetch::Failed,
            Err(_) => return Fetch::Interrupted,
        };
        if range_start > 0 && resp.status() != 206 {
            return Fetch::RangeIgnored;
        }
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; 65536];
        let mut total = range_start;
        loop {
            if self.cancelled() {
                let _ = out.flush();
                return Fetch::Interrupted;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out.write_all(&buf[..n]).is_err() {
                        return Fetch::Interrupted;
                    }
                    total += n as u64;
                    on_progress(total);
                }
                Err(_) => {
                    let _ = out.flush();
                    return Fetch::Interrupted;
                }
            }
        }
        let _ = out.flush();
        Fetch::Done
    }

    /// Download into `dest`, resuming from whatever is already there. 404/500 are treated as the
    /// camera being busy, not as gone. Writing goes straight to `dest`, so an interrupted run
    /// leaves a valid prefix that the next run continues from.
    pub fn download_resumable(
        &self,
        path: &str,
        dest: &Path,
        remote_size: Option<u64>,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<Outcome, DownloadError> {
        let remote = remote_size.unwrap_or(0);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dest)?;
        let mut offset = file.metadata()?.len();
        if remote > 0 && offset >= remote {
            return Ok(Outcome::AlreadyComplete);
        }
        let (mut attempt, mut barren) = (0u32, 0u32);
        loop {
            file.seek(SeekFrom::Start(offset))?;
            let outcome = self.download(path, &mut file, offset, on_progress);
            let _ = file.flush();
            let mut after = file.metadata()?.len();
            if outcome == Fetch::RangeIgnored {
                // The body would have been appended at the resume offset: start the file over instead.
                file.set_len(0)?;
                after = 0;
            }
            self.log(format!(
                "download: attempt {} from {offset}: {outcome:?}, file now {after} bytes",
                attempt + 1
            ));
            if outcome == Fetch::RangeNotSatisfiable {
                return Ok(Outcome::AlreadyComplete);
            }
            // With a known size only a full-length file is a saved file: a connection that closes cleanly
            // but early is a cut, not an end. Without one, a clean close is all we have to go on.
            let complete = if remote > 0 {
                after >= remote
            } else {
                outcome == Fetch::Done
            };
            if complete {
                return Ok(Outcome::Saved);
            }
            if self.cancelled() {
                return Err(DownloadError::Cancelled);
            }
            barren = if after > offset { 0 } else { barren + 1 };
            if barren >= self.policy.max_barren || attempt >= self.policy.max_resumes {
                return Err(DownloadError::Stalled { at: after });
            }
            attempt += 1;
            let pause = self.policy.backoff * barren.max(1);
            if barren > 0 {
                self.log(format!(
                    "download: no progress {barren}/{}, retrying in {} ms",
                    self.policy.max_barren,
                    pause.as_millis()
                ));
            }
            std::thread::sleep(pause);
            offset = after;
        }
    }
}
