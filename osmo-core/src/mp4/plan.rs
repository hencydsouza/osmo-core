//! Pure trim planning: decide which samples of each track survive a `from..to` cut.

use super::{Movie, Mp4Error, Track};

/// Samples `first..end` (end exclusive) of `movie.tracks[track]` survive the cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCut {
    pub track: usize,
    pub first: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrimPlan {
    pub cuts: Vec<TrackCut>,
    /// Where the cut really begins: the sync sample at or before the requested start.
    pub actual_start_secs: f64,
    /// Real length of the saved clip (the end is clamped to the clip's end).
    pub duration_secs: f64,
}

fn clip_secs(movie: &Movie) -> f64 {
    movie
        .tracks
        .iter()
        .find(|t| &t.handler == b"vide" && !t.samples.is_empty())
        .map_or(0.0, |t| t.duration as f64 / f64::from(t.timescale.max(1)))
}

fn bad_range(movie: &Movie, from: f64, to: f64) -> Mp4Error {
    Mp4Error::BadRange(format!(
        "{from}..{to} s is not inside a clip that is {:.3} s long",
        clip_secs(movie)
    ))
}

/// Index range of `t`'s samples whose dts fall in `start_ticks..end_ticks`.
fn span(t: &Track, start_ticks: u64, end_ticks: u64) -> (usize, usize) {
    let first = t.samples.partition_point(|s| s.dts < start_ticks);
    let end = t.samples.partition_point(|s| s.dts < end_ticks).max(first);
    (first, end)
}

