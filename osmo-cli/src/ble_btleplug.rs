//! Desktop BLE via btleplug, bridged to the blocking `BleLink` the core expects.
use btleplug::api::bleuuid::uuid_from_u16;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use osmo_cli::filter::{bluetooth_hint, device_matches, looks_like_camera};
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

pub fn pick(cams: Vec<Found>, wanted: Option<&str>) -> Option<Found> {
    cams.into_iter()
        .find(|c| wanted.is_none_or(|w| device_matches(&c.id, c.name.as_deref(), w)))
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

fn connect_inner(
    rt: Arc<Runtime>,
    found: &Found,
    arm_pairing: bool,
) -> Result<BtleplugLink, String> {
    let p = found.peripheral.clone();
    let (fff5_c, rx) = rt.block_on(async {
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
            return Err("camera does not expose the fff4/fff5 control characteristics".to_string());
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
        let _ = self.rt.block_on(self.peripheral.disconnect());
    }
}
