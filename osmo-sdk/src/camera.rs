//! The high-level camera API: every method opens its own datalink session, does one job and closes it.
//! Orchestration moved here from the CLI without changing behaviour; messages meant for a person go to
//! the notice callback instead of stderr.
use crate::http_range::HttpRangeSource;
use crate::listing::{
    dest_filename, download_error_message, file_from_path, fill_unknown_sizes, filter_files,
    find_file, format_status, known_size, plan_deletions, proxy_job, resolve_requested,
    sidecars_to_fetch, thumb_sources, truncation_warning, validate_get_path, LsFilter, ThumbSource,
};
use osmo_core::datalink::{
    has_media_records, tcp_poke_to, CameraSession, Datagram, SessionError, Timing, UdpPeer,
    WriteError,
};
use osmo_core::embedded_jpeg::{embedded_jpeg, image_kind, HEAD_BYTES};
use osmo_core::http::{DownloadError, HttpClient, Outcome};
use osmo_core::media::{url_path, CameraFile};
pub use osmo_core::mp4::TrimOutcome;
use osmo_core::mp4::{self, Mp4Error};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkError {
    /// The datalink to the camera failed or the camera refused a write.
    Session(String),
    NotFound(String),
    /// A delete plan was refused; one line per refused path. Nothing was sent.
    Refused(Vec<String>),
    Io(String),
    Mp4(String),
    Cancelled,
}

impl std::fmt::Display for SdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SdkError::Session(m) | SdkError::NotFound(m) | SdkError::Io(m) | SdkError::Mp4(m) => {
                f.write_str(m)
            }
            SdkError::Refused(lines) => write!(f, "refused: {}", lines.join("; ")),
            SdkError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for SdkError {}

impl From<SessionError> for SdkError {
    fn from(e: SessionError) -> Self {
        SdkError::Session(e.to_string())
    }
}

impl From<WriteError> for SdkError {
    fn from(e: WriteError) -> Self {
        SdkError::Session(e.to_string())
    }
}

impl From<Mp4Error> for SdkError {
    fn from(e: Mp4Error) -> Self {
        match e {
            Mp4Error::Cancelled => SdkError::Cancelled,
            other => SdkError::Mp4(other.to_string()),
        }
    }
}

