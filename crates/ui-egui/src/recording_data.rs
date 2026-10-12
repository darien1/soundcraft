//! Incremental, UI-owned recording overview. No audio-thread work or session mutations.
//! Kept independent of the UI so its peak and timeline math can also be tested with rustc.

const BLOCK: usize = 128;
const FANOUT: usize = 4;
const EMPTY: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

fn merge(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0.min(b.0), a.1.max(b.1))
}

#[derive(Default)]
pub(crate) struct Envelope {
    frames: usize,
    levels: Vec<Vec<(f32, f32)>>,
}

impl Envelope {
    fn extend(&mut self, samples: impl Iterator<Item = f32>) {
        let mut dirty = self.frames / BLOCK;
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        for sample in samples {
            if self.frames.is_multiple_of(BLOCK) {
                if let Some(level) = self.levels.first_mut() {
                    level.push(EMPTY);
                }
            }
            if sample.is_finite()
                && let Some(bin) = self.levels.first_mut().and_then(|level| level.last_mut())
            {
                *bin = merge(*bin, (sample, sample));
            }
            self.frames = self.frames.saturating_add(1);
        }
        let mut level = 0;
        while self.levels.get(level).is_some_and(|bins| bins.len() > 1) {
            let n = self.levels.get(level).map_or(0, |bins| bins.len().div_ceil(FANOUT));
            if self.levels.len() <= level + 1 {
                self.levels.push(Vec::new());
            }
            dirty /= FANOUT;
            for parent in dirty..n {
                let peak = self.levels.get(level).map_or(EMPTY, |bins| {
                    let from = parent.saturating_mul(FANOUT);
                    bins.get(from..from.saturating_add(FANOUT).min(bins.len())).unwrap_or_default().iter().copied().fold(EMPTY, merge)
                });
                if let Some(parents) = self.levels.get_mut(level + 1) {
                    if let Some(bin) = parents.get_mut(parent) {
                        *bin = peak;
                    } else {
                        parents.push(peak);
                    }
                }
            }
            level += 1;
        }
    }

    /// Conservative min/max over the 128-frame bins touching this range.
    pub(crate) fn minmax(&self, start: usize, end: usize) -> (f32, f32) {
        let end = end.min(self.frames);
        if start >= end {
            return (0.0, 0.0);
        }
        let mut lo = start / BLOCK;
        let mut hi = end.div_ceil(BLOCK);
        let mut acc = EMPTY;
        let mut level = 0;
        while lo < hi {
            let Some(bins) = self.levels.get(level) else { break };
            if level + 1 >= self.levels.len() || hi - lo < 2 * FANOUT {
                for bin in bins.get(lo..hi.min(bins.len())).unwrap_or_default() {
                    acc = merge(acc, *bin);
                }
                break;
            }
            while !lo.is_multiple_of(FANOUT) && lo < hi {
                acc = merge(acc, bins.get(lo).copied().unwrap_or(EMPTY));
                lo += 1;
            }
            while !hi.is_multiple_of(FANOUT) && hi > lo {
                hi -= 1;
                acc = merge(acc, bins.get(hi).copied().unwrap_or(EMPTY));
            }
            lo /= FANOUT;
            hi /= FANOUT;
            level += 1;
        }
        if acc.0 > acc.1 { (0.0, 0.0) } else { acc }
    }
}

#[derive(Default)]
pub(crate) struct LiveWaveform {
    pub(crate) rate: u32,
    pub(crate) frames: usize,
    pub(crate) channels: Vec<Envelope>,
}

impl LiveWaveform {
    pub(crate) fn append(&mut self, data: &[f32], channels: usize, rate: u32) {
        let channels = channels.max(1);
        if channels > 65_536 || rate == 0 {
            return;
        }
        if self.channels.len() != channels || self.rate != rate {
            *self = Self { rate, frames: 0, channels: (0..channels).map(|_| Envelope::default()).collect() };
        }
        for (ch, envelope) in self.channels.iter_mut().enumerate() {
            envelope.extend(data.chunks_exact(channels).filter_map(|frame| frame.get(ch).copied()));
        }
        self.frames = self.frames.saturating_add(data.len() / channels);
    }
}

pub(crate) struct PreviewSpan {
    pub(crate) start: i64,
    pub(crate) end: i64,
    origin: i64,
    input_offset: usize,
    ratio: f64,
}

impl PreviewSpan {
    pub(crate) fn input_at(&self, at: i64) -> f64 {
        self.input_offset as f64 + at.saturating_sub(self.origin) as f64 * self.ratio
    }
}

/// Map capture frames to the session timeline, including punch bounds and the current loop pass.
pub(crate) fn preview_span(
    start: i64,
    frames: usize,
    input_rate: u32,
    session_rate: f64,
    punch: Option<(i64, i64)>,
    loop_range: Option<(i64, i64)>,
) -> Option<PreviewSpan> {
    if frames == 0 || input_rate == 0 || !session_rate.is_finite() || session_rate <= 0.0 {
        return None;
    }
    let ratio = f64::from(input_rate) / session_rate;
    let (origin, offset) = if let Some((a, b)) = loop_range.filter(|(a, b)| b > a) {
        let pass = (b.saturating_sub(a) as f64 * ratio).round().max(1.0) as usize;
        // At an exact boundary retain the completed pass until the next input frame arrives.
        (a, frames.saturating_sub(1) / pass * pass)
    } else {
        (start, 0)
    };
    let mut end = origin.saturating_add((frames.saturating_sub(offset) as f64 / ratio).round() as i64);
    let mut visible_start = origin;
    if let Some((a, b)) = punch {
        visible_start = visible_start.max(a);
        end = end.min(b);
    }
    if let Some((_, b)) = loop_range {
        end = end.min(b);
    }
    (end > visible_start).then_some(PreviewSpan { start: visible_start, end, origin, input_offset: offset, ratio })
}

