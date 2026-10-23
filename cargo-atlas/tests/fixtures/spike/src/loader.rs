use crate::csv_reader::CsvReader;
use crate::json_reader::JsonReader;

pub trait Loader {
    fn load(&self) -> Vec<String>;
}

impl Loader for CsvReader {
    fn load(&self) -> Vec<String> { self.parse() }
}

impl Loader for JsonReader {
    fn load(&self) -> Vec<String> { self.parse() }
}
