mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use truepeak::{Config, Error, Limiter, Link, limit, true_peak};

fn assert_ceiling(fixture: &str, rate: u32, input: &[&[f32]]) {
    let config = Config::default();
    let output = common::run_limit(input, rate, &config);
    let refs: Vec<&[f32]> = output.channels.iter().map(Vec::as_slice).collect();
    let measured = true_peak(&refs, rate, 8).unwrap().dbtp;
    let first_peak = if output.passes == 1 {
        measured
    } else {
        common::run_limit(
            input,
            rate,
            &Config {
                max_passes: 1,
                ..Config::default()
            },
        )
        .output_true_peak_dbtp
    };
    println!(
        "{fixture}, rate={rate}: input={:.8} dBTP, output={measured:.8} dBTP, passes={}, max reduction={:.8} dB, pass-1 peak={first_peak:.8} dBTP, rise above pass-1 target={:.8} dB",
        output.input_true_peak_dbtp,
        output.passes,
        output.max_reduction_db,
        first_peak - (output.ceiling_dbtp - config.initial_margin_db)
    );
    assert!(
        measured <= output.ceiling_dbtp + 0.05,
        "measured={measured}"
    );
    assert!(
        output.ceiling_met,
        "reported peak={}",
        output.output_true_peak_dbtp
    );
    assert!(output.passes <= 4);
    assert_eq!(output.passes, 1, "avoidable retry: peak={measured}");
}

#[test]
fn tones_square_and_program_meet_the_ceiling() {
    for rate in common::RATES {
        let input = common::tone(rate, 997.0, 0.3, 1.0);
        assert_ceiling("997 Hz tone", rate, &[&input]);
        let input = common::program(rate);
        assert_ceiling("seeded program", rate, &[&input, &input]);
    }
    let input = common::tone(44100, 17200.0, 0.7, 1.0);
    assert_ceiling("17.2 kHz tone", 44100, &[&input]);
    let input: Vec<f32> = (0..4800)
        .map(|n| if n % 64 < 32 { 1.0 } else { -1.0 })
        .collect();
    assert_ceiling("square wave", 48000, &[&input]);
}