pub fn plan_trim(movie: &Movie, from_secs: f64, to_secs: f64) -> Result<TrimPlan, Mp4Error> {
    let bad = || bad_range(movie, from_secs, to_secs);
    if !from_secs.is_finite() || !to_secs.is_finite() || from_secs < 0.0 || from_secs >= to_secs {
        return Err(bad());
    }
    let vi = movie
        .tracks
        .iter()
        .position(|t| &t.handler == b"vide" && !t.samples.is_empty())
        .ok_or(Mp4Error::Unsupported("no video track with samples"))?;
    let video = &movie.tracks[vi];
    let scale = f64::from(video.timescale.max(1));
    if from_secs >= video.duration as f64 / scale {
        return Err(bad());
    }

    // Start: the last sync sample at or before `from` (floor, so a boundary tick is never missed).
    let from_ticks = (from_secs * scale).floor() as u64;
    let at_or_before = video.samples.partition_point(|s| s.dts <= from_ticks);
    let mut start = at_or_before.saturating_sub(1);
    while start > 0 && !video.samples[start].sync {
        start -= 1;
    }
    let start_dts = video.samples[start].dts;

    // End: the first sample at or after `to` (ceil), clamped to the clip's real end.
    let to_ticks = (to_secs * scale).ceil() as u64;
    let end = video.samples.partition_point(|s| s.dts < to_ticks).max(start + 1);
    let end_clamped = end == video.samples.len();
    let end_dts = video.samples.get(end).map_or(video.duration, |s| s.dts);

    let actual_start_secs = start_dts as f64 / scale;
    let end_secs = end_dts as f64 / scale;
    let mut cuts = vec![TrackCut { track: vi, first: start, end }];

    for (i, t) in movie.tracks.iter().enumerate().filter(|(i, _)| *i != vi) {
        match t.samples.len() {
            0 => {}
            // A lone sample (the timecode track) describes the whole clip, so it is kept whole.
            1 => cuts.push(TrackCut { track: i, first: 0, end: 1 }),
            _ => {
                let ts = f64::from(t.timescale.max(1));
                let (first, mut end) = span(
                    t,
                    (actual_start_secs * ts).round() as u64,
                    (end_secs * ts).round() as u64,
                );
                // At the clip's end keep every remaining sample: audio often outlasts the video.
                if end_clamped {
                    end = t.samples.len();
                }
                cuts.push(TrackCut { track: i, first, end });
            }
        }
    }
    cuts.sort_by_key(|c| c.track);
    Ok(TrimPlan { cuts, actual_start_secs, duration_secs: end_secs - actual_start_secs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::Sample;

    fn track(handler: &[u8; 4], timescale: u32, duration: u64, dts: &[(u64, u32, bool)]) -> Track {
        Track {
            id: 1,
            handler: *handler,
            timescale,
            duration,
            samples: dts
                .iter()
                .map(|&(dts, duration, sync)| Sample {
                    offset: 0,
                    size: 0,
                    dts,
                    duration,
                    cts_offset: 0,
                    sync,
                })
                .collect(),
            raw_stsd: vec![],
            raw_hdlr: vec![],
            raw_dinf: vec![],
            raw_media_header: vec![],
            raw_tkhd: vec![],
            raw_tref: vec![],
        }
    }

    fn movie(tracks: Vec<Track>) -> Movie {
        Movie { movie_timescale: 1000, tracks, ftyp: vec![] }
    }

    /// 7 video samples at 10 ticks/s (0.7 s); sync at dts 0, 3, 6.
    fn video() -> Track {
        let s = [(0, 1, true), (1, 1, false), (2, 1, false), (3, 1, true), (4, 1, false), (5, 1, false), (6, 1, true)];
        track(b"vide", 10, 7, &s)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn start_snaps_back_to_the_previous_sync_sample() {
        let plan = plan_trim(&movie(vec![video()]), 0.5, 0.6).unwrap();
        assert_eq!(plan.cuts, vec![TrackCut { track: 0, first: 3, end: 6 }]);
        assert!(close(plan.actual_start_secs, 0.3));
        assert!(close(plan.duration_secs, 0.3));
    }

    #[test]
    fn end_is_exclusive_and_clamps_to_clip_end_reporting_real_duration() {
        let plan = plan_trim(&movie(vec![video()]), 0.0, 5.0).unwrap();
        assert_eq!(plan.cuts, vec![TrackCut { track: 0, first: 0, end: 7 }]);
        assert!(close(plan.actual_start_secs, 0.0));
        assert!(close(plan.duration_secs, 0.7));
    }

    #[test]
    fn audio_and_data_tracks_cut_at_the_same_instant() {
        // Audio at 20 ticks/s, twice as dense as video.
        let audio_samples: Vec<(u64, u32, bool)> = (0..14).map(|i| (i, 1, true)).collect();
        let audio = track(b"soun", 20, 14, &audio_samples);
        // Data track at the video rate, one sample per frame.
        let data_samples: Vec<(u64, u32, bool)> = (0..7).map(|i| (i, 1, true)).collect();
        let data = track(b"meta", 10, 7, &data_samples);
        let plan = plan_trim(&movie(vec![video(), audio, data]), 0.3, 0.6).unwrap();
        assert_eq!(
            plan.cuts,
            vec![
                TrackCut { track: 0, first: 3, end: 6 },
                TrackCut { track: 1, first: 6, end: 12 },
                TrackCut { track: 2, first: 3, end: 6 },
            ]
        );
    }

    #[test]
    fn audio_outlasting_the_video_is_kept_to_the_end_when_the_cut_reaches_it() {
        let audio_samples: Vec<(u64, u32, bool)> = (0..15).map(|i| (i, 1, true)).collect();
        let audio = track(b"soun", 20, 15, &audio_samples);
        let plan = plan_trim(&movie(vec![video(), audio]), 0.0, 5.0).unwrap();
        assert_eq!(plan.cuts[1], TrackCut { track: 1, first: 0, end: 15 });
    }

    #[test]
    fn single_sample_timecode_track_is_kept_whole() {
        let tmcd = track(b"tmcd", 1, 1, &[(0, 999, true)]);
        let plan = plan_trim(&movie(vec![video(), tmcd]), 0.3, 0.6).unwrap();
        assert!(close(plan.actual_start_secs, 0.3));
        assert_eq!(plan.cuts[1], TrackCut { track: 1, first: 0, end: 1 });
    }

    #[test]
    fn empty_tracks_are_dropped() {
        let empty = track(b"meta", 10, 0, &[]);
        let plan = plan_trim(&movie(vec![video(), empty]), 0.0, 0.5).unwrap();
        assert!(plan.cuts.iter().all(|c| c.track != 1));
        assert_eq!(plan.cuts.len(), 1);
    }

    #[test]
    fn bad_ranges_name_the_clip_length() {
        let m = movie(vec![video()]);
        let cases = [
            (-1.0, 0.5),
            (f64::NAN, 0.5),
            (0.5, 0.5),
            (0.9, 0.3),
            (0.7, 0.8),
            (0.2, f64::INFINITY),
        ];
        for (from, to) in cases {
            match plan_trim(&m, from, to) {
                Err(Mp4Error::BadRange(msg)) => {
                    assert!(msg.contains("0.700"), "{from}..{to}: message lacks clip length: {msg}")
                }
                other => panic!("{from}..{to}: expected BadRange, got {other:?}"),
            }
        }
    }
}