pub(crate) fn first_input_channel(route: &str) -> usize {
    route.trim_start_matches("In ").split(['-', ' ']).next().and_then(|n| n.parse::<usize>().ok()).map_or(0, |n| n.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.037).sin()).collect()
    }

    #[test]
    fn incremental_overview_matches_whole_capture_at_every_chunk_boundary() {
        let data = signal(100_003);
        for size in [1, 31, 128, 517, 65_536] {
            let mut envelope = Envelope::default();
            let mut count = 0;
            for chunk in data.chunks(size) {
                envelope.extend(chunk.iter().copied());
                count += chunk.len();
                // The whole-capture range and the last partial bin are exact.
                for a in [0, count / BLOCK * BLOCK] {
                    let truth = data.get(a..count).unwrap_or_default().iter().copied().fold(EMPTY, |acc, s| merge(acc, (s, s)));
                    let truth = if truth.0 > truth.1 { (0.0, 0.0) } else { truth };
                    assert_eq!(envelope.minmax(a, count), truth, "chunk {size}, range {a}..{count}");
                }
            }
        }
    }

    #[test]
    fn range_queries_match_the_covering_bins_at_all_tree_depths() {
        let data = signal(100_003);
        let mut envelope = Envelope::default();
        envelope.extend(data.iter().copied());
        for a in (0..data.len()).step_by(997) {
            for length in [1, 129, 1024, 23_000, 200_000] {
                let b = (a + length).min(data.len());
                let truth = data[a / BLOCK * BLOCK..(b.div_ceil(BLOCK) * BLOCK).min(data.len())]
                    .iter().copied().fold(EMPTY, |acc, s| merge(acc, (s, s)));
                assert_eq!(envelope.minmax(a, b), truth);
            }
        }
        assert!(envelope.levels.iter().map(Vec::len).sum::<usize>() < data.len() / 80);
    }

    #[test]
    fn stereo_grows_without_mixing_channels_and_restarts_cleanly() {
        let mut preview = LiveWaveform::default();
        preview.append(&[-0.7, 0.2, 0.8, 0.3], 2, 48_000);
        preview.append(&[0.1, -0.4], 2, 48_000);
        assert_eq!(preview.frames, 3);
        assert_eq!(preview.channels[0].minmax(0, 3), (-0.7, 0.8));
        assert_eq!(preview.channels[1].minmax(0, 3), (-0.4, 0.3));
        preview = LiveWaveform::default();
        preview.append(&[0.5], 1, 44_100);
        assert_eq!(preview.frames, 1);
        assert_eq!(preview.rate, 44_100);
        assert_eq!(preview.channels[0].minmax(0, 1), (0.5, 0.5));
    }

    #[test]
    fn silence_and_nonfinite_input_are_safe() {
        let mut envelope = Envelope::default();
        assert_eq!(envelope.minmax(0, 1), (0.0, 0.0));
        envelope.extend([f32::NAN, f32::INFINITY, f32::NEG_INFINITY].into_iter());
        assert_eq!(envelope.minmax(0, 3), (0.0, 0.0));
        envelope.extend([-0.5, 0.25].into_iter());
        assert_eq!(envelope.minmax(0, 5), (-0.5, 0.25));
        assert_eq!(envelope.minmax(5, usize::MAX), (0.0, 0.0));
    }

    #[test]
    fn timeline_converts_input_rate_and_clips_preroll_and_postroll() {
        let span = preview_span(48_000, 44_100 * 3, 44_100, 48_000.0, Some((96_000, 144_000)), None).unwrap();
        assert_eq!((span.start, span.end), (96_000, 144_000));
        assert_eq!(span.input_at(span.start), 44_100.0);
        assert_eq!(span.input_at(span.end), 88_200.0);
        assert!(preview_span(48_000, 1000, 44_100, 48_000.0, Some((96_000, 144_000)), None).is_none());
    }

    #[test]
    fn loop_preview_uses_current_pass_and_keeps_exact_boundary_visible() {
        let boundary = preview_span(100, 48_000, 48_000, 48_000.0, None, Some((100, 48_100))).unwrap();
        assert_eq!((boundary.start, boundary.end), (100, 48_100));
        assert_eq!(boundary.input_at(100), 0.0);
        let next = preview_span(100, 60_000, 48_000, 48_000.0, None, Some((100, 48_100))).unwrap();
        assert_eq!((next.start, next.end), (100, 12_100));
        assert_eq!(next.input_at(100), 48_000.0);
        assert!(preview_span(0, 0, 48_000, 48_000.0, None, None).is_none());
        assert!(preview_span(0, 1, 0, 48_000.0, None, None).is_none());
        assert!(preview_span(0, 1, 48_000, f64::NAN, None, None).is_none());
    }

    #[test]
    fn hardware_input_routes_match_recorded_takes() {
        assert_eq!(first_input_channel("In 3-4"), 2);
        assert_eq!(first_input_channel("In 2"), 1);
        assert_eq!(first_input_channel("In 0"), 0);
        assert_eq!(first_input_channel("Input"), 0);
    }
}
