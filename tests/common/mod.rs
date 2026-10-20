#![allow(dead_code)]

use std::f64::consts::{PI, TAU};

pub const FREQUENCIES: [f64; 6] = [997.0, 5000.0, 12000.0, 17200.0, 19600.0, 20000.0];
pub const RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];

pub fn assert_peak_bits_eq(actual: truepeak::TruePeak, expected: truepeak::TruePeak) {
    assert_eq!(actual.dbtp.to_bits(), expected.dbtp.to_bits());
    assert_eq!(actual.linear.to_bits(), expected.linear.to_bits());
    assert_eq!(actual.channel, expected.channel);
    assert_eq!(
        actual.position_seconds.to_bits(),
        expected.position_seconds.to_bits()
    );
}

pub fn tone(rate: u32, frequency: f64, phase: f64, db: f64) -> Vec<f32> {
    let len = (rate / 10) as usize;
    let fade = (rate / 100) as usize;
    let amplitude = 10.0_f64.powf(db / 20.0);
    (0..len)
        .map(|n| {
            // Tapering avoids treating abrupt file edges as part of a pure tone.
            let edge = n.min(len - 1 - n);
            let envelope = if edge < fade {
                0.5 - 0.5 * (PI * edge as f64 / fade as f64).cos()
            } else {
                1.0
            };
            (amplitude * envelope * (TAU * frequency * n as f64 / rate as f64 + phase).sin()) as f32
        })
        .collect()
}

pub fn noise(len: usize) -> Vec<f32> {
    let mut state = 0x627e_c8a4_1197_2d35_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 40) as f64 / (1_u64 << 23) as f64 - 1.0) as f32
        })
        .collect()
}

pub fn oracle_available(available: bool, message: &str) -> bool {
    if !available {
        eprintln!("SKIP oracle: {message}");
        assert!(
            std::env::var("TRUEPEAK_REQUIRE_ORACLES").as_deref() != Ok("1"),
            "required oracle unavailable: {message}"
        );
    }
    available
}

pub fn scale_to_peak(input: &mut [f32], rate: u32, dbtp: f64) {
    let measured = truepeak::true_peak(&[input], rate, 8).unwrap().linear;
    let scale = 10.0_f64.powf(dbtp / 20.0) / measured;
    for sample in input {
        *sample = (*sample as f64 * scale) as f32;
    }
}

pub fn program(rate: u32) -> Vec<f32> {
    let len = (rate / 3) as usize;
    let white = noise(len + 96);
    let mut taps: Vec<f64> = (0..97)
        .map(|n| {
            let d = n as f64 - 48.0;
            let sinc = if d == 0.0 {
                0.7
            } else {
                (TAU * 0.35 * d).sin() / (PI * d)
            };
            sinc * (0.5 - 0.5 * (TAU * n as f64 / 96.0).cos())
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap /= sum;
    }
    let mut result: Vec<f32> = (0..len)
        .map(|n| {
            let t = n as f64 / rate as f64;
            let envelope = (PI * n as f64 / (len - 1) as f64).sin().powi(2);
            let noise: f64 = white[n..n + 97]
                .iter()
                .zip(&taps)
                .map(|(&x, &h)| x as f64 * h)
                .sum();
            let transient = (-((t - 0.12) / 0.0015).powi(2)).exp() * (TAU * 2300.0 * t).sin();
            ((0.3 + 0.7 * (TAU * 3.0 * t).sin().powi(2)) * envelope * noise + transient) as f32
        })
        .collect();
    scale_to_peak(&mut result, rate, 3.0);
    result
}

pub fn clicks(rate: u32) -> (Vec<f32>, Vec<usize>) {
    let mut input = vec![0.0; (rate / 3) as usize];
    let indices: Vec<usize> = (1..=3).map(|n| (rate as usize * n) / 12).collect();
    for &index in &indices {
        input[index] = 1.0;
    }
    scale_to_peak(&mut input, rate, 2.0);
    (input, indices)
}

pub fn run_limit(channels: &[&[f32]], rate: u32, config: &truepeak::Config) -> truepeak::Output {
    truepeak::limit(
        channels,
        rate,
        config,
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap()
}
