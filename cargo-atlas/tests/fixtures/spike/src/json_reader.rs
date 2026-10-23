#[derive(Debug, Clone)]
pub struct JsonReader {
    path: String,
}

impl JsonReader {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_string() }
    }
    pub fn parse(&self) -> Vec<String> {
        vec![format!("json:{}", self.path)]
    }
}