impl From<DownloadError> for SdkError {
    fn from(e: DownloadError) -> Self {
        match e {
            DownloadError::Cancelled => SdkError::Cancelled,
            other => SdkError::Io(download_error_message(&other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub dest: PathBuf,
    /// The file was already fully on disk, so nothing was fetched.
    pub already_complete: bool,
    pub bytes: u64,
}

/// What `list_report` returns beyond the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListReport {
    pub files: Vec<CameraFile>,
    /// Battery, free space and so on, one line; empty when the camera reported none.
    pub status: String,
    /// The raw manifest of the last page, when it held media records.
    pub manifest: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub bytes: Vec<u8>,
    /// The camera path the image came from.
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GetOptions {
    /// Skip the library lookup and assume every file is on this card.
    pub storage: Option<u8>,
    /// Keep the datalink alive during the transfer.
    pub keepalive: bool,
    pub sidecars: bool,
    /// Fetch the low-res proxy of each video instead.
    pub proxy: bool,
}

/// Progress of `download_many`, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetEvent {
    /// About to look this many files up in the camera's library.
    Lookup(usize),
    NotFound(String),
    NoProxy(String),
    Start {
        index: usize,
        total: usize,
        path: String,
    },
    Progress {
        done: u64,
        size: Option<u64>,
    },
    /// The main file finished transferring (its sidecars, if any, come next).
    FileDone {
        dest: PathBuf,
        size: Option<u64>,
    },
    NoSidecar,
    SidecarProgress {
        done: u64,
        size: u64,
    },
    SidecarSaved(PathBuf),
    SidecarFailed {
        name: String,
        message: String,
    },
    Saved(PathBuf),
    AlreadyComplete(PathBuf),
    Failed {
        path: String,
        message: String,
    },
    /// A stalled transfer ended the run; this many files were not tried.
    Stopping {
        left: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetReport {
    pub saved: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total: usize,
    pub not_found: usize,
}

/// Where the write counter lives between runs. The camera ignores a write whose counter it has already
/// seen, so a process that exits and starts again must continue the sequence.
pub trait WriteCounter: Send + Sync {
    fn next(&self) -> u32;
    fn save(&self, next: u32);
}

struct MemoryCounter(AtomicU32);

impl WriteCounter for MemoryCounter {
    fn next(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
    fn save(&self, next: u32) {
        self.0.store(next, Ordering::Relaxed);
    }
}

type Session = CameraSession<Box<dyn Datagram + Send>>;
type Opener = dyn Fn() -> Result<Session, SessionError> + Send + Sync;
type Callback = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Camera {
    ip: String,
    port: u16,
    cancel: Arc<AtomicBool>,
    open: Arc<Opener>,
    http_base: String,
    counter: Arc<dyn WriteCounter>,
    logger: Option<Callback>,
    notice: Option<Callback>,
    http_backoff: Option<Duration>,
    /// A datalink session kept alive while a screen browses thumbnails (the camera serves HTTP only while one is open).
    browse: Mutex<Option<Held>>,
}

/// Keeps the datalink alive on a background thread while a long HTTP transfer runs: the camera puts its
/// Wi-Fi to sleep when idle.
struct Held {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Session>,
    /// Latest status the camera pushed (battery, storage), refreshed every keepalive tick.
    status: Arc<Mutex<osmo_core::status::Status>>,
}

impl Held {
    fn start(mut s: Session) -> Held {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let status = Arc::new(Mutex::new(s.status().clone()));
        let shared = status.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                s.keepalive_tick();
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = s.status().clone();
            }
            s
        });
        Held {
            stop,
            handle,
            status,
        }
    }

    fn release(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = self.handle.join() {
            s.close();
        }
    }
}

impl Camera {
    /// A camera at `ip`; `port` is the datalink port (9004 with the TCP poke, or 10004 without it).
    pub fn new(ip: &str, port: u16) -> Camera {
        let addr = ip.to_string();
        let open: Arc<Opener> = Arc::new(move || {
            if port == 9004 {
                if let Ok(a) = format!("{addr}:7001").parse() {
                    tcp_poke_to(a, Duration::from_millis(400));
                }
            }
            let udp = UdpPeer::connect(&addr, port)?;
            Ok(CameraSession::new(
                Box::new(udp) as Box<dyn Datagram + Send>,
                format!("{addr}:{port}"),
                Timing::default(),
            ))
        });
        Camera {
            ip: ip.to_string(),
            port,
            cancel: Arc::new(AtomicBool::new(false)),
            open,
            http_base: format!("http://{ip}"),
            counter: Arc::new(MemoryCounter(AtomicU32::new(1))),
            logger: None,
            notice: None,
            http_backoff: None,
            browse: Mutex::new(None),
        }
    }

    /// Test seam: sessions come from `open` (a fake peer) and HTTP goes to `http_base`.
    #[cfg(test)]
    fn with_parts(open: Arc<Opener>, http_base: &str) -> Camera {
        let mut c = Camera::new("192.168.2.1", 9004);
        c.open = open;
        c.http_base = http_base.to_string();
        c.http_backoff = Some(Duration::ZERO);
        c
    }

    pub fn ip(&self) -> &str {
        &self.ip
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// One line per protocol event (the CLI's `-v`).
    pub fn set_logger(&mut self, f: impl Fn(&str) + Send + Sync + 'static) {
        self.logger = Some(Arc::new(f));
    }

    /// Warnings a person should see (playback not confirmed, a cut page, a stale favourite).
    pub fn set_notice(&mut self, f: impl Fn(&str) + Send + Sync + 'static) {
        self.notice = Some(Arc::new(f));
    }

    pub fn set_write_counter(&mut self, c: Arc<dyn WriteCounter>) {
        self.counter = c;
    }

    /// Stop the running operation at its next chunk or step. Safe from any thread.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn start(&self) {
        self.cancel.store(false, Ordering::Relaxed);
    }

    fn check_cancel(&self) -> Result<(), SdkError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(SdkError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn say(&self, msg: &str) {
        if let Some(n) = &self.notice {
            n(msg);
        }
    }

    fn http(&self) -> HttpClient {
        let mut h = HttpClient::with_base(&self.http_base);
        h.set_cancel(self.cancel.clone());
        if let Some(b) = self.http_backoff {
            h.policy.backoff = b;
        }
        h
    }

    fn session(&self) -> Result<Session, SdkError> {
        // Any operation that needs its own session ends the browse session first (the camera allows one).
        self.browse_end();
        self.open_session()
    }

    fn open_session(&self) -> Result<Session, SdkError> {
        self.check_cancel()?;
        let mut s = (self.open)()?;
        if let Some(l) = self.logger.clone() {
            s.set_logger(move |line| l(line));
        }
        Ok(s)
    }

    fn warn_if_unconfirmed(&self, s: &Session) {
        if !s.diagnostics().playback_confirmed {
            self.say("the camera did not report playback mode, so the list may be empty or incomplete; run it again");
        }
    }

    fn lookup(&self, s: &mut Session, path: &str) -> Result<CameraFile, SdkError> {
        let found = s.find_file(path)?;
        self.warn_if_unconfirmed(s);
        found.ok_or_else(|| {
            SdkError::NotFound(format!(
                "not found anywhere in the camera's library: {path} (check it against a full listing)"
            ))
        })
    }

    fn list_pages(&self, s: &mut Session, all: bool) -> Result<Vec<CameraFile>, SdkError> {
        let mut files = s.list_newest()?;
        if all {
            // A page cap guards against a camera that keeps answering with files we have not seen.
            for _ in 0..500 {
                self.check_cancel()?;
                if !s.more_available() {
                    break;
                }
                let next = s.list_next_page()?;
                if next.is_empty() {
                    break;
                }
                files.extend(next);
            }
        }
        Ok(files)
    }

    pub fn list(
        &self,
        filter: &LsFilter,
        all: bool,
        page_size: Option<u8>,
    ) -> Result<Vec<CameraFile>, SdkError> {
        self.list_report(filter, all, page_size).map(|r| r.files)
    }

    /// `list` plus what the CLI also shows: the camera's status line and the raw manifest of the last page.
    pub fn list_report(
        &self,
        filter: &LsFilter,
        all: bool,
        page_size: Option<u8>,
    ) -> Result<ListReport, SdkError> {
        self.start();
        let mut s = self.session()?;
        s.set_highlights_only(filter.highlights);
        if let Some(n) = page_size {
            s.set_page_size(n);
        }
        let files = self.list_pages(&mut s, all);
        self.warn_if_unconfirmed(&s);
        if let Some(w) = truncation_warning(s.diagnostics().truncated) {
            self.say(&w);
        }
        let status = format_status(s.status());
        let manifest = has_media_records(s.last_blob()).then(|| s.last_blob().to_vec());
        s.close();
        let mut files = files?;
        let http = self.http();
        fill_unknown_sizes(&mut files, &mut |x| http.head(&x.url_path()));
        Ok(ListReport {
            files: filter_files(&files, filter.kind, filter.starred),
            status,
            manifest,
        })
    }

    /// The image for `path`: its screennail or thumbnail, else the JPEG embedded in a still.
    /// Open (once) and keep a datalink session so many `thumbnail_fast` calls can run at the same time.
    /// Any other operation on this camera ends it automatically; call this again afterwards.
    pub fn browse_start(&self) -> Result<(), SdkError> {
        let mut g = self.browse.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let s = self.open_session()?;
            *g = Some(Held::start(s));
        }
        Ok(())
    }

    /// The camera's latest pushed status (battery, storage), from the browse session; `None` when no browse
    /// session is open yet.
    pub fn status(&self) -> Option<osmo_core::status::Status> {
        let g = self.browse.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .map(|h| h.status.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// A video's highlight marks, in milliseconds from the start. Empty when it has none.
    pub fn highlights(&self, path: &str) -> Result<Vec<u32>, SdkError> {
        self.start();
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        let mut s = self.session()?;
        let file = self.lookup(&mut s, path)?;
        s.highlights(&file)
            .map_err(|e| SdkError::Session(e.to_string()))
    }

    /// End the browse session, if any.
    pub fn browse_end(&self) {
        let held = self.browse.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = held {
            h.release();
        }
    }

    /// A thumbnail for a file whose store is known (from a listing): no listing lookup and no session of its
    /// own, so it is safe to run many at once after `browse_start`. Does not reset the cancel flag.
    pub fn thumbnail_fast(&self, path: &str, storage: u8) -> Result<Vec<u8>, SdkError> {
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        let file = file_from_path(storage, path);
        let http = self.http();
        for src in thumb_sources(&file) {
            self.check_cancel()?;
            let bytes = match &src {
                ThumbSource::Whole(p) => http.get_bytes(p),
                ThumbSource::EmbeddedHead(p) => http
                    .get_range(p, 0, HEAD_BYTES as u64 - 1)
                    .and_then(|head| embedded_jpeg(&head)),
            };
            if let Some(b) = bytes.filter(|b| image_kind(b).is_some()) {
                return Ok(b);
            }
        }
        Err(SdkError::NotFound(format!(
            "no thumbnail available for {path}"
        )))
    }

    pub fn thumbnail(&self, path: &str) -> Result<Vec<u8>, SdkError> {
        self.thumbnail_from(path, None).map(|t| t.bytes)
    }

    /// Like `thumbnail`, also saying where the image came from. With `storage` the camera's library is not
    /// consulted and the file is assumed to be on that card.
    pub fn thumbnail_from(&self, path: &str, storage: Option<u8>) -> Result<Thumbnail, SdkError> {
        self.start();
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        let (file, session) = match storage {
            Some(st) => (file_from_path(st, path), None),
            None => {
                let mut s = self.session()?;
                (self.lookup(&mut s, path)?, Some(s))
            }
        };
        let http = self.http();
        let mut tried = Vec::new();
        let mut found = None;
        for src in thumb_sources(&file) {
            if let Err(e) = self.check_cancel() {
                if let Some(mut s) = session {
                    s.close();
                }
                return Err(e);
            }
            let (label, bytes) = match &src {
                ThumbSource::Whole(p) => (p.clone(), http.get_bytes(p)),
                ThumbSource::EmbeddedHead(p) => (
                    format!("{p} (embedded EXIF)"),
                    http.get_range(p, 0, HEAD_BYTES as u64 - 1)
                        .and_then(|head| embedded_jpeg(&head)),
                ),
            };
            match bytes.filter(|b| image_kind(b).is_some()) {
                Some(bytes) => {
                    found = Some(Thumbnail { bytes, label });
                    break;
                }
                None => tried.push(label),
            }
        }
        if let Some(mut s) = session {
            s.close();
        }
        found.ok_or_else(|| {
            SdkError::NotFound(format!(
                "no thumbnail available for {path} (tried: {})",
                tried.join(", ")
            ))
        })
    }

    /// Download `path` into `dest_dir`, resuming a partial file. `progress` gets (bytes so far, total or 0).
    pub fn download(
        &self,
        path: &str,
        dest_dir: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<DownloadOutcome, SdkError> {
        self.start();
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        let http = self.http();
        let mut s = self.session()?;
        let file = self.lookup(&mut s, path)?;
        let size = known_size(&file, || http.head(&file.url_path()));
        std::fs::create_dir_all(dest_dir).map_err(|e| SdkError::Io(e.to_string()))?;
        let held = Held::start(s);
        let dest = dest_dir.join(dest_filename(path));
        let result =
            http.download_resumable(&url_path(file.storage, path), &dest, size, &mut |n| {
                progress(n, size.unwrap_or(0))
            });
        // A sidecar is a convenience: its failure never fails the main file.
        if result.is_ok() {
            for (sidecar, n) in sidecars_to_fetch(&file, &mut |u| http.head(u)) {
                let to = dest_dir.join(dest_filename(&sidecar));
                let r = http.download_resumable(
                    &url_path(file.storage, &sidecar),
                    &to,
                    Some(n),
                    &mut |_| {},
                );
                if let Err(e) = r {
                    self.say(&format!(
                        "sidecar {sidecar}: {}",
                        download_error_message(&e)
                    ));
                }
            }
        }
        held.release();
        let outcome = result?;
        Ok(DownloadOutcome {
            bytes: std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
            already_complete: outcome == Outcome::AlreadyComplete,
            dest,
        })
    }

    /// Download several files with one library lookup: resumable, optionally with sidecars or low-res proxies.
    /// Per-file failures are counted in the report; the run ends early when the camera's Wi-Fi is gone.
    pub fn download_many(
        &self,
        paths: &[String],
        dest_dir: &Path,
        opts: GetOptions,
        on: &mut dyn FnMut(GetEvent),
    ) -> Result<GetReport, SdkError> {
        self.start();
        let paths: Vec<&str> = paths
            .iter()
            .map(|p| validate_get_path(p).map_err(SdkError::NotFound))
            .collect::<Result<_, _>>()?;
        let http = self.http();
        // (path, store, known size)
        let mut jobs: Vec<(String, u8, Option<u64>)> = Vec::new();
        let mut not_found = 0usize;
        let session = match opts.storage {
            Some(st) => {
                for p in &paths {
                    jobs.push((p.to_string(), st, http.head(&url_path(st, p))));
                }
                None
            }
            None => {
                let mut sess = self.session()?;
                on(GetEvent::Lookup(paths.len()));
                let files = if let [p] = paths[..] {
                    sess.find_file(p)?.into_iter().collect()
                } else {
                    self.list_pages(&mut sess, true)?
                };
                self.warn_if_unconfirmed(&sess);
                let requested: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
                let (found, missing) = resolve_requested(&files, &requested);
                for m in &missing {
                    on(GetEvent::NotFound(m.clone()));
                }
                not_found = missing.len();
                for f in found {
                    jobs.push((
                        f.path.clone(),
                        f.storage,
                        known_size(f, || http.head(&f.url_path())),
                    ));
                }
                Some(sess)
            }
        };
        if opts.proxy {
            let mut swapped = Vec::new();
            for (path, store, _) in jobs {
                match proxy_job(&path, store, |u| http.head(u)) {
                    Ok(job) => swapped.push(job),
                    Err(why) => {
                        on(GetEvent::NoProxy(why));
                        not_found += 1;
                    }
                }
            }
            jobs = swapped;
        }
        if jobs.is_empty() {
            if let Some(mut s) = session {
                s.close();
            }
            return Err(SdkError::NotFound("nothing to download".to_string()));
        }
        std::fs::create_dir_all(dest_dir).map_err(|e| SdkError::Io(e.to_string()))?;
        let held = match session {
            Some(s) if opts.keepalive => Some(Held::start(s)),
            Some(mut s) => {
                s.close();
                None
            }
            None => None,
        };

        let total = jobs.len();
        let mut report = GetReport {
            total,
            not_found,
            failed: not_found,
            ..GetReport::default()
        };
        for (n, (path, store, size)) in jobs.iter().enumerate() {
            on(GetEvent::Start {
                index: n,
                total,
                path: path.clone(),
            });
            let dest = dest_dir.join(dest_filename(path));
            let result = http.download_resumable(&url_path(*store, path), &dest, *size, &mut |d| {
                on(GetEvent::Progress {
                    done: d,
                    size: *size,
                })
            });
            match result {
                Ok(outcome) => {
                    on(GetEvent::FileDone {
                        dest: dest.clone(),
                        size: *size,
                    });
                    if opts.sidecars {
                        let file = file_from_path(*store, path);
                        let found = sidecars_to_fetch(&file, &mut |u| http.head(u));
                        if found.is_empty() {
                            on(GetEvent::NoSidecar);
                        }
                        for (sidecar, len) in found {
                            let to = dest_dir.join(dest_filename(&sidecar));
                            let r = http.download_resumable(
                                &url_path(*store, &sidecar),
                                &to,
                                Some(len),
                                &mut |d| on(GetEvent::SidecarProgress { done: d, size: len }),
                            );
                            match r {
                                Ok(_) => on(GetEvent::SidecarSaved(to)),
                                Err(e) => on(GetEvent::SidecarFailed {
                                    name: sidecar,
                                    message: download_error_message(&e),
                                }),
                            }
                        }
                    }
                    if outcome == Outcome::AlreadyComplete {
                        report.skipped += 1;
                        on(GetEvent::AlreadyComplete(dest));
                    } else {
                        report.saved += 1;
                        on(GetEvent::Saved(dest));
                    }
                }
                Err(e) => {
                    report.failed += 1;
                    on(GetEvent::Failed {
                        path: path.clone(),
                        message: download_error_message(&e),
                    });
                    let stop =
                        matches!(e, DownloadError::Stalled { .. } | DownloadError::Cancelled);
                    if stop {
                        let left = total - n - 1;
                        if left > 0 && !matches!(e, DownloadError::Cancelled) {
                            on(GetEvent::Stopping { left });
                            report.failed += left;
                        }
                        break;
                    }
                }
            }
        }
        if let Some(h) = held {
            h.release();
        }
        Ok(report)
    }

    pub fn set_favourite(&self, path: &str, on: bool) -> Result<(), SdkError> {
        self.start();
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        {
            let mut s = self.session()?;
            let file = self.lookup(&mut s, path)?;
            s.set_write_counter_start(self.counter.next());
            let result = s.set_favourite(&file, on);
            // Advance the persisted counter whatever happened: the camera may have seen this one.
            self.counter.save(s.next_write_counter());
            result?;
        }
        // Confirm on a fresh session: a second handshake on the same transport is not attempted.
        let mut check = self.session()?;
        match check.find_file(path)? {
            Some(f) if f.starred == on => {}
            Some(_) => {
                self.say("a fresh listing does not show the change yet; list again to check")
            }
            None => self.say("could not find the file again to confirm"),
        }
        Ok(())
    }

    /// Walk older pages only until every path is found: a long walk would age the session past the point
    /// where the camera still honours writes (the delete is then refused, never sent).
    fn plan_on(&self, s: &mut Session, paths: &[String]) -> Result<Vec<CameraFile>, SdkError> {
        if paths.len() > 20 {
            return Err(SdkError::Refused(vec![
                "at most 20 files can be deleted per command".to_string(),
            ]));
        }
        let mut files = s.list_newest()?;
        for _ in 0..500 {
            self.check_cancel()?;
            if !s.more_available() || paths.iter().all(|p| find_file(&files, p).is_some()) {
                break;
            }
            let next = s.list_next_page()?;
            if next.is_empty() {
                break;
            }
            files.extend(next);
        }
        self.warn_if_unconfirmed(s);
        plan_deletions(&files, paths)
            .map(|plan| plan.into_iter().cloned().collect())
            .map_err(SdkError::Refused)
    }

    /// The exact files `delete` would remove, or why not. Sends nothing.
    pub fn plan_delete(&self, paths: &[String]) -> Result<Vec<CameraFile>, SdkError> {
        self.start();
        let mut s = self.session()?;
        let plan = self.plan_on(&mut s, paths);
        s.close();
        plan
    }

    /// Delete the files. IRREVERSIBLE. Re-plans on the session that sends the delete, and confirms on a
    /// fresh session that every file is gone.
    pub fn delete(&self, paths: &[String]) -> Result<(), SdkError> {
        self.start();
        let mut s = self.session()?;
        let plan = self.plan_on(&mut s, paths)?;
        self.check_cancel()?;
        s.set_write_counter_start(self.counter.next());
        let refs: Vec<&CameraFile> = plan.iter().collect();
        let result = s.delete_files(&refs);
        self.counter.save(s.next_write_counter());
        result?;
        // A second handshake on the same session makes the camera answer empty.
        drop(s);
        let after = self
            .session()
            .and_then(|mut c| c.list_newest().map_err(SdkError::from))
            .map_err(|e| {
                SdkError::Session(format!(
                    "the camera answered OK but the check failed ({e}): list the camera to see what is left"
                ))
            })?;
        if after.is_empty() {
            return Err(SdkError::Session(
                "the camera answered OK but the check returned no files at all, so it cannot be confirmed".to_string(),
            ));
        }
        let left: Vec<&str> = plan
            .iter()
            .filter(|f| after.iter().any(|a| a.path == f.path))
            .map(|f| f.path.as_str())
            .collect();
        if !left.is_empty() {
            return Err(SdkError::Session(format!(
                "the camera answered OK but these files are still listed: {}",
                left.join(", ")
            )));
        }
        Ok(())
    }

    /// Cut `from..to` seconds out of the clip into `dest` without re-encoding, reading only the needed
    /// bytes from the camera. The cut starts at the keyframe at or before `from`.
    pub fn trim(
        &self,
        path: &str,
        from: f64,
        to: f64,
        dest: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<TrimOutcome, SdkError> {
        self.start();
        let path = validate_get_path(path).map_err(SdkError::NotFound)?;
        let mut s = self.session()?;
        let file = self.lookup(&mut s, path)?;
        if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| SdkError::Io(e.to_string()))?;
        }
        let held = Held::start(s);
        let mut src = HttpRangeSource::new(self.http(), &file.url_path());
        if let Some(b) = self.http_backoff {
            src.backoff = b;
        }
        let cancel = self.cancel.clone();
        let result = mp4::trim(
            &mut src,
            dest,
            from,
            to,
            &move || cancel.load(Ordering::Relaxed),
            progress,
        );
        held.release();
        Ok(result?)
    }

    /// One still frame's worth of video: `trim(at, at + 0.5)`.
    pub fn frame_clip(&self, path: &str, at: f64, dest: &Path) -> Result<TrimOutcome, SdkError> {
        self.trim(path, at, at + 0.5, dest, &mut |_, _| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::Kind;
    use osmo_core::datalink::{delete_payload, favourite_payload};
    use osmo_core::duml::Frame;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<(u8, u8, Vec<u8>)>>>;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../osmo-core/tests/fixtures/manifests/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn wrap(frame: Vec<u8>) -> Vec<u8> {
        let mut d = vec![0u8; 20];
        d[6] = 5;
        d.extend(frame);
        d
    }

    fn chunk(sub: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![0x4A, sub, 0, 0, 1, 0, 0, 0, 0, 0];
        p.extend(body);
        wrap(
            Frame {
                target: 0x0201,
                id: 1,
                ty: 0xC0 | 0x27 << 16,
                payload: p,
            }
            .encode(),
        )
    }

    fn manifest_frames(manifest: &[u8]) -> Vec<Vec<u8>> {
        let mut v = vec![chunk(0x04, &[])];
        v.extend(manifest.chunks(400).map(|c| chunk(0x01, c)));
        v.push(chunk(0x03, &[]));
        v
    }

    fn playback_status() -> Vec<u8> {
        let mut p = vec![0u8; 16];
        p[3] = 0x40; // bit 30 of the LE flags word
        wrap(
            Frame {
                target: 0x0201,
                id: 1,
                ty: 0x40 | 0x02 << 8 | 0x80 << 16,
                payload: p,
            }
            .encode(),
        )
    }

    /// A camera that answers the handshake, enters playback, serves `pages` of the SD listing in order and
    /// replies to the scripted writes.
    struct Fake {
        inbox: VecDeque<Vec<u8>>,
        log: Log,
        pages: Vec<Vec<Vec<u8>>>,
        sd_queries: usize,
        replies: Vec<((u8, u8), Vec<u8>)>,
    }

    impl Datagram for Fake {
        fn send(&mut self, pkt: &[u8]) -> std::io::Result<()> {
            if pkt[6] == 0 {
                let mut r = vec![0u8; 12];
                r[6] = 0;
                self.inbox.push_back(r);
            }
            if pkt[6] == 5 {
                let f = Frame::decode(&pkt[20..]).expect("malformed DUML");
                let (set, cmd, p) = (f.cmd_set(), f.cmd_id(), f.payload.clone());
                if (set, cmd) == (0x02, 0x0C) && p == [1, 1, 0, 1] {
                    self.inbox.push_back(playback_status());
                }
                if (set, cmd) == (0x00, 0x26) && p.len() > 20 && p[4] == 1 {
                    if let Some(page) = self.pages.get(self.sd_queries) {
                        self.inbox.extend(page.clone());
                    }
                    self.sd_queries += 1;
                }
                if let Some((_, r)) = self.replies.iter().find(|(k, _)| *k == (set, cmd)) {
                    let ty = 0xC0 | (set as u32) << 8 | (cmd as u32) << 16;
                    self.inbox.push_back(wrap(
                        Frame {
                            target: 0x0201,
                            id: 1,
                            ty,
                            payload: r.clone(),
                        }
                        .encode(),
                    ));
                }
                self.log.lock().unwrap().push((set, cmd, p));
            }
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> std::io::Result<Option<Vec<u8>>> {
            if let Some(x) = self.inbox.pop_front() {
                return Ok(Some(x));
            }
            std::thread::sleep(timeout.min(Duration::from_millis(1)));
            Ok(None)
        }
    }

    struct Rig {
        cam: Camera,
        log: Log,
        notices: Arc<Mutex<Vec<String>>>,
        counter: Arc<TestCounter>,
    }

    struct TestCounter(AtomicU32);
    impl WriteCounter for TestCounter {
        fn next(&self) -> u32 {
            self.0.load(Ordering::Relaxed)
        }
        fn save(&self, next: u32) {
            self.0.store(next, Ordering::Relaxed);
        }
    }

    /// `pages_for(n)` gives the SD listing pages the n-th opened session sees.
    fn rig(
        http_base: &str,
        replies: Vec<((u8, u8), Vec<u8>)>,
        pages_for: impl Fn(usize) -> Vec<Vec<u8>> + Send + Sync + 'static,
    ) -> Rig {
        let log: Log = Arc::default();
        let opened = Arc::new(AtomicU32::new(0));
        let l = log.clone();
        let open: Arc<Opener> = Arc::new(move || {
            let n = opened.fetch_add(1, Ordering::Relaxed) as usize;
            let pages = pages_for(n).iter().map(|m| manifest_frames(m)).collect();
            let fake = Fake {
                inbox: VecDeque::new(),
                log: l.clone(),
                pages,
                sd_queries: 0,
                replies: replies.clone(),
            };
            Ok(CameraSession::new(
                Box::new(fake) as Box<dyn Datagram + Send>,
                "192.168.2.1:9004".to_string(),
                Timing::fast(),
            ))
        });
        let mut cam = Camera::with_parts(open, http_base);
        let notices: Arc<Mutex<Vec<String>>> = Arc::default();
        let n = notices.clone();
        cam.set_notice(move |m| n.lock().unwrap().push(m.to_string()));
        let counter = Arc::new(TestCounter(AtomicU32::new(7)));
        cam.set_write_counter(counter.clone());
        Rig {
            cam,
            log,
            notices,
            counter,
        }
    }

    const NO_HTTP: &str = "http://127.0.0.1:1";

    fn sent(log: &Log, set: u8, cmd: u8) -> Vec<Vec<u8>> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|(s, c, _)| (*s, *c) == (set, cmd))
            .map(|(_, _, p)| p.clone())
            .collect()
    }

    fn all() -> LsFilter {
        LsFilter {
            kind: Kind::All,
            starred: false,
            highlights: false,
        }
    }

    fn two_pages() -> Vec<Vec<u8>> {
        vec![fixture("oa4_45.bin"), fixture("oa4_6.bin")]
    }

    /// Page 2 whose DJI_..._0006_D.MP4 carries the handle of page 1's 0055_D.MP4.
    fn pages_with_a_cross_page_clash() -> Vec<Vec<u8>> {
        let mut page2 = fixture("oa4_6.bin");
        let from = 0x0004_0060u32.to_le_bytes();
        let at = page2.windows(4).position(|w| w == from).unwrap();
        page2[at..at + 4].copy_from_slice(&0x0004_0370u32.to_le_bytes());
        vec![fixture("oa4_45.bin"), page2]
    }

    const A0055: &str = "DCIM/DJI_001/DJI_20260904142344_0055_D.MP4";
    const A0054: &str = "DCIM/DJI_001/DJI_20260904002228_0054_D.JPG";
    const B0006: &str = "DCIM/DJI_001/DJI_20260812131032_0006_D.MP4";

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_returns_the_newest_page_by_default_and_every_page_with_all() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        let newest = r.cam.list(&all(), false, None).unwrap();
        assert_eq!(newest.len(), 45);
        let everything = r.cam.list(&all(), true, None).unwrap();
        assert_eq!(everything.len(), 51);
        assert_eq!(everything[45].path, B0006);
    }

    #[test]
    fn list_applies_the_kind_and_starred_filters() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        let f = |kind, starred| LsFilter {
            kind,
            starred,
            highlights: false,
        };
        let videos = r.cam.list(&f(Kind::Videos, false), false, None).unwrap();
        assert_eq!(videos.len(), 5);
        assert!(videos.iter().all(|x| x.is_video()));
        let photos = r.cam.list(&f(Kind::Photos, false), false, None).unwrap();
        assert_eq!(photos.len(), 40);
        let starred = r.cam.list(&f(Kind::All, true), false, None).unwrap();
        assert_eq!(starred.len(), 2);
        assert!(starred.iter().all(|x| x.starred));
    }

    #[test]
    fn list_passes_the_page_size_to_the_camera() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        r.cam.list(&all(), false, Some(5)).unwrap();
        let queries = sent(&r.log, 0x00, 0x26);
        assert_eq!(
            queries.iter().find(|p| p.len() > 20 && p[4] == 1).unwrap()[14],
            5
        );
    }

    #[test]
    fn plan_delete_refuses_a_typo_and_sends_nothing() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        let err = r
            .cam
            .plan_delete(&strings(&[
                A0055,
                "DCIM/DJI_001/DJI_20260904142344_0056_D.MP4",
            ]))
            .unwrap_err();
        assert!(
            matches!(&err, SdkError::Refused(l) if l.len() == 1 && l[0].contains("0056")),
            "{err:?}"
        );
        assert!(sent(&r.log, 0x00, 0x28).is_empty());
    }

    #[test]
    fn plan_delete_refuses_a_handle_shared_with_a_file_on_another_page() {
        let r = rig(NO_HTTP, vec![], |_| pages_with_a_cross_page_clash());
        let err = r.cam.plan_delete(&strings(&[A0055, B0006])).unwrap_err();
        let SdkError::Refused(lines) = &err else {
            panic!("{err:?}")
        };
        // The session already withholds the clashing page-2 file's handle, so that file is the refusal.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(B0006) && l.contains("will not be deleted")),
            "{lines:?}"
        );
        assert!(r.cam.delete(&strings(&[A0055, B0006])).is_err());
        assert!(sent(&r.log, 0x00, 0x28).is_empty());
    }

    #[test]
    fn plan_delete_returns_the_files_it_would_delete() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        let plan = r.cam.plan_delete(&strings(&[B0006, A0055])).unwrap();
        assert_eq!(
            plan.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            [B0006, A0055]
        );
        assert!(sent(&r.log, 0x00, 0x28).is_empty(), "planning never writes");
    }

