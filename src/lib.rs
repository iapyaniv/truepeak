//! True-peak measurement and lookahead limiting for planar `f32` audio.
//!
//! Inputs contain 1–8 equal-length channels at 44.1, 48, 88.2, 96, 176.4, or
//! 192 kHz. Non-finite samples are rejected. An empty channel represents silence;
//! silence reads negative infinity in decibels.
//!
//! [`limit`] returns time-aligned audio and verifies its measured peak against
//! the requested ceiling. Inspect [`Output::ceiling_met`] before accepting a
//! render. [`Limiter`] exposes a single streaming pass with uncompensated latency.
//! [`Upsampler`] and [`Downsampler`] can also be used independently.
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//! use truepeak::{Config, limit};
//!
//! let left = [0.0_f32, 1.2, -1.2, 0.0];
//! let right = [0.0_f32, 0.3, -0.3, 0.0];
//! let config = Config {
//!     verify_bit_depth: Some(24),
//!     ..Config::default()
//! };
//! let output = limit(
//!     &[&left, &right],
//!     48_000,
//!     &config,
//!     &AtomicBool::new(false),
//!     &mut |_| {},
//! )?;
//! assert!(output.ceiling_met);
//! # Ok::<(), truepeak::Error>(())
//! ```
//!
//! The optional `serde` feature enables serialization of configuration and
//! result types. Negative infinity is not a JSON number; consumers serializing
//! silence need to choose a representation for it.

#![forbid(unsafe_code)]

pub mod limiter;
pub mod meter;
pub mod resample;

pub use limiter::{Config, Limiter, Link, Output, limit};
pub use meter::{
    ChannelSamplePeak, SamplePeak, TruePeak, sample_peak, sample_peak_with_control, true_peak,
    true_peak_per_channel, true_peak_per_channel_with_control, true_peak_with_control,
};
pub use resample::{Downsampler, Upsampler};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Counts base-rate samples across channels. Standalone meters use pass 1;
/// the limiter uses pass 0 for input measurement and its pass number thereafter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Progress {
    /// Input measurement is pass 0; limiter passes start at 1. Meters use pass 1.
    pub pass: u32,
    /// Completed base-rate samples across channels within the current pass.
    pub done_samples: usize,
    /// Work for this pass, including verification for an offline limiter pass.
    /// Quantized bypass extends pass 0's total after measuring the input.
    pub total_samples: usize,
}

pub(crate) const BLOCK: usize = 1024;

pub(crate) fn validate_rate(sample_rate_hz: u32) -> Result<(), Error> {
    if matches!(
        sample_rate_hz,
        44100 | 48000 | 88200 | 96000 | 176400 | 192000
    ) {
        Ok(())
    } else {
        Err(Error::InvalidInput(format!(
            "unsupported sample_rate_hz: {sample_rate_hz}"
        )))
    }
}

pub(crate) fn validate_layout(channels: &[&[f32]]) -> Result<usize, Error> {
    if !(1..=8).contains(&channels.len()) {
        return Err(Error::InvalidInput("channel count must be 1 to 8".into()));
    }
    let len = channels[0].len();
    if channels.iter().any(|channel| channel.len() != len) {
        return Err(Error::InvalidInput("channel lengths differ".into()));
    }
    Ok(len)
}

/// Bit depths use signed PCM codes, rounding ties away from zero and saturating
/// to the representable range. Output audio itself is never quantized here.
pub(crate) fn quantize_sample(sample: f32, bit_depth: u8) -> f32 {
    let scale = 2.0_f64.powi(bit_depth as i32 - 1);
    ((sample as f64 * scale).round().clamp(-scale, scale - 1.0) / scale) as f32
}
