mod common;

use std::f64::consts::TAU;
use truepeak::{Downsampler, Error, Upsampler};

fn upsample(input: &[f32], factor: u32, rate: u32, block: usize) -> Vec<f32> {
    let mut up = Upsampler::new(factor, rate).unwrap();
    let mut output = Vec::new();
    for chunk in input.chunks(block) {
        up.process(chunk, &mut output).unwrap();
    }
    up.flush(&mut output);
    output
}

fn downsample(input: &[f32], factor: u32, rate: u32, block: usize) -> Vec<f32> {
    let mut down = Downsampler::new(factor, rate).unwrap();
    let mut output = Vec::new();
    for chunk in input.chunks(block) {
        down.process(chunk, &mut output).unwrap();
    }
    down.flush(&mut output);
    output
}

fn round_trip(input: &[f32], factor: u32, rate: u32) -> Vec<f32> {
    let delay = Upsampler::new(factor, rate).unwrap().delay() / factor as usize
        + Downsampler::new(factor, rate).unwrap().delay();
    let up = upsample(input, factor, rate, 1024);
    let down = downsample(&up, factor, rate, 1024);
    down[delay..delay + input.len()].to_vec()
}

fn metrics(input: &[f32], output: &[f32]) -> (f64, f64) {
    let energy: f64 = input.iter().map(|&v| (v as f64).powi(2)).sum();
    let out_energy: f64 = output.iter().map(|&v| (v as f64).powi(2)).sum();
    let residual: f64 = input
        .iter()
        .zip(output)
        .map(|(&a, &b)| (b as f64 - a as f64).powi(2))
        .sum();
    (
        10.0 * (out_energy / energy).log10(),
        10.0 * (residual / input.len() as f64).log10(),
    )
}

#[test]
fn prototype_frequency_response() {
    // A direct 2^19-point DFT of the public interpolator impulse response.
    // Symmetry removes the imaginary component without an FFT dependency.
    for factor in [4, 8, 16] {
        let impulse = upsample(&[1.0], factor, 44100, 1);
        let taps: Vec<f64> = impulse.iter().map(|&v| v as f64 / factor as f64).collect();
        let nonzero_len = taps.len() - factor as usize + 1;
        assert!(nonzero_len % 2 == 1);
        let mid = nonzero_len / 2;
        for n in 0..=mid {
            assert_eq!(taps[n], taps[2 * mid - n]);
        }
        let bins = 1 << 19;
        let mut min_db = f64::INFINITY;
        let mut max_db = f64::NEG_INFINITY;
        let mut stop_max = 0.0_f64;
        for bin in 0..=bins / 2 {
            let normalized = 2.0 * factor as f64 * bin as f64 / bins as f64;
            if normalized > 0.907 && normalized < 1.093 {
                continue;
            }
            let omega = TAU * bin as f64 / bins as f64;
            let mut response = taps[mid];
            for n in 1..=mid {
                response += 2.0 * taps[mid - n] * (omega * n as f64).cos();
            }
            if normalized <= 0.907 {
                let value = 20.0 * response.abs().log10();
                min_db = min_db.min(value);
                max_db = max_db.max(value);
            } else {
                stop_max = stop_max.max(response.abs());
            }
        }
        let ripple = max_db - min_db;
        let attenuation = -20.0 * stop_max.log10();
        println!(
            "prototype L={factor}: ripple={ripple:.8} dB, stopband={attenuation:.5} dB, taps={nonzero_len}"
        );
        assert!(ripple <= 0.01, "ripple={ripple}");
        assert!(attenuation >= 100.0, "attenuation={attenuation}");
    }
}

