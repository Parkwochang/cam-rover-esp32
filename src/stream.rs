use std::time::Duration;

pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const SESSION_LIMIT: Duration = Duration::from_secs(30);
pub const FRAME_INTERVAL: Duration = Duration::from_millis(67);

pub fn valid_jpeg(bytes: &[u8]) -> bool {
    (4..=MAX_FRAME_BYTES).contains(&bytes.len())
        && bytes.starts_with(&[0xff, 0xd8])
        && bytes.ends_with(&[0xff, 0xd9])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_partial_and_oversized_frames() {
        assert!(valid_jpeg(&[0xff, 0xd8, 0xff, 0xd9]));
        assert!(!valid_jpeg(&[]));
        assert!(!valid_jpeg(&[0xff, 0xd8, 0, 0]));
        let mut large = vec![0; MAX_FRAME_BYTES + 1];
        large[..2].copy_from_slice(&[0xff, 0xd8]);
        large[MAX_FRAME_BYTES - 1..].copy_from_slice(&[0xff, 0xd9]);
        assert!(!valid_jpeg(&large));
    }
}
