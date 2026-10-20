mod common;

use std::{
    f64::consts::TAU,
    sync::atomic::{AtomicBool, Ordering},
};
use truepeak::{
    Error, sample_peak, sample_peak_with_control, true_peak, true_peak_per_channel,
    true_peak_with_control,
};

#[test]
fn phase_ramped_tones() {
    let mut worst = 0.0_f64;
    for factor in [4, 8, 16] {
        for rate in [44100, 48000] {
            for frequency in common::FREQUENCIES {
                let mut min = f64::INFINITY;
                let mut max = f64::NEG_INFINITY;
                for phase in 0..32 {
                    let input = common::tone(rate, frequency, TAU * phase as f64 / 32.0, -3.0);
                    let peak = true_peak(&[&input], rate, factor).unwrap();
                    min = min.min(peak.dbtp);
                    max = max.max(peak.dbtp);
                    worst = worst.max((peak.dbtp + 3.0).abs());
                    assert!(
                        (peak.dbtp + 3.0).abs() <= 0.02,
                        "L={factor}, rate={rate}, f={frequency}, phase={phase}: {} dBTP, error={} dB",
                        peak.dbtp,
                        peak.dbtp + 3.0
                    );
                }
                println!(
                    "phase sweep L={factor}, rate={rate}, f={frequency}: [{min:.8}, {max:.8}] dBTP"
                );
            }
        }
    }
    println!("phase sweep worst absolute error={worst:.8} dB");
}

#[test]
fn sample_peak_exact_silence_layout_and_positions() {
    let a = [0.0, -0.25, 0.5, 0.5];
    let b = [-0.75, 0.0, 0.0, 0.0];
    let peaks = sample_peak(&[&a, &b]).unwrap();
    assert_eq!(peaks.per_channel[0].linear, 0.5);
    assert_eq!(peaks.per_channel[0].dbfs, 20.0 * 0.5_f64.log10());
    assert_eq!(peaks.per_channel[0].position_samples, 2);
    assert_eq!(peaks.overall, peaks.per_channel[1]);
    assert_eq!(peaks.overall.linear, 0.75);
    let mut late = vec![0.0; 4097];
    late[4096] = -1.25;
    assert_eq!(
        sample_peak(&[&late, &late])
            .unwrap()
            .overall
            .position_samples,
        4096
    );
    assert_eq!(sample_peak(&[&late, &late]).unwrap().overall.channel, 0);
    for input in [&[][..], &[0.0; 2500][..]] {
        assert_eq!(
            sample_peak(&[input]).unwrap().overall.dbfs,
            f64::NEG_INFINITY
        );
        for factor in [4, 8, 16] {
            let peak = true_peak(&[input], 48000, factor).unwrap();
            assert_eq!(peak.dbtp, f64::NEG_INFINITY);
            assert_eq!(peak.linear, 0.0);
            assert_eq!(peak.position_seconds, 0.0);
        }
    }
    for rate in common::RATES {
        let mut input = vec![0.0; 2049];
        input[1024] = 0.75;
        let quiet: Vec<f32> = input.iter().map(|v| v / 2.0).collect();
        let channels = [&quiet[..], &input[..]];
        let per_channel = true_peak_per_channel(&channels, rate, 8).unwrap();
        let peak = true_peak(&channels, rate, 8).unwrap();
        assert_eq!(peak, per_channel[1]);
        assert_eq!(peak.position_seconds, 1024.0 / rate as f64);
        assert_eq!(peak.channel, 1);
        assert_eq!(
            true_peak(&[input.as_slice(); 8], rate, 8).unwrap().channel,
            0
        );
    }
    for channels in [
        &[][..],
        &[&a[..]; 9][..],
        &[&a[..], &b[..3]][..],
        &[&[f32::NAN][..]][..],
        &[&[f32::INFINITY][..]][..],
    ] {
        assert!(matches!(sample_peak(channels), Err(Error::InvalidInput(_))));
        assert!(matches!(
            true_peak(channels, 48000, 8),
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(matches!(
        true_peak(&[&a], 32000, 8),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        true_peak(&[&a], 48000, 2),
        Err(Error::InvalidConfig(_))
    ));
}

#[test]
fn cancellation_in_each_channel_and_progress() {
    let input = common::tone(48000, 997.0, 0.0, -3.0);
    for channel in [0, 1] {
        let cancel = AtomicBool::new(false);
        let mut last_done = 0;
        let mut saw_pass = false;
        let result = true_peak_with_control(&[&input, &input], 48000, 8, &cancel, &mut |p| {
            assert!(p.done_samples >= last_done && p.done_samples <= p.total_samples);
            last_done = p.done_samples;
            if p.done_samples > channel * input.len() {
                saw_pass = true;
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(saw_pass);
        assert_eq!(result, Err(Error::Cancelled));
    }
    let cancel = AtomicBool::new(true);
    assert_eq!(
        true_peak_with_control(&[&input], 48000, 8, &cancel, &mut |_| panic!(
            "pre-cancelled"
        )),
        Err(Error::Cancelled)
    );
    cancel.store(false, Ordering::Relaxed);
    assert_eq!(
        sample_peak_with_control(&[&input], &cancel, &mut |p| {
            if p.done_samples > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        }),
        Err(Error::Cancelled)
    );
    cancel.store(false, Ordering::Relaxed);
    let mut last = None;
    let peak =
        true_peak_with_control(&[&input], 48000, 8, &cancel, &mut |p| last = Some(p)).unwrap();
    let progress = last.unwrap();
    assert_eq!(progress.done_samples, progress.total_samples);
    assert_eq!(progress.total_samples, input.len());
    assert_eq!(progress.pass, 1);
    assert_eq!(peak, true_peak(&[&input], 48000, 8).unwrap());
}

#[cfg(feature = "serde")]
#[test]
fn finite_outputs_serialize_and_round_trip() {
    let input = [0.0, -0.5, 0.25];
    let peak = true_peak(&[&input], 48000, 8).unwrap();
    let json = serde_json::to_string(&peak).unwrap();
    assert_eq!(
        serde_json::from_str::<truepeak::TruePeak>(&json).unwrap(),
        peak
    );
    let peaks = sample_peak(&[&input]).unwrap();
    let json = serde_json::to_string(&peaks).unwrap();
    assert_eq!(
        serde_json::from_str::<truepeak::SamplePeak>(&json).unwrap(),
        peaks
    );
}
