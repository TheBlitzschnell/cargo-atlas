pub mod shapes;
pub mod util;

pub use shapes::Area;
pub use util::helper as renamed_helper;

pub mod inline {
    pub mod deeper {
        pub fn deep_fn() -> u32 {
            42
        }
    }

    pub fn calls_deep() -> u32 {
        deeper::deep_fn()
    }
}

macro_rules! twice {
    ($e:expr) => {
        $e + $e
    };
}

pub fn uses_macro() -> u32 {
    twice!(inline::deeper::deep_fn())
}
