use std::hint::black_box;
use std::time::Instant;

fn main() {
    let start = Instant::now();
    let mut acc: u64 = 0;
    for _ in 0..300000 {
        acc = black_box(acc + 1);
    }
    let elapsed = start.elapsed();
    println!("elapsed = {} seconds (acc={})", elapsed.as_secs_f64(), acc);
}