#[test]
fn bypass_is_bit_exact_and_silence_stays_silence() {
    for dbtp in [-3.0, -1.01] {
        let input = common::tone(48000, 997.0, 0.7, dbtp);
        let output = common::run_limit(&[&input], 48000, &Config::default());
        assert_eq!(output.channels[0], input);
        assert_eq!(output.passes, 0);
        assert_eq!(output.max_reduction_db, 0.0);
        assert!(output.gain_db.iter().all(|&v| v == 0.0));
    }
    let tiny = [-0.0_f32, f32::from_bits(1), -0.25];
    let copied = common::run_limit(&[&tiny], 48000, &Config::default());
    assert!(
        tiny.iter()
            .zip(&copied.channels[0])
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    let empty = common::run_limit(&[&[]], 48000, &Config::default());
    assert!(empty.channels[0].is_empty());
    assert_eq!(empty.output_true_peak_dbtp, f64::NEG_INFINITY);
    let silent = vec![0.0; 4000];
    let output = common::run_limit(&[silent.as_slice()], 48000, &Config::default());
    assert_eq!(output.channels[0], silent);
    assert_eq!(output.output_true_peak_dbtp, f64::NEG_INFINITY);
    println!("bypass: bit-identical audio; passes=0; max reduction=0 dB; silence=-inf dBTP");
}

#[test]
fn quantized_verification_reports_the_exported_pcm_peak() {
    let mut input = common::tone(44100, 17200.0, 0.9, -0.7);
    common::scale_to_peak(&mut input, 44100, -0.7);
    for bits in [16, 24] {
        let config = Config {
            verify_bit_depth: Some(bits),
            ..Config::default()
        };
        let output = common::run_limit(&[&input], 44100, &config);
        let scale = (1_u64 << (bits - 1)) as f64;
        let quantized: Vec<f32> = output.channels[0]
            .iter()
            .map(|&v| ((v as f64 * scale).round().clamp(-scale, scale - 1.0) / scale) as f32)
            .collect();
        let measured = true_peak(&[&quantized], 44100, 8).unwrap().dbtp;
        assert_eq!(output.output_true_peak_dbtp, measured);
        assert!(measured <= -0.95, "bits={bits}, measured={measured}");
        assert!(output.ceiling_met);
        assert_eq!(
            output.passes, 1,
            "avoidable quantized retry: peak={measured}"
        );
        println!(
            "quantized verification, bits={bits}: exported PCM={measured:.8} dBTP; passes={}, rise above pass-1 target={:.8} dB",
            output.passes,
            measured - (config.ceiling_dbtp - config.initial_margin_db)
        );
        let quiet = common::tone(44100, 997.0, 0.3, -6.0);
        let bypass = common::run_limit(&[&quiet], 44100, &config);
        assert_eq!(bypass.passes, 0);
        assert_eq!(bypass.channels[0], quiet);
        let pcm: Vec<f32> = quiet
            .iter()
            .map(|&v| ((v as f64 * scale).round().clamp(-scale, scale - 1.0) / scale) as f32)
            .collect();
        assert_eq!(
            bypass.output_true_peak_dbtp,
            true_peak(&[&pcm], 44100, 8).unwrap().dbtp
        );
        let low_ceiling = 20.0 * (0.65 / scale).log10();
        let low = common::tone(44100, 997.0, 0.3, 20.0 * (0.6 / scale).log10());
        let rounded = common::run_limit(
            &[&low],
            44100,
            &Config {
                ceiling_dbtp: low_ceiling,
                ..config
            },
        );
        assert_eq!(rounded.passes, 0);
        assert_eq!(rounded.channels[0], low);
        assert!(!rounded.ceiling_met);
        assert!(rounded.output_true_peak_dbtp > low_ceiling + 0.001);
        println!(
            "quantized bypass, bits={bits}: ceiling={low_ceiling:.8}, exported={:.8} dBTP; passes=0, ceiling_met=false",
            rounded.output_true_peak_dbtp
        );
    }
}

#[test]
fn click_train_is_time_aligned_at_every_rate() {
    for rate in common::RATES {
        let (input, indices) = common::clicks(rate);
        let output = common::run_limit(&[&input], rate, &Config::default());
        assert_eq!(output.channels[0].len(), input.len());
        for index in indices {
            let radius = 20;
            let peak = output.channels[0][index - radius..=index + radius]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0
                + index
                - radius;
            assert_eq!(peak, index, "rate={rate}");
        }
        println!(
            "click train, rate={rate}: click displacement=0 samples, length={} frames, passes={}, output={:.8} dBTP",
            input.len(),
            output.passes,
            output.output_true_peak_dbtp
        );
    }
}

fn stream(
    input: &[&[f32]],
    rate: u32,
    config: &Config,
    block: usize,
) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, usize) {
    let mut processor = Limiter::new(rate, input.len(), config).unwrap();
    let mut output = vec![Vec::new(); input.len()];
    let mut gains = output.clone();
    for start in (0..input[0].len()).step_by(block) {
        let end = (start + block).min(input[0].len());
        let refs: Vec<&[f32]> = input.iter().map(|c| &c[start..end]).collect();
        processor
            .process_with_gain(&refs, &mut output, &mut gains)
            .unwrap();
    }
    processor.flush_with_gain(&mut output, &mut gains).unwrap();
    let before = output.clone();
    processor.flush_with_gain(&mut output, &mut gains).unwrap();
    assert_eq!(output, before);
    (output, gains, processor.latency_samples())
}

