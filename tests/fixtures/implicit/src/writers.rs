//! Two traits named `Write`, and `Debug` both derived and written by hand.

use std::fmt;
use std::io;

#[derive(Debug)]
pub struct Text(pub String);

impl fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

pub struct Bytes(pub Vec<u8>);

impl io::Write for Bytes {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0.len())
    }
}

/// One line names both traits: `io::Write` in the bound, `fmt::Write` as the trait.
pub struct Both<W: io::Write>(pub W, pub String);

impl<W: io::Write> fmt::Write for Both<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.1.push_str(s);
        Ok(())
    }
}
