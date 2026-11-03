//! Sizes and durations the way people read them.

pub fn human_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit + 1 < UNITS.len() {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

pub fn human_time(seconds: f64) -> String {
	let seconds = seconds.max(0.0);
	// Short jobs keep a decimal: "0 s" for a quarter-second extraction reads like nothing happened.
	if seconds < 10.0 {
		return format!("{seconds:.1} s");
	}
	let seconds = seconds.round() as u64;
	if seconds < 60 {
		format!("{seconds} s")
	} else {
		format!("{}:{:02}", seconds / 60, seconds % 60)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn formats() {
		assert_eq!(human_bytes(512), "512 B");
		assert_eq!(human_bytes(1536), "1.5 KB");
		assert_eq!(human_time(0.26), "0.3 s");
		assert_eq!(human_time(59.6), "1:00");
		assert_eq!(human_time(125.0), "2:05");
	}
}