#[test]
fn linking_controls_the_quiet_channel_and_stream_partitions_are_identical() {
    let left = common::tone(48000, 6000.0, 0.35, 2.0);
    let right = common::tone(48000, 997.0, 0.3, -20.0);
    for link in [Link::Linked, Link::Unlinked] {
        let config = Config {
            link,
            ..Config::default()
        };
        let (audio, gain, latency) = stream(&[&left, &right], 48000, &config, left.len());
        assert_eq!(latency, 76 + 96);
        assert_eq!(gain[0].len(), audio[0].len());
        if link == Link::Linked {
            assert_eq!(gain[0], gain[1]);
        } else {
            assert!(gain[1].iter().all(|&v| v == 1.0));
        }
        for block in [1, 7, 64, 4096] {
            let (partitioned, partitioned_gain, _) =
                stream(&[&left, &right], 48000, &config, block);
            assert_eq!(partitioned, audio, "block={block}, link={link:?}");
            assert!(
                partitioned
                    .iter()
                    .flatten()
                    .zip(audio.iter().flatten())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            assert!(
                partitioned_gain
                    .iter()
                    .flatten()
                    .zip(gain.iter().flatten())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            assert_eq!(partitioned_gain, gain, "block={block}, link={link:?}");
        }
        let first = common::run_limit(&[&left, &right], 48000, &config);
        assert_eq!(first.passes, 1, "link={link:?}");
        assert_eq!(first, common::run_limit(&[&left, &right], 48000, &config));
        println!(
            "channel linking: {link:?}, right minimum gain={:.8} dB",
            20.0 * gain[1].iter().copied().fold(1.0_f32, f32::min).log10()
        );
    }
    println!(
        "stream partitions: repeated runs and all five partitions are bit-identical, including gain telemetry"
    );
}

#[test]
fn transient_recovery_and_sustained_gain_are_stable() {
    let rate = 48000_u32;
    let len = (rate * 2) as usize;
    let mut input: Vec<f32> = (0..len)
        .map(|n| {
            (10.0_f64.powf(-12.0 / 20.0)
                * (std::f64::consts::TAU * 997.0 * n as f64 / rate as f64).sin()) as f32
        })
        .collect();
    let start = (rate / 10) as usize;
    let duration = (rate / 1000) as usize;
    let base = input[start..start + duration].to_vec();
    let mut low = 0.0;
    let mut high = 4.0;
    for _ in 0..32 {
        let amplitude = (low + high) * 0.5;
        for n in 0..duration {
            input[start + n] = base[n]
                + (amplitude
                    * (std::f64::consts::PI * n as f64 / (duration - 1) as f64)
                        .sin()
                        .powi(2)) as f32;
        }
        let peak = true_peak(&[&input[start - 256..start + duration + 256]], rate, 8)
            .unwrap()
            .dbtp;
        if peak > 5.0 {
            high = amplitude;
        } else {
            low = amplitude;
        }
    }
    let known_peak = true_peak(&[&input], rate, 8).unwrap().dbtp;
    assert!(
        (known_peak - 5.0).abs() < 0.02,
        "transient peak={known_peak}"
    );
    println!("transient recovery: input true peak={known_peak:.8} dBTP (6 dB over ceiling)");
    let output = common::run_limit(&[&input], rate, &Config::default());
    println!(
        "transient recovery: passes={}, output={:.8} dBTP",
        output.passes, output.output_true_peak_dbtp
    );
    for (ms, bound) in [(200, 0.5), (1000, 0.1)] {
        let frame = start + duration + rate as usize * ms / 1000;
        let reduction = -output.gain_db[frame / output.gain_hop_samples];
        println!("transient recovery: reduction {ms} ms after transient={reduction:.8} dB");
        assert!(reduction < bound, "ms={ms}, reduction={reduction}");
    }
    let input: Vec<f32> = (0..rate as usize * 3)
        .map(|n| {
            (10.0_f64.powf(3.0 / 20.0)
                * (std::f64::consts::TAU * 50.0 * n as f64 / rate as f64).sin()) as f32
        })
        .collect();
    let output = common::run_limit(&[&input], rate, &Config::default());
    println!(
        "sustained gain: passes={}, output={:.8} dBTP",
        output.passes, output.output_true_peak_dbtp
    );
    let input = &input[rate as usize / 2..];
    let actual = &output.channels[0][rate as usize / 2..];
    let energy: f64 = input.iter().map(|&v| (v as f64).powi(2)).sum();
    let cross: f64 = input
        .iter()
        .zip(actual)
        .map(|(&x, &y)| x as f64 * y as f64)
        .sum();
    let gain = cross / energy;
    let residual: f64 = input
        .iter()
        .zip(actual)
        .map(|(&x, &y)| (y as f64 - gain * x as f64).powi(2))
        .sum();
    let relative = 10.0 * (residual / (gain * gain * energy)).log10();
    println!(
        "sustained gain: constant gain={:.8} dB, relative residual={relative:.8} dB",
        20.0 * gain.log10()
    );
    assert!(relative < -30.0, "relative residual={relative}");
}

#[test]
fn cancellation_and_invalid_stream_blocks_return_errors() {
    let input = common::tone(48000, 997.0, 0.3, 1.0);
    for verify in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut last_done = 0;
        let result = limit(&[&input], 48000, &Config::default(), &cancel, &mut |p| {
            if p.pass == 1 {
                assert!(p.done_samples >= last_done && p.done_samples <= p.total_samples);
                last_done = p.done_samples;
                if p.done_samples > if verify { input.len() } else { 0 } {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(result, Err(Error::Cancelled));
    }
    let mut processor = Limiter::new(48000, 1, &Config::default()).unwrap();
    let mut output = vec![vec![9.0]];
    assert!(processor.process(&[&[0.0, f32::NAN]], &mut output).is_err());
    assert_eq!(output, [vec![9.0]]);
    output[0].clear();
    processor.process(&[&input], &mut output).unwrap();
    processor.flush(&mut output).unwrap();
    assert_eq!(output, stream(&[&input], 48000, &Config::default(), 4096).0);
    processor.reset();
    output[0].clear();
    processor.process(&[&input], &mut output).unwrap();
    processor.flush(&mut output).unwrap();
    assert_eq!(output, stream(&[&input], 48000, &Config::default(), 4096).0);
    println!("cancellation: processing and verification return Err(Cancelled)");
}

#[test]
fn config_validation_names_every_invalid_field() {
    let mut count = 0;
    for field in [
        "ceiling_dbtp",
        "initial_margin_db",
        "lookahead_ms",
        "release_fast_ms",
        "release_slow_ms",
        "slow_attack_ms",
        "gain_hop_ms",
    ] {
        let invalid_values = match field {
            "ceiling_dbtp" => vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 6.001],
            "initial_margin_db" => {
                vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.001, 0.101]
            }
            "lookahead_ms" => vec![f64::NAN, f64::INFINITY, 0.999, 10.001],
            _ => vec![f64::NAN, f64::INFINITY, 0.0, -1.0],
        };
        for value in invalid_values {
            let mut config = Config::default();
            match field {
                "ceiling_dbtp" => config.ceiling_dbtp = value,
                "initial_margin_db" => config.initial_margin_db = value,
                "lookahead_ms" => config.lookahead_ms = value,
                "release_fast_ms" => config.release_fast_ms = value,
                "release_slow_ms" => config.release_slow_ms = value,
                "slow_attack_ms" => config.slow_attack_ms = value,
                "gain_hop_ms" => config.gain_hop_ms = value,
                _ => unreachable!(),
            }
            assert!(
                matches!(config.validate(), Err(Error::InvalidConfig(message)) if message.contains(field))
            );
            assert!(
                matches!(limit(&[&[0.0]], 48000, &config, &AtomicBool::new(false), &mut |_| {}), Err(Error::InvalidConfig(message)) if message.contains(field))
            );
            count += 1;
        }
    }
    for value in [0, 1, 2, 3, 5, 32] {
        assert!(
            matches!((Config { oversampling:value,..Config::default() }).validate(),Err(Error::InvalidConfig(m)) if m.contains("oversampling"))
        );
        count += 1;
    }
    for value in [0, 9] {
        assert!(
            matches!((Config { max_passes:value,..Config::default() }).validate(),Err(Error::InvalidConfig(m)) if m.contains("max_passes"))
        );
        count += 1;
    }
    for value in [0, 8, 15, 17, 32] {
        assert!(
            matches!((Config { verify_bit_depth:Some(value),..Config::default() }).validate(),Err(Error::InvalidConfig(m)) if m.contains("verify_bit_depth"))
        );
        count += 1;
    }
    for initial_margin_db in [0.0, 0.1] {
        Config {
            initial_margin_db,
            ..Config::default()
        }
        .validate()
        .unwrap();
    }
    println!("config validation: {count} invalid configurations rejected with their field name");
}

#[test]
fn exhausted_budget_reports_failure_and_iterations_restart_from_original() {
    let rate = 44100;
    let input = common::tone(rate, 6000.0, 0.7, -37.0);
    let config = Config {
        ceiling_dbtp: -40.0,
        initial_margin_db: 0.01,
        verify_bit_depth: Some(16),
        max_passes: 1,
        ..Config::default()
    };
    let first = common::run_limit(&[&input], rate, &config);
    println!(
        "iteration budget, rate={rate}: single pass peak={:.8} dBTP, ceiling_met={}",
        first.output_true_peak_dbtp, first.ceiling_met
    );
    assert!(!first.ceiling_met);
    assert_eq!(first.passes, 1);
    let initial_target = config.ceiling_dbtp - config.initial_margin_db;
    let next_target = initial_target - (first.output_true_peak_dbtp - config.ceiling_dbtp + 0.005);
    let corrected = Config {
        // The fresh call applies its own initial margin before rendering.
        ceiling_dbtp: next_target + config.initial_margin_db,
        ..config
    };
    let expected = common::run_limit(&[&input], rate, &corrected);
    assert_eq!(expected.passes, 1);
    let actual = common::run_limit(
        &[&input],
        rate,
        &Config {
            max_passes: 4,
            ..config
        },
    );
    assert_eq!(actual.passes, 2);
    println!(
        "iteration regression, rate={rate}: pass-1 rise={:.8} dB, ceiling excess={:.8} dB; passes={}, output={:.8} dBTP",
        first.output_true_peak_dbtp - initial_target,
        first.output_true_peak_dbtp - config.ceiling_dbtp,
        actual.passes,
        actual.output_true_peak_dbtp
    );
    assert_eq!(actual.channels, expected.channels);
    assert_eq!(actual.gain_db, expected.gain_db);
}
