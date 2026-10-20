use std::{hint::black_box, time::Instant};
use truepeak::true_peak;

fn main() {
    let rate = 48000;
    let seconds = 10;
    let input: Vec<f32> = (0..rate * seconds)
        .map(|n| (0.7 * (std::f64::consts::TAU * 997.0 * n as f64 / rate as f64).sin()) as f32)
        .collect();
    let start = Instant::now();
    let result = true_peak(black_box(&[&input, &input]), rate, 8).unwrap();
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "8x meter, {seconds} s / 48 kHz / stereo: {elapsed:.3} s ({:.1}x realtime), {:.6} dBTP",
        seconds as f64 / elapsed,
        result.dbtp
    );
}
