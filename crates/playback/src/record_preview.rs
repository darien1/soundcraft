//! Bounded, nonblocking and nondestructive access to pending capture samples.

use std::sync::Mutex;

pub(crate) fn read(data: &Mutex<Vec<f32>>, channels: usize, from_frame: usize, out: &mut Vec<f32>) -> bool {
    const MAX_PREVIEW_SAMPLES: usize = 65_536;
    out.clear();
    let ch = channels.max(1);
    if ch > MAX_PREVIEW_SAMPLES {
        return false;
    }
    let limit = MAX_PREVIEW_SAMPLES / ch * ch;
    out.reserve(limit);
    let Ok(data) = data.try_lock() else { return false };
    let start = from_frame.saturating_mul(ch).min(data.len());
    let available = data.len().saturating_sub(start) / ch * ch;
    let end = start.saturating_add(available.min(limit));
    if let Some(block) = data.get(start..end) {
        out.extend_from_slice(block);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_incremental_nondestructive_and_skips_contention() {
        let data = Mutex::new(vec![1.0, 2.0, 3.0, 4.0]);
        let mut out = Vec::new();
        assert!(read(&data, 2, 1, &mut out));
        assert_eq!(out, vec![3.0, 4.0]);
        {
            let _guard = data.lock().unwrap();
            assert!(!read(&data, 2, 0, &mut out));
            assert!(out.is_empty());
        }
        assert_eq!(data.into_inner().unwrap(), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn preview_copy_has_a_fixed_bound_and_only_whole_frames() {
        let data = Mutex::new(vec![0.5; 100_001]);
        let mut out = Vec::new();
        assert!(read(&data, 3, 0, &mut out));
        assert_eq!(out.len(), 65_535);
        assert!(read(&data, 3, 21_845, &mut out));
        assert_eq!(out.len(), 34_464);
        assert!(read(&data, 3, usize::MAX, &mut out));
        assert!(out.is_empty());
        assert!(!read(&data, usize::MAX, 0, &mut out));
        assert!(out.is_empty());
    }
}
