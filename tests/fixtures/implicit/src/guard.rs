//! `Drop` runs when a value goes out of scope; no call names it.

pub struct Guard {
    pub name: String,
}

impl Guard {
    pub fn new(name: &str) -> Guard {
        Guard {
            name: name.to_string(),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.name.clear();
    }
}
