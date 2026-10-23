//! A static and a const whose initializers call functions.

use std::sync::LazyLock;

pub struct Config {
    pub port: u16,
}

pub const fn default_port() -> u16 {
    8080
}

pub const DEFAULT_PORT: u16 = default_port();

pub fn load_config() -> Config {
    Config { port: DEFAULT_PORT }
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| load_config());

pub fn port() -> u16 {
    CONFIG.port
}

#[cfg(test)]
mod tests {
    #[test]
    fn port_comes_from_the_config() {
        assert_eq!(super::port(), 8080);
    }
}
