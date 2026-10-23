pub fn connect(port: u16) -> bool {
    port > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn connects() {
        assert!(super::connect(1));
    }
}
