mod common;

use std::{f64::consts::TAU, path::Path, process::Command};
use truepeak::true_peak;

fn ffmpeg_peak(path: &Path) -> f64 {
    let result = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-af", "ebur128=peak=true", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "ffmpeg failed: {stderr}");
    let summary = stderr
        .rsplit_once("True peak:")
        .expect("FFmpeg true-peak summary")
        .1;
    summary
        .split_once("Peak:")
        .expect("summary Peak field")
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .expect("numeric dBTP reading")
}

fn compare(rate: u32, frequency: f64) {
    let version = Command::new("ffmpeg").arg("-version").output();
    if !common::oracle_available(
        version.as_ref().is_ok_and(|r| r.status.success()),
        "ffmpeg is not on PATH",
    ) {
        return;
    }
    println!(
        "{}",
        String::from_utf8_lossy(&version.unwrap().stdout)
            .lines()
            .next()
            .unwrap()
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("tone.wav");
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut failures = Vec::new();
    for phase in 0..32 {
        let input = common::tone(rate, frequency, TAU * phase as f64 / 32.0, -3.0);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for &sample in &input {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let reference = ffmpeg_peak(&path);
        let peak = true_peak(&[&input], rate, 8).unwrap().dbtp;
        let delta = peak - reference;
        min = min.min(delta);
        max = max.max(delta);
        let valid = if frequency < 12000.0 {
            delta.abs() <= 0.1
        } else if frequency == 12000.0 {
            (-0.05..=0.25).contains(&delta)
        } else {
            (-0.05..=0.5).contains(&delta)
        };
        if !valid {
            failures.push(format!(
                "phase={phase}: meter={peak:.8}, FFmpeg={reference:.1}, delta={delta:.8} dB"
            ));
        }
    }
    println!(
        "meter comparison, rate={rate}, f={frequency}: meter - FFmpeg [{min:.8}, {max:.8}] dB"
    );
    assert!(
        failures.is_empty(),
        "rate={rate}, f={frequency}:\n{}",
        failures.join("\n")
    );
}

#[test]
fn low_frequency_tones() {
    for rate in [44100, 48000] {
        for frequency in [997.0, 5000.0, 12000.0] {
            compare(rate, frequency);
        }
    }
}

#[test]
fn tone_17200_hz_at_44100() {
    compare(44100, 17200.0);
}
#[test]
fn tone_17200_hz_at_48000() {
    compare(48000, 17200.0);
}
#[test]
fn tone_19600_hz_at_44100() {
    compare(44100, 19600.0);
}
#[test]
fn tone_19600_hz_at_48000() {
    compare(48000, 19600.0);
}
#[test]
fn tone_20000_hz_at_44100() {
    compare(44100, 20000.0);
}
#[test]
fn tone_20000_hz_at_48000() {
    compare(48000, 20000.0);
}

fn limited_reference(fixture: &str, rate: u32, input: &[f32]) {
    let version = Command::new("ffmpeg").arg("-version").output();
    if !common::oracle_available(
        version.as_ref().is_ok_and(|r| r.status.success()),
        "ffmpeg is not on PATH",
    ) {
        return;
    }
    println!(
        "{}",
        String::from_utf8_lossy(&version.unwrap().stdout)
            .lines()
            .next()
            .unwrap()
    );
    let output = common::run_limit(&[input, input], rate, &truepeak::Config::default());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("limited.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for frame in 0..input.len() {
        for channel in &output.channels {
            writer.write_sample(channel[frame]).unwrap();
        }
    }
    writer.finalize().unwrap();
    let reference = ffmpeg_peak(&path);
    println!(
        "{fixture} FFmpeg, rate={rate}: input={:.8}, meter={:.8}, FFmpeg={reference:.1} dBTP; passes={}",
        output.input_true_peak_dbtp, output.output_true_peak_dbtp, output.passes
    );
    assert!(
        reference <= -0.95,
        "{fixture}, rate={rate}, FFmpeg={reference}"
    );
    assert!(
        output.output_true_peak_dbtp <= -0.95,
        "{fixture}, rate={rate}, meter={}",
        output.output_true_peak_dbtp
    );
    assert!(output.ceiling_met);
}

#[test]
fn limiter_997_hz_across_sample_rates() {
    for rate in common::RATES {
        limited_reference("997 Hz tone", rate, &common::tone(rate, 997.0, 0.3, 1.0));
    }
}

#[test]
fn limiter_17200_hz_at_44100() {
    limited_reference(
        "17.2 kHz tone",
        44100,
        &common::tone(44100, 17200.0, 0.7, 1.0),
    );
}

#[test]
fn limiter_square_wave() {
    let input: Vec<f32> = (0..4800)
        .map(|n| if n % 64 < 32 { 1.0 } else { -1.0 })
        .collect();
    limited_reference("square wave", 48000, &input);
}

#[test]
fn limiter_program_across_sample_rates() {
    for rate in common::RATES {
        limited_reference("seeded program", rate, &common::program(rate));
    }
}
