use std::{hint::black_box, sync::atomic::AtomicBool, time::Instant};
use truepeak::{Config, Limiter, limit, true_peak};

fn main() {
    let rate = 48000_u32;
    let seconds = 240;
    let input: Vec<f32> = (0..rate as usize * seconds)
        .map(|n| {
            let t = n as f64 / rate as f64;
            (1.4 * (std::f64::consts::TAU * 997.0 * t).sin()
                * (0.8 + 0.2 * (std::f64::consts::TAU * 0.7 * t).sin())) as f32
        })
        .collect();
    let channels = [&input[..], &input[..]];
    let config = Config::default();
    let start = Instant::now();
    let peak = true_peak(black_box(&channels), rate, 8).unwrap();
    let meter = start.elapsed().as_secs_f64();
    println!(
        "4-minute 48 kHz stereo meter: {meter:.6} s, {:.3}x realtime, peak={:.8} dBTP",
        seconds as f64 / meter,
        peak.dbtp
    );
    let mut processor = Limiter::new(rate, 2, &config).unwrap();
    let mut output = vec![Vec::new(); 2];
    let start = Instant::now();
    for start in (0..input.len()).step_by(4096) {
        let end = (start + 4096).min(input.len());
        processor
            .process(&[&input[start..end], &input[start..end]], &mut output)
            .unwrap();
        for channel in &mut output {
            black_box(&*channel);
            channel.clear();
        }
    }
    processor.flush(&mut output).unwrap();
    let processing = start.elapsed().as_secs_f64();
    println!("4-minute 48 kHz stereo single streaming pass: {processing:.6} s");
    let start = Instant::now();
    let work = input.len() * channels.len();
    let mut stages: Vec<(u32, f64, Option<f64>, Option<f64>)> = Vec::new();
    let mut preceding = 0.0;
    let mut input_measurement = 0.0;
    let result = limit(
        black_box(&channels),
        rate,
        &config,
        &AtomicBool::new(false),
        &mut |p| {
            let now = start.elapsed().as_secs_f64();
            if p.pass == 0 {
                preceding = now;
                input_measurement = now;
                return;
            }
            if stages.last().is_none_or(|stage| stage.0 != p.pass) {
                stages.push((p.pass, preceding, None, None));
            }
            let stage = stages.last_mut().unwrap();
            if p.done_samples >= work && stage.2.is_none() {
                stage.2 = Some(now);
            }
            if p.done_samples == p.total_samples {
                stage.3 = Some(now);
                preceding = now;
            }
        },
    )
    .unwrap();
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "4-minute 48 kHz stereo limit: {elapsed:.6} s, passes={}, total/pass={:.6} s, excluding initial meter/pass={:.6} s, output={:.8} dBTP, met={}",
        result.passes,
        elapsed / result.passes as f64,
        (elapsed - input_measurement) / result.passes as f64,
        result.output_true_peak_dbtp,
        result.ceiling_met
    );
    for (pass, begin, processed, verified) in stages {
        println!(
            "pass {pass}: processing+telemetry={:.6} s, flush/alignment+verification={:.6} s",
            processed.unwrap() - begin,
            verified.unwrap() - processed.unwrap()
        );
    }
    println!(
        "Throughput targets: meter >=40x: {}; limit <6 s/pass including input scan: {}",
        seconds as f64 / meter >= 40.0,
        elapsed / (result.passes as f64) < 6.0
    );
}
