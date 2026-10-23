//! A macro whose body calls a function: the call is written in the
//! `macro_rules!` body, not where the macro is used.

pub fn write_line(message: &str) -> usize {
    message.len()
}

macro_rules! log_err {
    ($e:expr) => {
        $crate::log::write_line(&format!("error: {}", $e))
    };
}

pub fn report(code: u32) -> usize {
    log_err!(code)
}

#[cfg(test)]
mod tests {
    #[test]
    fn report_writes_a_line() {
        assert!(super::report(7) > 0);
    }
}
