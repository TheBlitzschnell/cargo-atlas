//! Loaded through `#[path = "extra/renamed.rs"]` in `lib.rs`.

pub fn renamed_fn() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn renamed_works() {
        assert_eq!(super::renamed_fn(), 1);
    }
}
