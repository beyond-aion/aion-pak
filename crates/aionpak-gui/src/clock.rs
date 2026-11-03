//! Local wall-clock time for the log. The OS already knows the offset and the daylight rule, so this asks it
//! instead of carrying a calendar library.

use std::fmt;

#[derive(Clone, Copy)]
pub struct Stamp {
	year: i32,
	month: u32,
	day: u32,
	hour: u32,
	minute: u32,
	second: u32,
	millis: u32,
}

impl Stamp {
	pub fn now() -> Self {
		#[cfg(windows)]
		let stamp = windows_now();
		#[cfg(unix)]
		let stamp = unix_now();
		#[cfg(not(any(windows, unix)))]
		let stamp = utc_now();
		stamp
	}
}

impl fmt::Display for Stamp {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
			self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
		)
	}
}

#[cfg(windows)]
fn windows_now() -> Stamp {
	use windows_sys::Win32::Foundation::SYSTEMTIME;
	use windows_sys::Win32::System::SystemInformation::GetLocalTime;

	let mut time = SYSTEMTIME {
		wYear: 0,
		wMonth: 0,
		wDayOfWeek: 0,
		wDay: 0,
		wHour: 0,
		wMinute: 0,
		wSecond: 0,
		wMilliseconds: 0,
	};
	// GetLocalTime cannot fail; it is the documented way to read the local wall clock.
	unsafe { GetLocalTime(&mut time) };
	Stamp {
		year: time.wYear as i32,
		month: time.wMonth as u32,
		day: time.wDay as u32,
		hour: time.wHour as u32,
		minute: time.wMinute as u32,
		second: time.wSecond as u32,
		millis: time.wMilliseconds as u32,
	}
}

#[cfg(unix)]
fn unix_now() -> Stamp {
	let millis = subsec_millis();
	let seconds = unsafe { libc::time(std::ptr::null_mut()) };
	let mut broken = std::mem::MaybeUninit::<libc::tm>::uninit();
	if unsafe { libc::localtime_r(&seconds, broken.as_mut_ptr()) }.is_null() {
		return from_unix(seconds as i64, millis);
	}
	let broken = unsafe { broken.assume_init() };
	Stamp {
		year: broken.tm_year + 1900,
		month: (broken.tm_mon + 1) as u32,
		day: broken.tm_mday as u32,
		hour: broken.tm_hour as u32,
		minute: broken.tm_min as u32,
		second: broken.tm_sec as u32,
		millis,
	}
}

#[cfg(not(any(windows, unix)))]
fn utc_now() -> Stamp {
	let millis = subsec_millis();
	let seconds = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|elapsed| elapsed.as_secs() as i64)
		.unwrap_or(0);
	from_unix(seconds, millis)
}

#[cfg(not(windows))]
fn from_unix(seconds: i64, millis: u32) -> Stamp {
	let days = seconds.div_euclid(86_400);
	let rest = seconds.rem_euclid(86_400);
	let (year, month, day) = civil_from_days(days);
	Stamp {
		year,
		month,
		day,
		hour: (rest / 3_600) as u32,
		minute: ((rest % 3_600) / 60) as u32,
		second: (rest % 60) as u32,
		millis,
	}
}

/// Howard Hinnant's `civil_from_days`: 1970-01-01 is day zero.
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i32, u32, u32) {
	let shifted = days + 719_468;
	let era = shifted.div_euclid(146_097);
	let day_of_era = shifted.rem_euclid(146_097);
	let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let year = year_of_era + era * 400;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_part = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_part + 2) / 5 + 1;
	let month = if month_part < 10 {
		month_part + 3
	} else {
		month_part - 9
	};
	let year = year + i64::from(month <= 2);
	(year as i32, month as u32, day as u32)
}

#[cfg(not(windows))]
fn subsec_millis() -> u32 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|elapsed| elapsed.subsec_millis())
		.unwrap_or(0)
}
