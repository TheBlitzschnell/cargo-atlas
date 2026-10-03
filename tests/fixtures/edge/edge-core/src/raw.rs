//! Unsafe code, with and without the comments that explain it, and tests.

/// Reads the first byte without a bounds check.
///
/// # Safety
///
/// `bytes` must not be empty.
pub unsafe fn first_unchecked(bytes: &[u8]) -> u8 {
    // SAFETY: the caller promises `bytes` is not empty.
    unsafe { *bytes.get_unchecked(0) }
}

/// Reads the last byte without a bounds check. No safety section.
pub unsafe fn last_unchecked(bytes: &[u8]) -> u8 {
    unsafe { *bytes.get_unchecked(bytes.len() - 1) }
}

pub fn first_or_zero(bytes: &[u8]) -> u8 {
    // SAFETY: the second arm only runs when `bytes` is not empty.
    match bytes {
        [] => 0,
        [_, ..] => unsafe { first_unchecked(bytes) },
    }
}

pub fn last_or_zero(bytes: &[u8]) -> u8 {
    if bytes.is_empty() {
        return 0;
    }
    unsafe { last_unchecked(bytes) }
}

pub struct RawBuf(*mut u8);

impl RawBuf {
    /// # Safety
    ///
    /// The buffer must hold at least one byte.
    pub unsafe fn peek(&self) -> u8 {
        // SAFETY: the caller promises the buffer is not empty.
        unsafe { *self.0 }
    }
}

// SAFETY: a RawBuf owns its pointer; nothing else can reach it.
unsafe impl Send for RawBuf {}

unsafe impl Sync for RawBuf {}

/// A type that hands out a raw pointer.
///
/// # Safety
///
/// The pointer must be valid for reads while `self` is alive.
pub unsafe trait Pointer {
    fn ptr(&self) -> *const u8;
}

// SAFETY: the pointer belongs to this RawBuf, which is alive while borrowed.
unsafe impl Pointer for RawBuf {
    fn ptr(&self) -> *const u8 {
        self.0
    }
}

pub fn safe_sum(bytes: &[u8]) -> u32 {
    bytes.iter().map(|&b| b as u32).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_byte() -> Vec<u8> {
        vec![1]
    }

    #[test]
    fn first_of_empty_is_zero() {
        assert_eq!(first_or_zero(&[]), 0);
    }

    #[test]
    fn sum_of_one_byte() {
        assert_eq!(safe_sum(&one_byte()), 1);
    }
}
