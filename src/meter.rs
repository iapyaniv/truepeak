use std::sync::atomic::{AtomicBool, Ordering};

use crate::{BLOCK, Error, Progress, Upsampler, validate_layout, validate_rate};

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TruePeak {
    /// Refined absolute peak in dBTP; negative infinity for silence.
    pub dbtp: f64,
    /// Absolute peak amplitude; can exceed one for inter-sample peaks.
    pub linear: f64,
    /// Zero-based input channel index.
    pub channel: usize,
    /// Relative to input sample 0, after removing interpolation delay.
    /// Zero padding can place a boundary peak slightly outside the input duration.
    pub position_seconds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChannelSamplePeak {
    /// Absolute sample maximum in dBFS; negative infinity for silence.
    pub dbfs: f64,
    /// Exact absolute input maximum, converted losslessly from `f32` to `f64`.
    pub linear: f64,
    /// Zero-based input channel index.
    pub channel: usize,
    /// Earliest index attaining the maximum; zero for empty or silent input.
    pub position_samples: usize,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SamplePeak {
    /// Largest peak across channels; ties choose the first channel.
    pub overall: ChannelSamplePeak,
    /// Peaks in input channel order.
    pub per_channel: Vec<ChannelSamplePeak>,
}

fn db(linear: f64) -> f64 {
    20.0 * linear.log10()
}

pub(crate) fn refine_peak(left: f64, center: f64, right: f64) -> (f64, f64) {
    if center.abs() >= left.abs() && center.abs() >= right.abs() {
        let curvature = right - 2.0 * center + left;
        let offset = if curvature == 0.0 {
            0.0
        } else {
            (left - right) / (2.0 * curvature)
        };
        if curvature != 0.0 && offset.abs() <= 0.5 {
            return (center - (right - left).powi(2) / (8.0 * curvature), offset);
        }
    }
    (center, 0.0)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn report(
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
    pass: u32,
    done_samples: usize,
    total_samples: usize,
) -> Result<(), Error> {
    progress(Progress {
        pass,
        done_samples,
        total_samples,
    });
    check_cancel(cancel)
}

/// Uses 4x, 8x, or 16x interpolation; 8x is recommended.
/// Phase-sweep fixtures require readings within 0.02 dB of known tone peaks.
/// Ties choose the first channel. Empty channels and silence read negative infinity.
/// For long input, use `true_peak_with_control` to support cancellation and progress.
pub fn true_peak(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
) -> Result<TruePeak, Error> {
    true_peak_with_control(
        channels,
        sample_rate_hz,
        oversampling,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

/// Measures the overall peak with the validation and padding rules of [`true_peak`].
/// Progress counts base-rate samples across channels with pass 1. Cancellation
/// is checked at block boundaries and returns [`Error::Cancelled`] without partial results.
pub fn true_peak_with_control(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<TruePeak, Error> {
    let peaks = true_peak_per_channel_with_control(
        channels,
        sample_rate_hz,
        oversampling,
        cancel,
        progress,
    )?;
    let mut best = peaks[0];
    for peak in peaks.into_iter().skip(1) {
        if peak.linear > best.linear {
            best = peak;
        }
    }
    Ok(best)
}

/// Same validation and zero-padding semantics as [`true_peak`], in input channel order.
pub fn true_peak_per_channel(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
) -> Result<Vec<TruePeak>, Error> {
    true_peak_per_channel_with_control(
        channels,
        sample_rate_hz,
        oversampling,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

/// Refines every local maximum in one scan, retaining only two neighboring samples.
/// Progress counts base-rate samples across channels; cancellation returns no partial peaks.
pub fn true_peak_per_channel_with_control(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<TruePeak>, Error> {
    measure_peaks(
        channels,
        sample_rate_hz,
        oversampling,
        cancel,
        progress,
        BLOCK,
        None,
    )
}

fn measure_peaks(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
    block_size: usize,
    bit_depth: Option<u8>,
) -> Result<Vec<TruePeak>, Error> {
    let len = validate_layout(channels)?;
    validate_rate(sample_rate_hz)?;
    if !matches!(oversampling, 4 | 8 | 16) {
        return Err(Error::InvalidConfig(
            "oversampling must be 4, 8, or 16".into(),
        ));
    }
    check_cancel(cancel)?;
    let mut up = Upsampler::new(oversampling, sample_rate_hz)?;
    let delay = up.delay();
    let total = len
        .checked_mul(channels.len())
        .ok_or_else(|| Error::InvalidInput("input length overflow".into()))?;
    let mut done = 0;
    report(cancel, progress, 1, done, total)?;
    let mut output = Vec::with_capacity(block_size * oversampling as usize + 2 * delay);
    let mut results = Vec::with_capacity(channels.len());
    let mut quantized = Vec::with_capacity(block_size);
    for (channel, input) in channels.iter().enumerate() {
        up.reset();
        let mut index = 0;
        let mut best = 0.0;
        let mut position = 0.0;
        let mut neighbors = [0.0_f64; 2];
        let mut consume = |samples: &[f32]| {
            for &sample in samples {
                let right = sample as f64;
                if index >= 2 {
                    let [left, center] = neighbors;
                    let (value, offset) = refine_peak(left, center, right);
                    if value.abs() > best {
                        best = value.abs();
                        position = index as f64 - 1.0 + offset;
                    }
                }
                if right.abs() > best {
                    best = right.abs();
                    position = index as f64;
                }
                neighbors = [neighbors[1], right];
                index += 1;
            }
        };
        for block in input.chunks(block_size) {
            check_cancel(cancel)?;
            output.clear();
            if let Some(bits) = bit_depth {
                quantized.clear();
                quantized.extend(block.iter().map(|&v| crate::quantize_sample(v, bits)));
                up.process(&quantized, &mut output)?;
            } else {
                up.process(block, &mut output)?;
            }
            consume(&output);
            done += block.len();
            report(cancel, progress, 1, done, total)?;
        }
        output.clear();
        up.flush(&mut output);
        consume(&output);
        check_cancel(cancel)?;
        results.push(TruePeak {
            dbtp: db(best),
            linear: best,
            channel,
            position_seconds: if best == 0.0 {
                0.0
            } else {
                (position - delay as f64) / (sample_rate_hz as f64 * oversampling as f64)
            },
        });
    }
    report(cancel, progress, 1, done, total)?;
    Ok(results)
}

pub(crate) fn export_peak_with_control(
    channels: &[&[f32]],
    sample_rate_hz: u32,
    oversampling: u32,
    bit_depth: Option<u8>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<f64, Error> {
    let peaks = measure_peaks(
        channels,
        sample_rate_hz,
        oversampling,
        cancel,
        progress,
        BLOCK,
        bit_depth,
    )?;
    Ok(peaks
        .iter()
        .map(|p| p.dbtp)
        .fold(f64::NEG_INFINITY, f64::max))
}

/// Exact absolute f32 maxima, converted losslessly to f64. Ties within a channel use the earliest
/// sample; ties across channels use the first channel. Rejects non-finite samples and invalid layouts.
pub fn sample_peak(channels: &[&[f32]]) -> Result<SamplePeak, Error> {
    sample_peak_with_control(channels, &AtomicBool::new(false), &mut |_| {})
}

/// Measures sample maxima with the validation and tie rules of [`sample_peak`].
/// Progress counts base-rate samples across channels with pass 1. Cancellation
/// is checked at block boundaries and returns [`Error::Cancelled`] without partial results.
pub fn sample_peak_with_control(
    channels: &[&[f32]],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<SamplePeak, Error> {
    let len = validate_layout(channels)?;
    check_cancel(cancel)?;
    let total = len
        .checked_mul(channels.len())
        .ok_or_else(|| Error::InvalidInput("input length overflow".into()))?;
    let mut done = 0;
    report(cancel, progress, 1, done, total)?;
    let mut per_channel = Vec::with_capacity(channels.len());
    for (channel, input) in channels.iter().enumerate() {
        let mut peak = ChannelSamplePeak {
            dbfs: f64::NEG_INFINITY,
            linear: 0.0,
            channel,
            position_samples: 0,
        };
        for block in input.chunks(BLOCK) {
            check_cancel(cancel)?;
            for (index, &sample) in block.iter().enumerate() {
                if !sample.is_finite() {
                    return Err(Error::InvalidInput("non-finite sample".into()));
                }
                let value = (sample as f64).abs();
                if value > peak.linear {
                    peak.linear = value;
                    peak.position_samples = done - channel * len + index;
                }
            }
            done += block.len();
            report(cancel, progress, 1, done, total)?;
        }
        peak.dbfs = db(peak.linear);
        per_channel.push(peak);
    }
    let mut overall = per_channel[0];
    for &peak in &per_channel[1..] {
        if peak.linear > overall.linear {
            overall = peak;
        }
    }
    Ok(SamplePeak {
        overall,
        per_channel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refined_peaks_cross_block_boundaries_without_changing_results() {
        let mut input = vec![0.0; 5003];
        for (index, value) in [
            (6, 0.3),
            (63, -0.7),
            (1023, 0.9),
            (4095, -1.0),
            (5002, 0.99),
        ] {
            input[index] = value;
            input[index - 1] = value * 0.6;
        }
        for factor in [4, 8, 16] {
            let analyze = |block_size| {
                measure_peaks(
                    &[&input],
                    44100,
                    factor,
                    &AtomicBool::new(false),
                    &mut |_| {},
                    block_size,
                    None,
                )
                .unwrap()
            };
            let expected = analyze(input.len());
            for block in [1, 7, 64, 4096] {
                assert_eq!(analyze(block), expected, "L={factor}, block={block}");
            }
        }
    }
}