#[test]
fn unity_round_trip_tones_and_band_limited_noise() {
    for factor in [4, 8, 16] {
        let mut worst_magnitude = 0.0_f64;
        let mut worst_residual = f64::NEG_INFINITY;
        for rate in common::RATES {
            for frequency in [
                20.0, 50.0, 100.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 15000.0, 17200.0,
                19600.0, 20000.0,
            ] {
                let input = common::tone(rate, frequency, 0.0, -6.0);
                let output = round_trip(&input, factor, rate);
                let (magnitude, residual) = metrics(&input, &output);
                worst_magnitude = worst_magnitude.max(magnitude.abs());
                worst_residual = worst_residual.max(residual);
                assert!(
                    magnitude.abs() <= 0.01,
                    "L={factor}, rate={rate}, f={frequency}: magnitude={magnitude}"
                );
                assert!(
                    residual < -90.0,
                    "L={factor}, rate={rate}, f={frequency}: residual={residual}"
                );
            }
            // Synthesizing seeded random Fourier components gives an independently
            // band-limited noise fixture, without reusing the filter under test.
            let random = common::noise(256);
            let len = 8192;
            let input: Vec<f32> = (0..len)
                .map(|n| {
                    let t = n as f64 / len as f64;
                    let envelope = (std::f64::consts::PI * t).sin().powi(4);
                    let value: f64 = random
                        .chunks_exact(2)
                        .enumerate()
                        .map(|(k, r)| {
                            let bin = 2 + k * 28;
                            r[0] as f64 * (TAU * (bin as f64 * t + r[1] as f64)).sin()
                        })
                        .sum();
                    (value * envelope / 32.0) as f32
                })
                .collect();
            // Highest component plus taper sidebands is 3560/8192 cycles/sample,
            // below 0.907 times Nyquist (0.4535 cycles/sample).
            let output = round_trip(&input, factor, rate);
            let (magnitude, residual) = metrics(&input, &output);
            worst_magnitude = worst_magnitude.max(magnitude.abs());
            worst_residual = worst_residual.max(residual);
            assert!(magnitude.abs() <= 0.01, "noise magnitude={magnitude}");
            assert!(residual < -90.0, "noise residual={residual}");
        }
        let full_scale = common::tone(44100, 1000.0, 0.7, 0.0);
        let (gain, residual) = metrics(&full_scale, &round_trip(&full_scale, factor, 44100));
        println!(
            "round trip L={factor}: max magnitude error={worst_magnitude:.8} dB, worst residual={worst_residual:.5} dBFS; full-scale 1 kHz gain={gain:.8} dB, residual={residual:.5} dBFS"
        );
        assert!(gain.abs() <= 0.01, "full-scale gain={gain}");
    }
}

#[test]
fn streaming_partition_delay_flush_and_reset() {
    let input = common::noise(8193);
    for factor in [4, 8, 16] {
        let whole_up = upsample(&input, factor, 48000, input.len());
        let whole_down = downsample(&whole_up, factor, 48000, whole_up.len());
        for block in [1, 7, 64, 4096] {
            assert_eq!(
                upsample(&input, factor, 48000, block),
                whole_up,
                "up L={factor}, block={block}"
            );
            assert_eq!(
                downsample(&whole_up, factor, 48000, block),
                whole_down,
                "down L={factor}, block={block}"
            );
        }
        let mut up = Upsampler::new(factor, 48000).unwrap();
        let mut down = Downsampler::new(factor, 48000).unwrap();
        let mut output = vec![99.0];
        up.flush(&mut output);
        down.flush(&mut output);
        assert_eq!(output, [99.0]);
        up.reset();
        down.reset();
        output.clear();
        up.process(&[1.0], &mut output).unwrap();
        up.flush(&mut output);
        let impulse = output.clone();
        up.flush(&mut output);
        assert_eq!(output, impulse);
        assert_eq!(
            impulse
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0,
            up.delay()
        );
        assert!(up.process(&[1.0], &mut output).is_err());
        up.reset();
        output.clear();
        up.process(&input, &mut output).unwrap();
        up.flush(&mut output);
        assert_eq!(output, whole_up);
        output.clear();
        down.process(&whole_up, &mut output).unwrap();
        down.flush(&mut output);
        assert_eq!(output, whole_down);
        let before = output.clone();
        down.flush(&mut output);
        assert_eq!(output, before);
        down.reset();
        output.clear();
        down.process(&whole_up, &mut output).unwrap();
        down.flush(&mut output);
        assert_eq!(output, whole_down);
        let round = downsample(&impulse, factor, 48000, 7);
        let index = round
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert_eq!(index, up.delay() / factor as usize + down.delay());
    }
}

#[test]
fn invalid_blocks_are_atomic() {
    assert!(matches!(
        Upsampler::new(2, 48000),
        Err(Error::InvalidConfig(_))
    ));
    assert!(matches!(
        Downsampler::new(8, 32000),
        Err(Error::InvalidInput(_))
    ));
    let mut up = Upsampler::new(8, 48000).unwrap();
    let mut down = Downsampler::new(8, 48000).unwrap();
    let mut output = vec![7.0];
    assert!(up.process(&[1.0, f32::NAN], &mut output).is_err());
    assert!(down.process(&[1.0, f32::INFINITY], &mut output).is_err());
    assert_eq!(output, [7.0]);
    output.clear();
    up.process(&[1.0], &mut output).unwrap();
    up.flush(&mut output);
    assert_eq!(output, upsample(&[1.0], 8, 48000, 1));
    output.clear();
    down.process(&[1.0], &mut output).unwrap();
    down.flush(&mut output);
    assert_eq!(output, downsample(&[1.0], 8, 48000, 1));
}