    #[test]
    fn delete_never_sends_without_a_plan() {
        let r = rig(NO_HTTP, vec![((0x00, 0x28), vec![0, 0])], |_| two_pages());
        for bad in [
            strings(&["DCIM/DJI_001/NOPE.JPG"]),
            strings(&["DCIM/*"]),
            strings(&[]),
            strings(&[A0055, "DCIM/DJI_001/NOPE.JPG"]),
        ] {
            assert!(
                matches!(r.cam.delete(&bad), Err(SdkError::Refused(_))),
                "{bad:?}"
            );
        }
        assert!(sent(&r.log, 0x00, 0x28).is_empty());
        assert_eq!(r.counter.next(), 7, "no write, so the counter is untouched");
    }

    #[test]
    fn delete_sends_the_planned_handles_and_confirms_on_a_fresh_session() {
        // The first session sees the full library; the check session no longer lists the file.
        let r = rig(NO_HTTP, vec![((0x00, 0x28), vec![0, 0])], |n| {
            if n == 0 {
                two_pages()
            } else {
                vec![fixture("oa4_6.bin")]
            }
        });
        r.cam.delete(&strings(&[A0055])).unwrap();
        assert_eq!(
            sent(&r.log, 0x00, 0x28),
            vec![delete_payload(&[0x0004_0370], 7)]
        );
        assert_eq!(r.counter.next(), 8, "the counter continues after a write");
    }

