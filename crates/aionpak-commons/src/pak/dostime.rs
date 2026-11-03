use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// MS-DOS date and time as stored in zip headers, two-second resolution. Times are treated as UTC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DosTime {
	pub date: u16,
	pub time: u16,
}

const DOS_EPOCH_YEAR: i64 = 1980;

impl DosTime {
	pub fn from_system(t: SystemTime) -> Self {
		let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
		let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
		let (y, m, d) = civil_from_days(days);
		if y < DOS_EPOCH_YEAR {
			return DosTime {
				date: (1 << 5) | 1,
				time: 0,
			};
		}
		let y = y.min(DOS_EPOCH_YEAR + 127);
		DosTime {
			date: (((y - DOS_EPOCH_YEAR) << 9) | (m << 5) | d) as u16,
			time: (((rem / 3600) << 11) | ((rem / 60 % 60) << 5) | (rem % 60 / 2)) as u16,
		}
	}

	pub fn to_system(self) -> SystemTime {
		let y = DOS_EPOCH_YEAR + (self.date >> 9) as i64;
		let m = ((self.date >> 5) & 0xf).clamp(1, 12) as i64;
		let d = (self.date & 0x1f).max(1) as i64;
		let secs = days_from_civil(y, m, d) * 86_400
			+ (self.time >> 11) as i64 * 3600
			+ ((self.time >> 5) & 0x3f) as i64 * 60
			+ (self.time & 0x1f) as i64 * 2;
		UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
	}
}

pub fn now() -> DosTime {
	DosTime::from_system(SystemTime::now())
}

// Howard Hinnant's civil calendar algorithms.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
	let z = z + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = doy - (153 * mp + 2) / 5 + 1;
	let m = if mp < 10 { mp + 3 } else { mp - 9 };
	(yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
	let y = if m <= 2 { y - 1 } else { y };
	let era = y.div_euclid(400);
	let yoe = y.rem_euclid(400);
	let mp = if m > 2 { m - 3 } else { m + 9 };
	let doy = (153 * mp + 2) / 5 + d - 1;
	let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let t = UNIX_EPOCH + Duration::from_secs(1_450_000_000);
		assert_eq!(DosTime::from_system(t).to_system(), t);
		let dos = DosTime {
			date: 0x4787,
			time: 0x6000,
		};
		assert_eq!(DosTime::from_system(dos.to_system()), dos);
	}
}
