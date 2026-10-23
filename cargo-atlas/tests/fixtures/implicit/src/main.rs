mod cli;

fn main() {
    let sum = cli::run(&["12", "30"]);
    println!("{}", sum.0);
}