    #[test]
    fn delete_reports_a_file_that_is_still_listed() {
        let r = rig(NO_HTTP, vec![((0x00, 0x28), vec![0, 0])], |_| two_pages());
        let err = r.cam.delete(&strings(&[A0055])).unwrap_err();
        assert!(
            matches!(&err, SdkError::Session(m) if m.contains("still listed")),
            "{err:?}"
        );
    }

    #[test]
    fn favourite_sends_the_command_handle_and_warns_when_the_listing_has_not_changed() {
        let r = rig(NO_HTTP, vec![((0x02, 0xBF), vec![0x00])], |_| two_pages());
        let files = r.cam.list(&all(), false, None).unwrap();
        let file = files.iter().find(|f| f.path == A0054).unwrap();
        r.cam.set_favourite(A0054, true).unwrap();
        assert_eq!(
            sent(&r.log, 0x02, 0xBF),
            vec![favourite_payload(file.cmd_handle, 7, true)]
        );
        assert_eq!(r.counter.next(), 8);
        assert!(r
            .notices
            .lock()
            .unwrap()
            .iter()
            .any(|n| n.contains("does not show the change")));
    }

    #[test]
    fn a_missing_file_is_not_found_and_nothing_is_written() {
        let r = rig(NO_HTTP, vec![((0x02, 0xBF), vec![0x00])], |_| two_pages());
        let err = r
            .cam
            .set_favourite("DCIM/DJI_001/NOPE.JPG", true)
            .unwrap_err();
        assert!(matches!(err, SdkError::NotFound(_)), "{err:?}");
        assert!(sent(&r.log, 0x02, 0xBF).is_empty());
        assert!(matches!(
            r.cam.set_favourite("../etc/passwd", true),
            Err(SdkError::NotFound(_))
        ));
    }

