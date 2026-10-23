use edge_core::renamed_helper;
use edge_core::shapes::{Area, Circle, Square, Wrapper, total_dyn};

fn main() {
    let s = Square(2.0);
    let w = Wrapper(Circle { r: 1.0 });
    let shapes: Vec<Box<dyn Area>> = vec![Box::new(s), Box::new(Circle { r: 2.0 })];
    println!("{}", total_dyn(&shapes));
    println!("{}", w.area());
    println!("{}", renamed_helper(1));
    helper_local();
}

fn helper_local() {}

#[cfg(test)]
mod tests {
    #[test]
    fn calls_helper_local() {
        super::helper_local();
    }
}
