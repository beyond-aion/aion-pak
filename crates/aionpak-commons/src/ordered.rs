//! Work spread over the thread pool whose results are consumed in order, with a bounded amount in flight.

use rayon::Yield;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

/// Runs `work` on every item on the rayon pool and hands the results to `consume` in item order.
///
/// Items costing at least `big` start first, since one of them alone can outlast all the others: up to half the
/// pool's threads work on them at once. The rest start in order while the window holds no more than `window`: an
/// item counts with its cost while in work, then with `held` of its result until consumed (always at least one item
/// in work). A result is usually much smaller than its input, so finished results waiting behind a slow item take
/// little of the window and the pool keeps working. The caller helps with the work while it waits, so this may itself
/// run inside the pool.
pub fn run<T: Sync, R: Send, E>(
	items: &[T],
	cost: impl Fn(&T) -> u64,
	window: u64,
	big: u64,
	work: impl Fn(&T) -> R + Sync,
	held: impl Fn(&R) -> u64,
	mut consume: impl FnMut(usize, R) -> Result<(), E>,
) -> Result<(), E> {
	let n = items.len();
	let costs: Vec<u64> = items.iter().map(cost).collect();
	let is_big = |i: usize| costs[i] >= big;
	let bigs: Vec<usize> = (0..n).filter(|&i| is_big(i)).collect();
	let work = &work;
	let (tx, rx) = mpsc::channel();
	rayon::in_place_scope(|s| {
		let spawn = |i: usize| {
			let tx = tx.clone();
			s.spawn(move |_| {
				let _ = tx.send((i, work(&items[i])));
			});
		};
		let mut pending = HashMap::new();
		let (mut next_write, mut next_start, mut next_big) = (0, 0, 0);
		let big_slots = (rayon::current_num_threads() / 2).max(1);
		let (mut in_flight, mut bigs_in_flight) = (0, 0);
		while next_write < n {
			while next_big < bigs.len() && bigs_in_flight < big_slots {
				bigs_in_flight += 1;
				spawn(bigs[next_big]);
				next_big += 1;
			}
			while next_start < n && (is_big(next_start) || in_flight == 0 || in_flight + costs[next_start] <= window) {
				if !is_big(next_start) {
					in_flight += costs[next_start];
					spawn(next_start);
				}
				next_start += 1;
			}
			let mut finish = |i: usize, r: R| {
				let h = held(&r);
				if !is_big(i) {
					in_flight = in_flight - costs[i] + h;
				}
				pending.insert(i, (r, h));
			};
			let mut received = false;
			while let Ok((i, r)) = rx.try_recv() {
				finish(i, r);
				received = true;
			}
			if !received
				&& rayon::yield_now() != Some(Yield::Executed)
				&& let Ok((i, r)) = rx.recv_timeout(Duration::from_millis(1))
			{
				finish(i, r);
			}
			while let Some((r, h)) = pending.remove(&next_write) {
				consume(next_write, r)?;
				if is_big(next_write) {
					bigs_in_flight -= 1;
				} else {
					in_flight -= h;
				}
				next_write += 1;
			}
		}
		Ok(())
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keeps_order_and_bounds() {
		let items: Vec<u64> = (0..2000).map(|i| if i % 97 == 0 { 50 } else { i % 7 }).collect();
		let mut seen = Vec::new();
		run(
			&items,
			|&c| c,
			20,
			40,
			|&c| c * 2,
			|&r| r / 4,
			|i, r| {
				assert_eq!(r, items[i] * 2);
				seen.push(i);
				Ok::<_, ()>(())
			},
		)
		.unwrap();
		assert_eq!(seen, (0..items.len()).collect::<Vec<_>>());
	}

	#[test]
	fn stops_on_error() {
		let items = vec![1u64; 100];
		let r = run(
			&items,
			|&c| c,
			4,
			10,
			|&c| c,
			|_| 0,
			|i, _| if i == 10 { Err(i) } else { Ok(()) },
		);
		assert_eq!(r, Err(10));
	}
}
