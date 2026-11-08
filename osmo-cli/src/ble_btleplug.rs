//! Desktop BLE via btleplug, bridged to the blocking `BleLink` the core expects.
use btleplug::api::bleuuid::uuid_from_u16;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use osmo_cli::filter::{bluetooth_hint, looks_like_camera, select_camera_index};
use osmo_core::ble::{decode_advert, is_dji_company_id, BleLink, LinkError};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tokio::runtime::Runtime;

pub struct Found {
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub model_id: Option<u16>,
    peripheral: Peripheral,
}

fn bt_err(e: impl std::fmt::Display) -> String {
    format!("{e}. {}", bluetooth_hint())
}

pub fn adapter(rt: &Runtime) -> Result<Adapter, String> {
    rt.block_on(async {
        let manager = Manager::new().await.map_err(bt_err)?;
        let adapters = manager.adapters().await.map_err(bt_err)?;
        adapters.into_iter().next().ok_or_else(bluetooth_hint)
    })
}

/// Scan for `seconds`; returns cameras and a count (never names) of other devices seen.
pub fn scan(rt: &Runtime, adapter: &Adapter, seconds: u64) -> Result<(Vec<Found>, usize), String> {
    rt.block_on(async {
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(bt_err)?;
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        let _ = adapter.stop_scan().await;
        let mut cams = Vec::new();
        let mut others = 0;
        for p in adapter.peripherals().await.map_err(bt_err)? {
            let Some(props) = p.properties().await.map_err(bt_err)? else {
                continue;
            };
            let cids: Vec<u16> = props.manufacturer_data.keys().copied().collect();
            if !looks_like_camera(props.local_name.as_deref(), &cids) {
                others += 1;
                continue;
            }
            let model_id = props
                .manufacturer_data
                .iter()
                .find(|(c, _)| is_dji_company_id(**c))
                .and_then(|(_, d)| decode_advert(d).model_id);
            cams.push(Found {
                id: p.id().to_string(),
                name: props.local_name,
                rssi: props.rssi,
                model_id,
                peripheral: p,
            });
        }
        Ok((cams, others))
    })
}

/// Scan until the wanted camera shows up, trying up to `attempts` times: a camera's Bluetooth adverts come and go,
/// so one short scan can miss it. `stop` ends the search early (Ctrl-C).
pub fn find_camera(
    rt: &Runtime,
    adapter: &Adapter,
    wanted: Option<&str>,
    attempts: usize,
    stop: &dyn Fn() -> bool,
) -> Result<Option<Found>, String> {
    for attempt in 1..=attempts {
        if stop() {
            return Ok(None);
        }
        if attempt > 1 {
            eprintln!("Camera not seen yet; scanning again ({attempt}/{attempts}) ...");
        }
        let (cams, _) = scan(rt, adapter, 6)?;
        if let Some(found) = pick(cams, wanted)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

pub fn pick(mut cams: Vec<Found>, wanted: Option<&str>) -> Result<Option<Found>, String> {
    let candidates: Vec<_> = cams
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_deref()))
        .collect();
    let index = select_camera_index(&candidates, wanted).map_err(str::to_owned)?;
    Ok(index.map(|i| cams.swap_remove(i)))
}

pub struct BtleplugLink {
    rt: Arc<Runtime>,
    peripheral: Peripheral,
    fff5: Characteristic,
    rx: mpsc::Receiver<Vec<u8>>,
}

pub fn connect(rt: Arc<Runtime>, found: &Found) -> Result<BtleplugLink, String> {
    connect_inner(rt, found, true)
}

/// R-SDK uses the same characteristics but does not arm the DUML pairing characteristic.
pub fn connect_rsdk(rt: Arc<Runtime>, found: &Found) -> Result<BtleplugLink, String> {
    connect_inner(rt, found, false)
}

const CONNECT_ATTEMPTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Connect with a time limit and a few retries: CoreBluetooth connects fail transiently, and a stalled one never
/// returns on its own.
/// Disconnect, giving up after 5 s: a Bluetooth stack that never answers must not hang the command on exit.
fn disconnect(rt: &Runtime, p: &Peripheral) {
    // The timeout must be built inside the runtime: outside it, tokio panics ("no reactor running").
    rt.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(5), p.disconnect()).await;
    });
}

fn connect_inner(
    rt: Arc<Runtime>,
    found: &Found,
    arm_pairing: bool,
) -> Result<BtleplugLink, String> {
    let mut last = String::new();
    for attempt in 1..=CONNECT_ATTEMPTS {
        if attempt > 1 {
            eprintln!(
                "Bluetooth connection failed ({last}); retrying ({attempt}/{CONNECT_ATTEMPTS}) ..."
            );
            disconnect(&rt, &found.peripheral);
            std::thread::sleep(Duration::from_secs(1));
        }
        match connect_once(rt.clone(), found, arm_pairing) {
            Ok(link) => return Ok(link),
            Err(e) => last = e,
        }
    }
    Err(format!("{last} (after {CONNECT_ATTEMPTS} attempts)"))
}

fn connect_once(
    rt: Arc<Runtime>,
    found: &Found,
    arm_pairing: bool,
) -> Result<BtleplugLink, String> {
    let p = found.peripheral.clone();
    let (fff5_c, rx) = rt.block_on(async {
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            p.connect().await.map_err(bt_err)?;
            p.discover_services().await.map_err(bt_err)?;
            let chars = p.characteristics();
            let find = |short: u16| {
                chars
                    .iter()
                    .find(|c| c.uuid == uuid_from_u16(short))
                    .cloned()
            };
            let (Some(c4), Some(c5)) = (find(0xFFF4), find(0xFFF5)) else {
                return Err(
                    "camera does not expose the fff4/fff5 control characteristics".to_string(),
                );
            };
            p.subscribe(&c4).await.map_err(bt_err)?;
            p.subscribe(&c5).await.map_err(bt_err)?;
            let mut stream = p.notifications().await.map_err(bt_err)?;
            let (tx, rx) = mpsc::channel();
            tokio::spawn(async move {
                while let Some(n) = stream.next().await {
                    if tx.send(n.value).is_err() {
                        break;
                    }
                }
            });
            if arm_pairing {
                // DUML pairing only: R-SDK has its own approval handshake.
                p.write(&c4, &[0x01, 0x00], WriteType::WithResponse)
                    .await
                    .map_err(bt_err)?;
            }
            Ok::<_, String>((c5, rx))
        })
        .await
        .map_err(|_| {
            format!(
                "Bluetooth connection timed out after {}s",
                CONNECT_TIMEOUT.as_secs()
            )
        })?
    })?;
    Ok(BtleplugLink {
        rt,
        peripheral: p,
        fff5: fff5_c,
        rx,
    })
}

impl BleLink for BtleplugLink {
    fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.rt
            .block_on(
                self.peripheral
                    .write(&self.fff5, bytes, WriteType::WithoutResponse),
            )
            .map_err(|e| LinkError::Other(e.to_string()))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        match self.rx.recv_timeout(timeout) {
            Ok(v) => Ok(Some(v)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(LinkError::Disconnected),
        }
    }
}

impl Drop for BtleplugLink {
    fn drop(&mut self) {
        disconnect(&self.rt, &self.peripheral);
    }
}