    /// Serves one body for any path with `Range` and `HEAD` support.
    fn serve(body: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if s.read(&mut b).unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(b[0]);
                    }
                    let text = String::from_utf8_lossy(&head).to_string();
                    let range = text.lines().find_map(|l| {
                        l.strip_prefix("Range: bytes=")
                            .or(l.strip_prefix("range: bytes="))
                    });
                    let (a, z) = range.and_then(|r| r.split_once('-')).map_or(
                        (0, body.len() - 1),
                        |(a, z)| {
                            (
                                a.parse().unwrap(),
                                z.parse().unwrap_or(body.len() - 1).min(body.len() - 1),
                            )
                        },
                    );
                    let part = &body[a..=z];
                    let status = if range.is_some() {
                        "206 Partial Content"
                    } else {
                        "200 OK"
                    };
                    let out = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{z}/{}\r\nConnection: close\r\n\r\n",
                        if text.starts_with("HEAD") { body.len() } else { part.len() },
                        body.len()
                    );
                    let _ = s.write_all(out.as_bytes());
                    if !text.starts_with("HEAD") {
                        let _ = s.write_all(part);
                    }
                    let _ = s.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        base
    }

    fn photo_bytes() -> Vec<u8> {
        (0..4_349_952usize).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn download_saves_the_file_and_reports_progress() {
        let body = photo_bytes();
        let r = rig(&serve(body.clone()), vec![], |_| two_pages());
        let dir = tempfile::tempdir().unwrap();
        let mut last = (0, 0);
        let out = r
            .cam
            .download(A0054, dir.path(), &mut |d, t| last = (d, t))
            .unwrap();
        assert_eq!(std::fs::read(&out.dest).unwrap(), body);
        assert_eq!(
            (out.bytes, out.already_complete),
            (body.len() as u64, false)
        );
        assert_eq!(last, (body.len() as u64, body.len() as u64));
        // Run again: nothing left to fetch.
        let again = r.cam.download(A0054, dir.path(), &mut |_, _| {}).unwrap();
        assert!(again.already_complete);
    }

    #[test]
    fn cancel_stops_a_download_within_one_chunk() {
        let r = rig(&serve(photo_bytes()), vec![], |_| two_pages());
        let dir = tempfile::tempdir().unwrap();
        let mut first = None;
        let err = r
            .cam
            .download(A0054, dir.path(), &mut |d, _| {
                if first.is_none() {
                    first = Some(d);
                    r.cam.cancel();
                }
            })
            .unwrap_err();
        assert_eq!(err, SdkError::Cancelled);
        let on_disk = std::fs::metadata(dir.path().join(dest_filename(A0054)))
            .unwrap()
            .len();
        assert!(on_disk <= first.unwrap() + 65_536, "{on_disk} vs {first:?}");
        assert!(on_disk < 4_349_952);
        // The next operation starts clean.
        assert!(r.cam.list(&all(), false, None).is_ok());
    }

    #[test]
    fn cancel_before_an_operation_is_cleared_when_it_starts() {
        let r = rig(NO_HTTP, vec![], |_| two_pages());
        r.cam.cancel();
        assert!(r.cam.list(&all(), false, None).is_ok());
    }

    #[test]
    fn trim_and_frame_clip_report_a_bad_file_as_an_mp4_error() {
        // The served body is not an MP4, so the cut fails cleanly and leaves no file.
        let r = rig(&serve(vec![7u8; 4096]), vec![], |_| two_pages());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = r
            .cam
            .trim(A0055, 0.0, 1.0, &dest, &mut |_, _| {})
            .unwrap_err();
        assert!(matches!(err, SdkError::Mp4(_)), "{err:?}");
        assert!(r.cam.frame_clip(A0055, 2.0, &dest).is_err());
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
