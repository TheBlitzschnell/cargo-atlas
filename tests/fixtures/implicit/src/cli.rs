//! A module of the binary, not the library: its tests run with `--bin implicit`.

use implicit::money::{Money, parse_money, total};

pub fn run(args: &[&str]) -> Money {
    args.iter()
        .filter_map(|a| parse_money(a).ok())
        .fold(Money(0), total)
}

#[cfg(test)]
mod tests {
    #[test]
    fn runs_the_arguments() {
        assert_eq!(super::run(&["1", "2"]).0, 3);
    }
}
