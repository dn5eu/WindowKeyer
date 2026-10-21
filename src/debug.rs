use std::sync::atomic::AtomicBool;

pub static DEBUG: AtomicBool = AtomicBool::new(false);

#[macro_export]
macro_rules! debugln {
	($($arg:tt)*) => {
		if $crate::debug::DEBUG.load(std::sync::atomic::Ordering::Relaxed) {
			eprintln!($($arg)*);
		}
	};
}
