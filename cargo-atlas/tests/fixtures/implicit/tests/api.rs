mod common;

use implicit::guard::Guard;
use implicit::money::{parse_money, total};

#[test]
fn api_parses_money() {
    let m = parse_money("5").unwrap();
    assert_eq!(total(m, common::cents(1)).0, 6);
}

#[test]
fn guard_is_released() {
    let _guard = Guard::new("x");
}
