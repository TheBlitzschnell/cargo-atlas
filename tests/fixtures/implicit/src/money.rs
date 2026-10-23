//! Operators and `?` call trait methods without naming them.

use std::fmt;
use std::num::ParseIntError;
use std::ops::{Add, Index};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Money(pub i64);

impl Add for Money {
    type Output = Money;
    fn add(self, other: Money) -> Money {
        Money(self.0 + other.0)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} cents", self.0)
    }
}

#[derive(Debug)]
pub struct MoneyError(pub String);

impl From<ParseIntError> for MoneyError {
    fn from(e: ParseIntError) -> Self {
        MoneyError(e.to_string())
    }
}

pub fn parse_money(text: &str) -> Result<Money, MoneyError> {
    let cents: i64 = text.trim().parse()?;
    Ok(Money(cents))
}

pub fn total(a: Money, b: Money) -> Money {
    a + b
}

pub struct Ledger(pub Vec<Money>);

impl Index<usize> for Ledger {
    type Output = Money;
    fn index(&self, i: usize) -> &Money {
        &self.0[i]
    }
}

pub fn first_entry(ledger: Ledger) -> Money {
    ledger[0]
}

pub fn describe(m: Money) -> String {
    format!("{m}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cents() {
        assert_eq!(parse_money("12").unwrap(), Money(12));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_money("x").is_err());
    }

    #[test]
    fn describes_money() {
        assert_eq!(describe(Money(3)), "3 cents");
    }
}
