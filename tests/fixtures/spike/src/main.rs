mod csv_reader;
mod json_reader;
mod loader;

use csv_reader::CsvReader;
use json_reader::JsonReader;
use loader::Loader;

fn run(l: &dyn Loader) -> usize {
    l.load().len()
}

fn main() {
    let r = JsonReader::new("data.json");
    let rows = r.parse();
    let c = CsvReader::new("data.csv");
    println!("{} {}", rows.len(), run(&c));
}
