mod common;

use std::{fs, path::PathBuf};
use truepeak::true_peak_per_channel;

fn measure(sequence: u32, expected: f64) {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/ebu");
    let prefix = format!("seq-3341-{sequence}-");
    let path = fs::read_dir(&directory).ok().and_then(|entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix)
                    && path.extension().is_some_and(|ext| ext == "wav")
            })
    });
    if !common::oracle_available(
        path.is_some(),
        &format!("EBU sequence {sequence} absent; run `just validate`"),
    ) {
        return;
    }
    let path = path.unwrap();
    let mut reader = hound::WavReader::open(&path).unwrap();
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = 2.0_f64.powi(spec.bits_per_sample as i32 - 1);
            reader
                .samples::<i32>()
                .map(|sample| (sample.unwrap() as f64 / scale) as f32)
                .collect()
        }
    };
    let mut channels = vec![Vec::new(); spec.channels as usize];
    for frame in samples.chunks_exact(spec.channels as usize) {
        for (channel, &sample) in channels.iter_mut().zip(frame) {
            channel.push(sample);
        }
    }
    let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    // EBU Tech 3341 (2023), Table 1, cases 15–20 specifies these readings
    // and the asymmetric tolerance: https://tech.ebu.ch/docs/tech/tech3341.pdf
    for factor in [4, 8, 16] {
        for peak in true_peak_per_channel(&refs, spec.sample_rate, factor).unwrap() {
            let error = peak.dbtp - expected;
            println!(
                "EBU seq-3341-{sequence}, L={factor}, channel={}: {:.8} dBTP, expected={expected:.1}, error={error:+.8} dB",
                peak.channel, peak.dbtp
            );
            assert!(
                (-0.4..=0.2).contains(&error),
                "{}: L={factor}, channel={}, error={error:+.8} dB",
                path.display(),
                peak.channel
            );
        }
    }
}

#[test]
fn seq_3341_15() {
    measure(15, -6.0);
}
#[test]
fn seq_3341_16() {
    measure(16, -6.0);
}
#[test]
fn seq_3341_17() {
    measure(17, -6.0);
}
#[test]
fn seq_3341_18() {
    measure(18, -6.0);
}
#[test]
fn seq_3341_19() {
    measure(19, 3.0);
}
#[test]
fn seq_3341_20() {
    measure(20, 0.0);
}
