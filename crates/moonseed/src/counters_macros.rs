// The off expansion does not evaluate arguments or resolve measurement names.
#[cfg(feature = "counters")]
macro_rules! count {
    ($name:expr) => {
        crate::counters::event($name, 1)
    };
    ($name:expr, $value:expr) => {
        crate::counters::event($name, $value as u64)
    };
}
#[cfg(not(feature = "counters"))]
macro_rules! count {
    ($($tokens:tt)*) => {};
}
