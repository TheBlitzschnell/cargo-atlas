pub trait Area {
    fn area(&self) -> f64;

    fn describe(&self) -> String {
        format!("area {}", self.area())
    }
}

/// Associated consts make a trait unusable as `dyn`, so they live apart from `Area`.
pub trait Sides {
    const SIDES: u32;
}

pub struct Square(pub f64);

pub struct Circle {
    pub r: f64,
}

impl Area for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}

impl Sides for Square {
    const SIDES: u32 = 4;
}

impl Area for Circle {
    fn area(&self) -> f64 {
        3.14 * self.r * self.r
    }
}

pub struct Wrapper<T>(pub T);

impl<T: Area> Area for Wrapper<T> {
    fn area(&self) -> f64 {
        self.0.area()
    }
}

impl<T: Sides> Sides for Wrapper<T> {
    const SIDES: u32 = T::SIDES;
}

pub trait Named {
    fn name(&self) -> String;
}

impl<T: Area> Named for T {
    fn name(&self) -> String {
        self.describe()
    }
}

pub fn total_dyn(shapes: &[Box<dyn Area>]) -> f64 {
    shapes.iter().map(|s| s.area()).sum()
}

pub fn total_generic<A: Area>(shapes: &[A]) -> f64 {
    shapes.iter().map(Area::area).sum()
}

pub fn square_area(s: &Square) -> f64 {
    s.area()
}
