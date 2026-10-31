//! Shared secret comparison for setup codes and legacy scanner credentials.
use subtle::ConstantTimeEq;

pub fn constant_time_eq(expected: &[u8], observed: &[u8]) -> bool {
    let mut equal = expected.len().ct_eq(&observed.len());
    for index in 0..expected.len().max(observed.len()) {
        equal &= expected
            .get(index)
            .copied()
            .unwrap_or(0)
            .ct_eq(&observed.get(index).copied().unwrap_or(0));
    }
    bool::from(equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_all_bytes_and_lengths() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"secret", b"secret"));
        for value in [
            b"secre".as_slice(),
            b"secretx",
            b"xecret",
            b"secrex",
            b"secret\0",
            b"",
        ] {
            assert!(!constant_time_eq(b"secret", value));
            assert!(!constant_time_eq(value, b"secret"));
        }
        assert!(!constant_time_eq(b"\0", b""));
    }
}
