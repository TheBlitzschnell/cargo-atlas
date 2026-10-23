#[derive(Debug, Clone)]
pub struct CsvReader {
    path: String,
}

impl CsvReader {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_string() }
    }
    pub fn parse(&self) -> Vec<String> {
        vec![format!("csv:{}", self.path)]
    }
}
