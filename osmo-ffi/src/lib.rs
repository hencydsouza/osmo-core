//! UniFFI surface of the Osmo SDK for Swift, Kotlin and C#. Every call is blocking: apps run it on a
//! background thread. A panic inside a call becomes `OsmoError::Internal`, never a crash.
use osmo_core::ble::{self, LinkError, PairError, PairOptions};
use osmo_sdk::camera::{Camera as SdkCamera, SdkError};
use osmo_sdk::listing::{Kind, LsFilter};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Error)]
pub enum OsmoError {
    Session { detail: String },
    NotFound { detail: String },
    Refused { detail: String },
    Io { detail: String },
    Mp4 { detail: String },
    Cancelled { detail: String },
    Ble { detail: String },
    Internal { detail: String },
}

impl std::fmt::Display for OsmoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (k, m) = match self {
            OsmoError::Session { detail: message } => ("session", message),
            OsmoError::NotFound { detail: message } => ("not found", message),
            OsmoError::Refused { detail: message } => ("refused", message),
            OsmoError::Io { detail: message } => ("io", message),
            OsmoError::Mp4 { detail: message } => ("mp4", message),
            OsmoError::Cancelled { detail: message } => ("cancelled", message),
            OsmoError::Ble { detail: message } => ("ble", message),
            OsmoError::Internal { detail: message } => ("internal", message),
        };
        write!(f, "{k}: {m}")
    }
}

impl std::error::Error for OsmoError {}

impl From<SdkError> for OsmoError {
    fn from(e: SdkError) -> Self {
        let message = e.to_string();
        match e {
            SdkError::Session(_) => OsmoError::Session { detail: message },
            SdkError::NotFound(_) => OsmoError::NotFound { detail: message },
            SdkError::Refused(_) => OsmoError::Refused { detail: message },
            SdkError::Io(_) => OsmoError::Io { detail: message },
            SdkError::Mp4(_) => OsmoError::Mp4 { detail: message },
            SdkError::Cancelled => OsmoError::Cancelled { detail: message },
        }
    }
}

