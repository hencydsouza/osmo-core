//! GPS input conversion. Locations are never printed by this module.
use osmo_core::rsdk::GpsFix;
use serde::Deserialize;

pub const MAX_FIX_AGE_MS: u64 = 30_000;

#[derive(Deserialize)]
pub struct GpsInput {
    pub observed_unix_ms: u64,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude_m: f64,
    pub speed_mps: f64,
    pub bearing_deg: f64,
    pub horizontal_accuracy_m: f64,
    pub vertical_accuracy_m: f64,
    pub speed_accuracy_mps: f64,
    pub satellites: u32,
}

impl GpsInput {
    pub fn parse(line: &str) -> Result<Self, String> {
        serde_json::from_str(line).map_err(|_| "invalid GPS JSON input".to_string())
    }

    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms
            .checked_sub(self.observed_unix_ms)
            .is_some_and(|age| age <= MAX_FIX_AGE_MS)
    }

    pub fn to_fix(&self, date: u32, time: u32) -> Result<GpsFix, String> {
        if !self.latitude.is_finite()
            || !(-90.0..=90.0).contains(&self.latitude)
            || !self.longitude.is_finite()
            || !(-180.0..=180.0).contains(&self.longitude)
        {
            return Err("GPS coordinates are out of range".into());
        }
        if !self.speed_mps.is_finite()
            || self.speed_mps < 0.0
            || self.speed_mps > f32::MAX as f64 / 100.0
            || !self.bearing_deg.is_finite()
            || !(0.0..=360.0).contains(&self.bearing_deg)
        {
            return Err("GPS speed or bearing is invalid".into());
        }
        let signed = |v: f64, scale: f64| -> Result<i32, String> {
            let n = (v * scale).round();
            if !n.is_finite() || n < i32::MIN as f64 || n > i32::MAX as f64 {
                return Err("GPS altitude is out of range".into());
            }
            Ok(n as i32)
        };
        let accuracy = |v: f64, scale: f64| -> Result<u32, String> {
            let n = (v * scale).round();
            if !v.is_finite() || v < 0.0 || n > i32::MAX as f64 {
                return Err("GPS accuracy is out of range".into());
            }
            Ok(n as u32)
        };
        let bearing = self.bearing_deg.to_radians();
        let speed = self.speed_mps * 100.0;
        let fix = GpsFix {
            date,
            time,
            latitude_e7: (self.latitude * 1e7).round() as i32,
            longitude_e7: (self.longitude * 1e7).round() as i32,
            height_mm: signed(self.altitude_m, 1000.0)?,
            north_cm_s: (speed * bearing.cos()) as f32,
            east_cm_s: (speed * bearing.sin()) as f32,
            down_cm_s: 0.0,
            vertical_accuracy_mm: accuracy(self.vertical_accuracy_m, 1000.0)?,
            horizontal_accuracy_mm: accuracy(self.horizontal_accuracy_m, 1000.0)?,
            speed_accuracy_cm_s: accuracy(self.speed_accuracy_mps, 100.0)?,
            satellites: self.satellites,
        };
        osmo_core::rsdk::gps_payload(&fix).map_err(|e| e.to_string())?;
        Ok(fix)
    }
}

/// Tracks the latest fresh fix without retaining or logging older positions.
#[derive(Default)]
pub struct GpsFeedState {
    latest: Option<GpsInput>,
    dirty: bool,
}

impl GpsFeedState {
    pub fn accept(
        &mut self,
        input: GpsInput,
        now: u64,
        date: u32,
        time: u32,
    ) -> Result<bool, String> {
        if !input.is_fresh(now)
            || self
                .latest
                .as_ref()
                .is_some_and(|old| input.observed_unix_ms < old.observed_unix_ms)
        {
            return Ok(false);
        }
        input.to_fix(date, time)?;
        self.latest = Some(input);
        self.dirty = true;
        Ok(true)
    }

    pub fn next_fix(
        &mut self,
        now: u64,
        date: u32,
        time: u32,
        due: bool,
        closing: bool,
    ) -> Result<Option<GpsFix>, String> {
        let Some(input) = &self.latest else {
            return Ok(None);
        };
        if !input.is_fresh(now) || !(due || (closing && self.dirty)) {
            return Ok(None);
        }
        let fix = input.to_fix(date, time)?;
        self.dirty = false;
        Ok(Some(fix))
    }

    pub fn is_stale(&self, now: u64) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|input| !input.is_fresh(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const LINE: &str = r#"{"observed_unix_ms":100000,"latitude":41.4,"longitude":2.16,"altitude_m":42.0,"speed_mps":0.125,"bearing_deg":0.0,"horizontal_accuracy_m":1.0,"vertical_accuracy_m":1.0,"speed_accuracy_mps":0.1,"satellites":11}"#;

    #[test]
    fn input_becomes_dji_units_and_uses_the_current_local_time() {
        let input = GpsInput::parse(LINE).unwrap();
        let fix = input.to_fix(20260718, 143005).unwrap();
        assert_eq!(
            (fix.latitude_e7, fix.longitude_e7, fix.height_mm),
            (414000000, 21600000, 42000)
        );
        assert_eq!((fix.north_cm_s, fix.east_cm_s), (12.5, 0.0));
        assert_eq!((fix.date, fix.time, fix.satellites), (20260718, 143005, 11));
    }

    #[test]
    fn stale_and_future_fixes_are_not_fresh() {
        let input = GpsInput::parse(LINE).unwrap();
        assert!(input.is_fresh(130000));
        assert!(!input.is_fresh(130001));
        assert!(!input.is_fresh(99999));
    }

    #[test]
    fn feed_keeps_the_newest_fresh_fix_and_flushes_it_on_close() {
        let mut feed = GpsFeedState::default();
        let newer = LINE.replace("100000", "100010").replace("41.4", "42.0");
        assert!(feed
            .accept(GpsInput::parse(&newer).unwrap(), 100020, 20260718, 143005)
            .unwrap());
        assert!(!feed
            .accept(GpsInput::parse(LINE).unwrap(), 100020, 20260718, 143005)
            .unwrap());
        let fix = feed
            .next_fix(100020, 20260718, 143005, false, true)
            .unwrap()
            .unwrap();
        assert_eq!(fix.latitude_e7, 420000000);
        assert!(feed
            .next_fix(100020, 20260718, 143005, false, true)
            .unwrap()
            .is_none());
        assert!(feed
            .next_fix(130011, 20260718, 143006, true, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_and_out_of_range_inputs_are_rejected_without_echoing_coordinates() {
        let error = GpsInput::parse("not json private coordinates")
            .err()
            .unwrap();
        assert!(!error.contains("private coordinates"));
        let mut input = GpsInput::parse(LINE).unwrap();
        input.latitude = 91.0;
        assert!(input.to_fix(20260718, 143005).is_err());
        input.latitude = 0.0;
        input.speed_mps = f64::INFINITY;
        assert!(input.to_fix(20260718, 143005).is_err());
        input.speed_mps = 0.0;
        input.horizontal_accuracy_m = -0.0001;
        assert!(input.to_fix(20260718, 143005).is_err());
    }
}
