use edge_core::shapes::{Area, Circle, Square, square_area, total_dyn};

#[test]
fn square_area_is_four() {
    assert_eq!(square_area(&Square(2.0)), 4.0);
}

#[test]
fn total_of_two_shapes() {
    let shapes: Vec<Box<dyn Area>> = vec![Box::new(Square(1.0)), Box::new(Circle { r: 1.0 })];
    assert!(total_dyn(&shapes) > 4.0);
}