/// Run `f`, turning a panic into `OsmoError::Internal`.
fn guarded<T>(f: impl FnOnce() -> Result<T, OsmoError>) -> Result<T, OsmoError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let message = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(OsmoError::Internal {
                detail: format!("internal error: {message}"),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaKind {
    Photo,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MediaFile {
    pub path: String,
    pub size_bytes: u64,
    pub starred: bool,
    pub kind: MediaKind,
    pub storage: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ListFilter {
    pub kind: Option<MediaKind>,
    pub starred_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct TrimResult {
    pub actual_start_secs: f64,
    pub duration_secs: f64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WifiCredentials {
    pub ssid: String,
    /// Some cameras (the Nano) do not return a password over BLE.
    pub password: Option<String>,
}

/// Progress of a long transfer. Return `false` to cancel it.
#[uniffi::export(with_foreign)]
pub trait ProgressListener: Send + Sync {
    fn on_progress(&self, done: u64, total: u64) -> bool;
}

/// A connected BLE GATT channel, implemented by the app (CoreBluetooth, Android BLE, WinRT). It has
/// already connected, enabled notifications on `fff4`/`fff5` and armed pairing.
#[uniffi::export(with_foreign)]
pub trait BleLink: Send + Sync {
    fn write_command(&self, bytes: Vec<u8>) -> Result<(), OsmoError>;
    fn recv(&self, timeout_ms: u32) -> Result<Option<Vec<u8>>, OsmoError>;
}

struct LinkAdapter(Arc<dyn BleLink>);

fn link_err(e: OsmoError) -> LinkError {
    match e {
        OsmoError::Ble { detail: message } if message.contains("disconnected") => {
            LinkError::Disconnected
        }
        other => LinkError::Other(other.to_string()),
    }
}

impl ble::BleLink for LinkAdapter {
    fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.0.write_command(bytes.to_vec()).map_err(link_err)
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        self.0.recv(ms).map_err(link_err)
    }
}

/// Pair over BLE (the user approves on the camera) and read the camera's Wi-Fi name and password.
#[uniffi::export]
pub fn read_wifi_credentials(link: Arc<dyn BleLink>) -> Result<WifiCredentials, OsmoError> {
    guarded(|| {
        let mut adapter = LinkAdapter(link);
        let creds = ble::session::pair_and_read_wifi(
            &mut adapter,
            &PairOptions::default(),
            &mut |_step| {},
        )
        .map_err(|e| match e {
            PairError::Cancelled => OsmoError::Cancelled {
                detail: e.to_string(),
            },
            other => OsmoError::Ble {
                detail: other.to_string(),
            },
        })?;
        Ok(WifiCredentials {
            ssid: creds.ssid,
            password: creds.password,
        })
    })
}

/// Cut `from_secs..to_secs` out of an MP4 already on disk, without re-encoding. The cut starts at the
/// keyframe at or before `from_secs`. Needs no camera, so apps can trim a downloaded clip.
#[uniffi::export]
pub fn trim_local_file(
    src_path: String,
    from_secs: f64,
    to_secs: f64,
    dest_path: String,
    progress: Option<Arc<dyn ProgressListener>>,
) -> Result<TrimResult, OsmoError> {
    guarded(|| {
        check_secs("from", from_secs)?;
        check_secs("to", to_secs)?;
        let mut src =
            osmo_core::mp4::FileSource::open(Path::new(&src_path)).map_err(|e| OsmoError::Io {
                detail: e.to_string(),
            })?;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut p = |done, total| {
            if let Some(l) = &progress {
                if !l.on_progress(done, total) {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        };
        let r = osmo_core::mp4::trim(
            &mut src,
            dest(&dest_path)?,
            from_secs,
            to_secs,
            &|| stop.load(std::sync::atomic::Ordering::Relaxed),
            &mut p,
        )
        .map_err(|e| OsmoError::from(SdkError::from(e)))?;
        Ok(TrimResult {
            actual_start_secs: r.actual_start_secs,
            duration_secs: r.duration_secs,
            bytes: r.bytes,
        })
    })
}

#[uniffi::export]
pub fn sdk_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn media_file(f: &osmo_core::media::CameraFile) -> MediaFile {
    MediaFile {
        path: f.path.clone(),
        size_bytes: f.size_bytes,
        starred: f.starred,
        kind: if f.is_video() {
            MediaKind::Video
        } else {
            MediaKind::Photo
        },
        storage: f.storage,
    }
}

fn check_secs(name: &str, v: f64) -> Result<(), OsmoError> {
    if v.is_finite() && v >= 0.0 {
        Ok(())
    } else {
        Err(OsmoError::Refused {
            detail: format!("{name} must be a finite number of seconds, 0 or more (got {v})"),
        })
    }
}

fn dest(p: &str) -> Result<&Path, OsmoError> {
    if p.is_empty() {
        return Err(OsmoError::Io {
            detail: "empty destination path".to_string(),
        });
    }
    Ok(Path::new(p))
}

#[derive(uniffi::Object)]
pub struct Camera {
    inner: SdkCamera,
}

/// Adapt a foreign listener to the SDK's progress callback; a `false` answer cancels the operation.
fn progress_fn<'a>(
    cam: &'a SdkCamera,
    l: &'a Option<Arc<dyn ProgressListener>>,
) -> impl FnMut(u64, u64) + 'a {
    move |done, total| {
        if let Some(l) = l {
            if !l.on_progress(done, total) {
                cam.cancel();
            }
        }
    }
}

#[uniffi::export]
impl Camera {
    /// `port` is the datalink port: 9004 for the Action 4 (with the TCP poke).
    #[uniffi::constructor]
    pub fn new(ip: String, port: u16) -> Arc<Self> {
        Arc::new(Camera {
            inner: SdkCamera::new(&ip, port),
        })
    }

    pub fn list(&self, filter: ListFilter, all: bool) -> Result<Vec<MediaFile>, OsmoError> {
        guarded(|| {
            let f = LsFilter {
                kind: match filter.kind {
                    None => Kind::All,
                    Some(MediaKind::Photo) => Kind::Photos,
                    Some(MediaKind::Video) => Kind::Videos,
                },
                starred: filter.starred_only,
                highlights: false,
            };
            let files = self.inner.list(&f, all, None)?;
            Ok(files.iter().map(media_file).collect())
        })
    }

    pub fn thumbnail(&self, path: String) -> Result<Vec<u8>, OsmoError> {
        guarded(|| Ok(self.inner.thumbnail(&path)?))
    }

    /// Returns the path of the saved file.
    pub fn download(
        &self,
        path: String,
        dest_dir: String,
        progress: Option<Arc<dyn ProgressListener>>,
    ) -> Result<String, OsmoError> {
        guarded(|| {
            let mut p = progress_fn(&self.inner, &progress);
            let out = self.inner.download(&path, dest(&dest_dir)?, &mut p)?;
            Ok(out.dest.to_string_lossy().into_owned())
        })
    }

    pub fn set_favourite(&self, path: String, on: bool) -> Result<(), OsmoError> {
        guarded(|| Ok(self.inner.set_favourite(&path, on)?))
    }

    /// The files `delete` would remove, or an error saying why not. Sends nothing.
    pub fn plan_delete(&self, paths: Vec<String>) -> Result<Vec<MediaFile>, OsmoError> {
        guarded(|| {
            let plan = self.inner.plan_delete(&paths)?;
            Ok(plan.iter().map(media_file).collect())
        })
    }

    /// IRREVERSIBLE. Re-plans and confirms internally.
    pub fn delete(&self, paths: Vec<String>) -> Result<(), OsmoError> {
        guarded(|| Ok(self.inner.delete(&paths)?))
    }

    pub fn trim(
        &self,
        path: String,
        from_secs: f64,
        to_secs: f64,
        dest_path: String,
        progress: Option<Arc<dyn ProgressListener>>,
    ) -> Result<TrimResult, OsmoError> {
        guarded(|| {
            check_secs("from", from_secs)?;
            check_secs("to", to_secs)?;
            let mut p = progress_fn(&self.inner, &progress);
            let r = self
                .inner
                .trim(&path, from_secs, to_secs, dest(&dest_path)?, &mut p)?;
            Ok(TrimResult {
                actual_start_secs: r.actual_start_secs,
                duration_secs: r.duration_secs,
                bytes: r.bytes,
            })
        })
    }

    /// A half-second clip at `at_secs`, from which the app can extract a still frame.
    pub fn frame_clip(
        &self,
        path: String,
        at_secs: f64,
        dest_path: String,
    ) -> Result<TrimResult, OsmoError> {
        guarded(|| {
            check_secs("at", at_secs)?;
            let r = self.inner.frame_clip(&path, at_secs, dest(&dest_path)?)?;
            Ok(TrimResult {
                actual_start_secs: r.actual_start_secs,
                duration_secs: r.duration_secs,
                bytes: r.bytes,
            })
        })
    }

    /// Stop the running call at its next step. Safe from any thread.
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn panic_inside_a_call_becomes_osmo_error_internal() {
        let r: Result<(), _> = guarded(|| panic!("boom"));
        assert!(
            matches!(&r, Err(OsmoError::Internal { detail: message }) if message.contains("boom")),
            "{r:?}"
        );
        let r: Result<(), _> = guarded(|| panic!("{}", String::from("formatted")));
        assert!(
            matches!(&r, Err(OsmoError::Internal { detail: message }) if message.contains("formatted"))
        );
    }

    #[test]
    fn negative_or_nan_times_are_errors_not_panics() {
        let cam = Camera::new("127.0.0.1".into(), 9);
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let r = cam.trim(
                "DCIM/DJI_001/X.MP4".into(),
                bad,
                2.0,
                "/tmp/x.mp4".into(),
                None,
            );
            assert!(matches!(r, Err(OsmoError::Refused { .. })), "from={bad}");
            let r = cam.trim(
                "DCIM/DJI_001/X.MP4".into(),
                0.0,
                bad,
                "/tmp/x.mp4".into(),
                None,
            );
            assert!(matches!(r, Err(OsmoError::Refused { .. })), "to={bad}");
            let r = cam.frame_clip("DCIM/DJI_001/X.MP4".into(), bad, "/tmp/x.mp4".into());
            assert!(matches!(r, Err(OsmoError::Refused { .. })));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_free_path_is_an_error() {
        // Foreign strings are always valid UTF-8; the empty string is the degenerate path we must refuse.
        let cam = Camera::new("127.0.0.1".into(), 9);
        let r = cam.trim("DCIM/DJI_001/X.MP4".into(), 0.0, 1.0, String::new(), None);
        assert!(matches!(r, Err(OsmoError::Io { .. })), "{r:?}");
        let r = cam.download("../../etc/passwd".into(), "/tmp".into(), None);
        assert!(matches!(r, Err(OsmoError::NotFound { .. })), "{r:?}");
    }

    struct Listener {
        calls: Mutex<Vec<(u64, u64)>>,
        keep_going: bool,
    }
    impl ProgressListener for Listener {
        fn on_progress(&self, done: u64, total: u64) -> bool {
            self.calls.lock().unwrap().push((done, total));
            self.keep_going
        }
    }

    #[test]
    fn progress_listener_returning_false_cancels() {
        let cam = SdkCamera::new("127.0.0.1", 9);
        let l = Arc::new(Listener {
            calls: Mutex::default(),
            keep_going: false,
        });
        let opt: Option<Arc<dyn ProgressListener>> = Some(l.clone());
        {
            let mut p = progress_fn(&cam, &opt);
            p(10, 100);
            p(20, 100);
        }
        assert_eq!(l.calls.lock().unwrap().len(), 2);
        let go = Arc::new(Listener {
            calls: Mutex::default(),
            keep_going: true,
        });
        let opt: Option<Arc<dyn ProgressListener>> = Some(go.clone());
        progress_fn(&cam, &opt)(1, 2);
        assert_eq!(go.calls.lock().unwrap().len(), 1);
    }

    struct Foreign {
        written: Mutex<Vec<Vec<u8>>>,
        reply: Mutex<Option<Result<Option<Vec<u8>>, OsmoError>>>,
        fail_write: bool,
    }
    impl BleLink for Foreign {
        fn write_command(&self, bytes: Vec<u8>) -> Result<(), OsmoError> {
            if self.fail_write {
                return Err(OsmoError::Ble {
                    detail: "device disconnected".into(),
                });
            }
            self.written.lock().unwrap().push(bytes);
            Ok(())
        }
        fn recv(&self, _t: u32) -> Result<Option<Vec<u8>>, OsmoError> {
            self.reply.lock().unwrap().take().unwrap_or(Ok(None))
        }
    }

    #[test]
    fn ble_adapter_forwards_write_and_recv_to_the_foreign_link_and_maps_errors() {
        use ble::BleLink as _;
        let f = Arc::new(Foreign {
            written: Mutex::default(),
            reply: Mutex::new(Some(Ok(Some(vec![1, 2, 3])))),
            fail_write: false,
        });
        let mut a = LinkAdapter(f.clone());
        a.write_command(&[9, 8]).unwrap();
        assert_eq!(f.written.lock().unwrap()[0], vec![9, 8]);
        assert_eq!(
            a.recv(Duration::from_millis(5)).unwrap(),
            Some(vec![1, 2, 3])
        );
        assert_eq!(a.recv(Duration::from_millis(5)).unwrap(), None);
        *f.reply.lock().unwrap() = Some(Err(OsmoError::Ble {
            detail: "gatt 133".into(),
        }));
        assert!(
            matches!(a.recv(Duration::ZERO), Err(LinkError::Other(m)) if m.contains("gatt 133"))
        );
        let dead = Arc::new(Foreign {
            written: Mutex::default(),
            reply: Mutex::default(),
            fail_write: true,
        });
        assert!(matches!(
            LinkAdapter(dead).write_command(&[1]),
            Err(LinkError::Disconnected)
        ));
    }

    #[test]
    fn a_dead_link_makes_read_wifi_credentials_a_ble_error_not_a_hang() {
        let dead = Arc::new(Foreign {
            written: Mutex::default(),
            reply: Mutex::default(),
            fail_write: true,
        });
        let r = read_wifi_credentials(dead);
        assert!(matches!(r, Err(OsmoError::Ble { .. })), "{r:?}");
    }

    fn fixture(name: &str) -> String {
        format!(
            "{}/../osmo-core/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn trim_local_file_cuts_a_fixture_and_refuses_bad_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cut.mp4").to_string_lossy().into_owned();
        let r = trim_local_file(fixture("h264_aac.mp4"), 0.5, 1.5, out.clone(), None).unwrap();
        assert!(r.duration_secs > 0.9 && r.duration_secs < 1.6, "{r:?}");
        assert!(std::fs::metadata(&out).unwrap().len() == r.bytes);
        let bad = trim_local_file(fixture("h264_aac.mp4"), 2.0, 1.0, out.clone(), None);
        assert!(matches!(bad, Err(OsmoError::Mp4 { .. })), "{bad:?}");
        let missing = trim_local_file("/nonexistent.mp4".into(), 0.0, 1.0, out, None);
        assert!(matches!(missing, Err(OsmoError::Io { .. })), "{missing:?}");
        struct Stop;
        impl ProgressListener for Stop {
            fn on_progress(&self, _: u64, _: u64) -> bool {
                false
            }
        }
        let dest2 = dir
            .path()
            .join("stopped.mp4")
            .to_string_lossy()
            .into_owned();
        let c = trim_local_file(
            fixture("h264_aac.mp4"),
            0.0,
            1.5,
            dest2.clone(),
            Some(Arc::new(Stop)),
        );
        assert!(matches!(c, Err(OsmoError::Cancelled { .. })), "{c:?}");
        assert!(!Path::new(&dest2).exists());
    }

    #[test]
    fn sdk_version_matches_cargo_pkg() {
        assert_eq!(sdk_version(), env!("CARGO_PKG_VERSION"));
    }
}
