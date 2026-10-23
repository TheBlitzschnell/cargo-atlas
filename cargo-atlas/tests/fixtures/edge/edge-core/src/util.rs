pub fn helper(x: u32) -> u32 {
    x + 1
}

pub fn café_naïve() -> u32 {
    let s = "日本語 🦀"; helper(s.len() as u32)
}

pub fn outer() -> u32 {
    fn inner() -> u32 {
        helper(1)
    }
    inner() + outer_rec(3)
}

pub fn outer_rec(n: u32) -> u32 {
    if n == 0 { 0 } else { outer_rec(n - 1) }
}

/// rust-analyzer turns on `cfg(miri)` unless told otherwise, which would hide this.
#[cfg(not(miri))]
pub fn not_under_miri() -> u32 {
    helper(2)
}

/// Only built with `--features extra`.
#[cfg(feature = "extra")]
pub fn only_with_extra() -> u32 {
    outer()
}
