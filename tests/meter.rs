mod common;

use std::{
    f64::consts::TAU,
    sync::atomic::{AtomicBool, Ordering},
};
use truepeak::{
    Error, sample_peak, sample_peak_with_control, true_peak, true_peak_in_range,
    true_peak_in_range_with_control, true_peak_per_channel, true_peak_per_channel_in_range,
    true_peak_per_channel_in_range_with_control, true_peak_with_control,
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
                    common::assert_peak_bits_eq(
                        true_peak_in_range(&[&input], rate, factor, 0..input.len()).unwrap(),
                        peak,
                    );
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
            common::assert_peak_bits_eq(
                true_peak_in_range(&[input], 48000, factor, 0..input.len()).unwrap(),
                peak,
            );
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
        for (actual, expected) in true_peak_per_channel_in_range(&channels, rate, 8, 0..input.len())
            .unwrap()
            .into_iter()
            .zip(&per_channel)
        {
            common::assert_peak_bits_eq(actual, *expected);
        }
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
fn range_keeps_continuous_context_at_the_cut() {
    let rate = 48000;
    let end = 76800;
    let input: Vec<f32> = (0..rate * 4)
        .map(|n| (0.99 * (TAU * 50.15625 * n as f64 / rate as f64).sin()) as f32)
        .collect();
    for factor in [4, 8, 16] {
        let whole = true_peak(&[&input], rate, factor).unwrap();
        let range = true_peak_in_range(&[&input], rate, factor, 0..end).unwrap();
        let slice = true_peak(&[&input[..end]], rate, factor).unwrap();
        println!(
            "50.15625 Hz, 48 kHz, amplitude 0.99, L={factor}: whole={:.8}, range={:.8}, slice={:.8} dBTP",
            whole.dbtp, range.dbtp, slice.dbtp
        );
        assert!((range.dbtp - whole.dbtp).abs() < 0.001);
        assert!((range.dbtp - 20.0 * 0.99_f64.log10()).abs() < 0.001);
        assert!(slice.dbtp > 1.0);
        assert!((0.0..1.6).contains(&range.position_seconds));
    }
}

#[test]
fn range_selects_refined_click_positions_at_both_ends() {
    let range = 1000..3000;
    for factor in [4, 8, 16] {
        let mut outside = vec![0.0; 4000];
        outside[range.start - 1] = 1.0;
        outside[range.end] = 1.0;
        let peak = true_peak_in_range(&[&outside], 48000, factor, range.clone()).unwrap();
        // The out-of-range impulses still contribute sinc sidelobes inside the range.
        assert!(peak.linear > 0.2 && peak.linear < 0.3, "{peak:?}");
        assert!(
            (range.start as f64..range.end as f64).contains(&(peak.position_seconds * 48000.0))
        );
        for index in [range.start, range.end - 1] {
            let mut inside = vec![0.0; 4000];
            inside[index] = 1.0;
            let peaks =
                true_peak_per_channel_in_range(&[&outside, &inside], 48000, factor, range.clone())
                    .unwrap();
            assert!(peaks[1].linear > 0.99, "{:?}", peaks[1]);
            assert_eq!(peaks[1].position_seconds, index as f64 / 48000.0);
            assert_eq!(peaks[1].channel, 1);
            assert_eq!(
                true_peak_in_range(&[&outside, &inside], 48000, factor, range.clone()).unwrap(),
                peaks[1],
            );
        }
        for boundary in [range.start, range.end] {
            let mut asymmetric = vec![0.0; 4000];
            asymmetric[boundary] = 1.0;
            asymmetric[boundary - 1] = 0.03;
            let whole = true_peak(&[&asymmetric], 48000, factor).unwrap();
            let frame = whole.position_seconds * 48000.0;
            assert!(frame < boundary as f64 && frame > boundary as f64 - 0.5 / factor as f64);
            let peak = true_peak_in_range(&[&asymmetric], 48000, factor, range.clone()).unwrap();
            if boundary == range.start {
                assert!(peak.linear < 0.3, "{peak:?}");
            } else {
                assert_eq!(peak.linear, whole.linear);
                assert!((peak.position_seconds - whole.position_seconds).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn range_validation_empty_ranges_and_control() {
    let input = common::tone(48000, 997.0, 0.0, -3.0);
    let channels = [&input[..], &input[..]];
    for range in [0..0, 1234..1234, input.len()..input.len()] {
        for peak in true_peak_per_channel_in_range(&channels, 48000, 8, range).unwrap() {
            assert_eq!(peak.dbtp, f64::NEG_INFINITY);
            assert_eq!(peak.linear, 0.0);
            assert_eq!(peak.position_seconds, 0.0);
        }
    }
    for (start, end) in [(10, 9), (0, input.len() + 1), (usize::MAX, usize::MAX)] {
        assert!(matches!(
            true_peak_in_range(&channels, 48000, 8, start..end),
            Err(Error::InvalidInput(_))
        ));
    }
    let range = 1200..3600;
    let cancel = AtomicBool::new(true);
    assert_eq!(
        true_peak_in_range_with_control(&channels, 48000, 8, range.clone(), &cancel, &mut |_| {
            panic!("pre-cancelled");
        }),
        Err(Error::Cancelled)
    );
    for channel in [0, 1] {
        cancel.store(false, Ordering::Relaxed);
        let mut last_done = 0;
        let result = true_peak_per_channel_in_range_with_control(
            &channels,
            48000,
            8,
            range.clone(),
            &cancel,
            &mut |p| {
                assert_eq!(p.pass, 1);
                assert!(p.done_samples >= last_done && p.done_samples <= p.total_samples);
                last_done = p.done_samples;
                if p.done_samples > channel * (p.total_samples / channels.len()) {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert_eq!(result, Err(Error::Cancelled));
    }
    cancel.store(false, Ordering::Relaxed);
    let mut last = None;
    let peak =
        true_peak_in_range_with_control(&channels, 48000, 8, range.clone(), &cancel, &mut |p| {
            last = Some(p)
        })
        .unwrap();
    assert_eq!(
        peak,
        true_peak_in_range(&channels, 48000, 8, range).unwrap()
    );
    assert_eq!(peak.channel, 0);
    let progress = last.unwrap();
    assert_eq!(progress.done_samples, progress.total_samples);
    assert!(progress.total_samples > 2400 * channels.len());
    assert!(progress.total_samples < input.len() * channels.len());
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
