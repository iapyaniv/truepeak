use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    BLOCK, Downsampler, Error, Progress, Upsampler, true_peak_with_control, validate_layout,
    validate_rate,
};

const PASS_MARGIN_DB: f64 = 0.005;

/// Whether channels share one gain envelope or use independent envelopes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Link {
    /// Apply the gain required by the largest refined peak across channels.
    #[default]
    Linked,
    /// Compute and apply gain separately for each channel.
    Unlinked,
}

/// Settings for offline and streaming limiting.
///
/// Constructors and [`limit`] validate all fields. Initial margin, pass budget,
/// export verification, and gain-hop settings apply only to offline [`limit`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// Requested ceiling in dBTP; finite and at most +6. Default: -1.
    pub ceiling_dbtp: f64,
    /// Headroom below the ceiling for the first offline pass, in dB.
    /// Finite, 0–0.1 inclusive; default 0.03. Does not affect bypass.
    pub initial_margin_db: f64,
    /// Lookahead in milliseconds; finite, 1–10 inclusive. Default: 2.
    pub lookahead_ms: f64,
    /// Fast recovery time constant in milliseconds; finite and positive. Default: 50.
    pub release_fast_ms: f64,
    /// Sustained-gain recovery time constant in milliseconds; finite and positive.
    /// Default: 500.
    pub release_slow_ms: f64,
    /// Sustained-gain attack time constant in milliseconds; finite and positive.
    /// Default: 50. Keeps short transients from holding down the slow envelope.
    pub slow_attack_ms: f64,
    /// Interpolation factor: 4, 8, or 16. Default: 8.
    pub oversampling: u32,
    /// Channel gain coupling. Default: [`Link::Linked`].
    pub link: Link,
    /// Maximum offline passes, each starting from the original input.
    /// In 1–8 inclusive; default 4. Bypass uses zero passes.
    pub max_passes: u32,
    /// `None` measures float output; `Some(16)` or `Some(24)` measures rounded,
    /// saturated signed PCM without dither. Returned audio stays float. Default: `None`.
    pub verify_bit_depth: Option<u8>,
    /// Offline gain-series hop in milliseconds; finite and positive. Default: 1.
    /// Rounded to base-rate samples with a minimum hop of one sample.
    pub gain_hop_ms: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ceiling_dbtp: -1.0,
            initial_margin_db: 0.03,
            lookahead_ms: 2.0,
            release_fast_ms: 50.0,
            release_slow_ms: 500.0,
            slow_attack_ms: 50.0,
            oversampling: 8,
            link: Link::Linked,
            max_passes: 4,
            verify_bit_depth: None,
            gain_hop_ms: 1.0,
        }
    }
}

impl Config {
    /// Returns [`Error::InvalidConfig`] identifying the first invalid field.
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = |field: &str| Error::InvalidConfig(field.into());
        if !self.ceiling_dbtp.is_finite() || self.ceiling_dbtp > 6.0 {
            return Err(invalid("ceiling_dbtp must be finite and at most +6"));
        }
        if !self.initial_margin_db.is_finite() || !(0.0..=0.1).contains(&self.initial_margin_db) {
            return Err(invalid("initial_margin_db must be finite and 0 to 0.1"));
        }
        if !self.lookahead_ms.is_finite() || !(1.0..=10.0).contains(&self.lookahead_ms) {
            return Err(invalid("lookahead_ms must be 1 to 10"));
        }
        for (field, value) in [
            ("release_fast_ms", self.release_fast_ms),
            ("release_slow_ms", self.release_slow_ms),
            ("slow_attack_ms", self.slow_attack_ms),
            ("gain_hop_ms", self.gain_hop_ms),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(invalid(&format!("{field} must be finite and positive")));
            }
        }
        if !matches!(self.oversampling, 4 | 8 | 16) {
            return Err(invalid("oversampling must be 4, 8, or 16"));
        }
        if !(1..=8).contains(&self.max_passes) {
            return Err(invalid("max_passes must be 1 to 8"));
        }
        if !matches!(self.verify_bit_depth, None | Some(16 | 24)) {
            return Err(invalid("verify_bit_depth must be None, 16, or 24"));
        }
        Ok(())
    }
}

/// Audio and measurements from the final offline pass, or from bypass.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Output {
    /// Planar float audio, time-aligned with the input and with the same frame count.
    pub channels: Vec<Vec<f32>>,
    /// Measured float input peak in dBTP; negative infinity for silence.
    pub input_true_peak_dbtp: f64,
    /// Measured output peak in dBTP, after optional PCM quantization for verification.
    pub output_true_peak_dbtp: f64,
    /// Requested ceiling, before applying internal margins.
    pub ceiling_dbtp: f64,
    /// Whether the measured output is at or below `ceiling_dbtp + 0.001`.
    /// Can be false after exhausting the pass budget or quantizing bypassed audio.
    pub ceiling_met: bool,
    /// Number of limiting passes; zero when the float input bypasses processing.
    pub passes: u32,
    /// Largest applied gain reduction in dB, across all channels of the final pass.
    pub max_reduction_db: f64,
    /// Minimum applied gain per hop, across channels, in dB; values are at most zero.
    pub gain_db: Vec<f32>,
    /// Base-rate frames per gain-series entry; the final hop can be shorter.
    pub gain_hop_samples: usize,
}

#[derive(Clone, Debug)]
struct BoxFilter {
    history: Vec<f64>,
    next: usize,
    sum: f64,
    reciprocal: f64,
}

impl BoxFilter {
    fn new(len: usize) -> Self {
        Self {
            history: vec![1.0; len],
            next: 0,
            sum: len as f64,
            reciprocal: 1.0 / len as f64,
        }
    }

    fn push(&mut self, value: f64) -> f64 {
        self.sum += value - self.history[self.next];
        self.history[self.next] = value;
        self.next += 1;
        if self.next == self.history.len() {
            self.next = 0;
        }
        self.sum * self.reciprocal
    }

    fn rebase(&mut self) {
        self.sum = self.history.iter().sum();
    }
}

#[derive(Clone, Debug)]
struct Envelope {
    slow: f64,
    fast: f64,
    attack: f64,
    slow_release: f64,
    fast_release: f64,
    minimum: VecDeque<(usize, f64)>,
    boxes: [BoxFilter; 3],
}

impl Envelope {
    fn new(config: &Config, rate: f64, window: usize) -> Self {
        let coefficient = |ms: f64| -(-1000.0 / (ms * rate)).exp_m1();
        Self {
            slow: 1.0,
            fast: 1.0,
            attack: coefficient(config.slow_attack_ms),
            slow_release: coefficient(config.release_slow_ms),
            fast_release: (-1000.0 / (config.release_fast_ms * rate)).exp(),
            minimum: VecDeque::with_capacity(window + 1),
            boxes: std::array::from_fn(|_| BoxFilter::new(window / 3)),
        }
    }

    fn push(&mut self, required: f64, index: usize, window: usize, quantum: usize) -> f64 {
        self.slow += (required - self.slow)
            * if required < self.slow {
                self.attack
            } else {
                self.slow_release
            };
        self.fast = required.min(self.slow - (self.slow - self.fast) * self.fast_release);
        while self
            .minimum
            .back()
            .is_some_and(|&(_, value)| value >= self.fast)
        {
            self.minimum.pop_back();
        }
        self.minimum.push_back((index, self.fast));
        let first = index.saturating_sub(window);
        while self.minimum.front().is_some_and(|&(n, _)| n < first) {
            self.minimum.pop_front();
        }
        let mut gain = self.minimum.front().unwrap().1;
        // Fixed boundaries preserve bit identity when callers repartition blocks.
        if index & (quantum - 1) == 0 {
            for filter in &mut self.boxes {
                filter.rebase();
            }
        }
        for filter in &mut self.boxes {
            gain = filter.push(gain);
        }
        gain.clamp(0.0, 1.0)
    }
}

/// A single causal pass. The offline `limit` function adds bypass and verification.
/// Output includes the leading latency and the FIR tail; neither is trimmed here.
#[derive(Clone, Debug)]
pub struct Limiter {
    config: Config,
    rate: u32,
    window: usize,
    latency: usize,
    up: Vec<Upsampler>,
    down: Vec<Downsampler>,
    envelopes: Vec<Envelope>,
    neighbors: Vec<[f64; 2]>,
    audio_delay: Vec<Vec<f32>>,
    gain_delay: Vec<Vec<f32>>,
    base_gain: Vec<f64>,
    oversampled: Vec<Vec<f32>>,
    index: usize,
    audio_next: usize,
    frames: usize,
    finished: bool,
}

impl Limiter {
    /// Validates config, rate, and channel count. Latency is always an integer
    /// number of base-rate frames, including both FIR delays and lookahead.
    pub fn new(sample_rate_hz: u32, channels: usize, config: &Config) -> Result<Self, Error> {
        config.validate()?;
        validate_rate(sample_rate_hz)?;
        if !(1..=8).contains(&channels) {
            return Err(Error::InvalidInput("channel count must be 1 to 8".into()));
        }
        let factor = config.oversampling as usize;
        let window =
            (config.lookahead_ms * sample_rate_hz as f64 / 1000.0).round() as usize * factor;
        let up = (0..channels)
            .map(|_| Upsampler::new(config.oversampling, sample_rate_hz))
            .collect::<Result<Vec<_>, _>>()?;
        let down = (0..channels)
            .map(|_| Downsampler::new(config.oversampling, sample_rate_hz))
            .collect::<Result<Vec<_>, _>>()?;
        let latency = up[0].delay() / factor + down[0].delay() + window / factor;
        let envelopes = (0..if config.link == Link::Linked {
            1
        } else {
            channels
        })
            .map(|_| Envelope::new(config, sample_rate_hz as f64 * factor as f64, window))
            .collect();
        let gain_delay = vec![vec![1.0; down[0].delay()]; channels];
        Ok(Self {
            config: config.clone(),
            rate: sample_rate_hz,
            window,
            latency,
            up,
            down,
            envelopes,
            neighbors: vec![[0.0; 2]; channels],
            audio_delay: vec![vec![0.0; window]; channels],
            gain_delay,
            base_gain: vec![1.0; channels],
            oversampled: (0..channels)
                .map(|_| Vec::with_capacity(BLOCK * factor))
                .collect(),
            index: 0,
            audio_next: 0,
            frames: 0,
            finished: false,
        })
    }

    /// Leading delay in base-rate frames: both FIR delays plus rounded lookahead.
    pub fn latency_samples(&self) -> usize {
        self.latency
    }

    /// Appends one causal output frame per input frame. Invalid input changes
    /// neither state nor output. Callers bound blocks and handle cancellation.
    pub fn process(&mut self, input: &[&[f32]], output: &mut [Vec<f32>]) -> Result<(), Error> {
        self.process_inner(input, output, None)
    }

    /// Also appends per-channel linear gains at base rate, aligned with the
    /// causal audio output. These are the minima over each oversampled frame.
    pub fn process_with_gain(
        &mut self,
        input: &[&[f32]],
        output: &mut [Vec<f32>],
        gain: &mut [Vec<f32>],
    ) -> Result<(), Error> {
        self.process_inner(input, output, Some(gain))
    }

    fn process_inner(
        &mut self,
        input: &[&[f32]],
        output: &mut [Vec<f32>],
        gain: Option<&mut [Vec<f32>]>,
    ) -> Result<(), Error> {
        if self.finished {
            return Err(Error::InvalidInput(
                "process after flush requires reset".into(),
            ));
        }
        let len = validate_layout(input)?;
        if input.len() != self.up.len()
            || output.len() != input.len()
            || gain.as_ref().is_some_and(|g| g.len() != input.len())
        {
            return Err(Error::InvalidInput("stream channel count differs".into()));
        }
        if input
            .iter()
            .any(|channel| channel.iter().any(|v| !v.is_finite()))
        {
            return Err(Error::InvalidInput("non-finite sample".into()));
        }
        let frames = self
            .frames
            .checked_add(len)
            .filter(|n| n.checked_mul(self.config.oversampling as usize).is_some())
            .ok_or_else(|| Error::InvalidInput("input length overflow".into()))?;
        let mut gain = gain;
        let mut oversampled = std::mem::take(&mut self.oversampled);
        let result = (|| {
            // Fixed-size scratch bounds memory even if the caller supplies a whole file.
            for start in (0..len).step_by(BLOCK) {
                let end = (start + BLOCK).min(len);
                for ((up, buffer), channel) in self.up.iter_mut().zip(&mut oversampled).zip(input) {
                    buffer.clear();
                    up.process(&channel[start..end], buffer)?;
                }
                self.apply(&mut oversampled, output, gain.as_deref_mut())?;
            }
            Ok(())
        })();
        self.oversampled = oversampled;
        result?;
        self.frames = frames;
        Ok(())
    }

    fn apply(
        &mut self,
        audio: &mut [Vec<f32>],
        output: &mut [Vec<f32>],
        mut gains: Option<&mut [Vec<f32>]>,
    ) -> Result<(), Error> {
        let ceiling = 10.0_f64.powf(self.config.ceiling_dbtp / 20.0);
        let factor = self.config.oversampling as usize;
        let mut applied = [1.0_f64; 8];
        let mut peaks = [0.0_f64; 8];
        for n in 0..audio[0].len() {
            for (channel, neighbors) in self.neighbors.iter_mut().enumerate() {
                let right = audio[channel][n] as f64;
                let [left, center] = *neighbors;
                peaks[channel] = if self.index >= 2 {
                    crate::meter::refine_peak(left, center, right)
                        .0
                        .abs()
                        .max(center.abs())
                } else {
                    center.abs()
                };
                *neighbors = [center, right];
            }
            // Smoother support <= W leaves room for this one-sample detector delay
            // without extending the W-sample audio delay.
            if self.config.link == Link::Linked {
                let peak = peaks[..audio.len()].iter().copied().fold(0.0_f64, f64::max);
                let required = if peak <= ceiling { 1.0 } else { ceiling / peak };
                applied[..audio.len()].fill(self.envelopes[0].push(
                    required,
                    self.index,
                    self.window,
                    BLOCK * factor,
                ));
            } else {
                for (channel, envelope) in self.envelopes.iter_mut().enumerate() {
                    let peak = peaks[channel];
                    let required = if peak <= ceiling { 1.0 } else { ceiling / peak };
                    applied[channel] =
                        envelope.push(required, self.index, self.window, BLOCK * factor);
                }
            }
            let slot = self.audio_next;
            for (channel, buffer) in audio.iter_mut().enumerate() {
                let delayed = self.audio_delay[channel][slot];
                self.audio_delay[channel][slot] = buffer[n];
                buffer[n] = (delayed as f64 * applied[channel]) as f32;
                self.base_gain[channel] = self.base_gain[channel].min(applied[channel]);
            }
            self.audio_next += 1;
            if self.audio_next == self.window {
                self.audio_next = 0;
            }
            self.index += 1;
            if self.index & (factor - 1) == 0 {
                let frame = self.index / factor - 1;
                for channel in 0..audio.len() {
                    let slot = frame % self.gain_delay[channel].len();
                    let previous = self.gain_delay[channel][slot];
                    self.gain_delay[channel][slot] = self.base_gain[channel] as f32;
                    if let Some(gains) = &mut gains {
                        gains[channel].push(previous);
                    }
                    self.base_gain[channel] = 1.0;
                }
            }
        }
        for ((down, channel), output) in self.down.iter_mut().zip(audio).zip(output) {
            down.process(channel, output)?;
        }
        Ok(())
    }

    /// Appends the delayed audio and filter tail once. An empty stream has no tail.
    /// Further processing requires [`Self::reset`]. Output must have one vector per channel.
    pub fn flush(&mut self, output: &mut [Vec<f32>]) -> Result<(), Error> {
        self.flush_inner(output, None)
    }

    /// Flushes audio and aligned linear gains, each into one vector per channel.
    /// Repeated calls append nothing; an invalid layout leaves state and output unchanged.
    pub fn flush_with_gain(
        &mut self,
        output: &mut [Vec<f32>],
        gain: &mut [Vec<f32>],
    ) -> Result<(), Error> {
        self.flush_inner(output, Some(gain))
    }

    fn flush_inner(
        &mut self,
        output: &mut [Vec<f32>],
        mut gain: Option<&mut [Vec<f32>]>,
    ) -> Result<(), Error> {
        if output.len() != self.up.len() || gain.as_ref().is_some_and(|g| g.len() != self.up.len())
        {
            return Err(Error::InvalidInput("stream channel count differs".into()));
        }
        if self.finished {
            return Ok(());
        }
        if self.frames != 0 {
            let mut tail = vec![Vec::new(); self.up.len()];
            for (up, channel) in self.up.iter_mut().zip(&mut tail) {
                up.flush(channel);
            }
            self.apply(&mut tail, output, gain.as_deref_mut())?;
            for channel in &mut tail {
                channel.clear();
                channel.resize(self.window, 0.0);
            }
            self.apply(&mut tail, output, gain.as_deref_mut())?;
            for (channel, down) in self.down.iter_mut().enumerate() {
                let before = output[channel].len();
                down.flush(&mut output[channel]);
                if let Some(gain) = &mut gain {
                    for n in 0..output[channel].len() - before {
                        let slot = (self.index / self.config.oversampling as usize + n)
                            % self.gain_delay[channel].len();
                        gain[channel].push(self.gain_delay[channel][slot]);
                        self.gain_delay[channel][slot] = 1.0;
                    }
                }
            }
        }
        self.finished = true;
        Ok(())
    }

    /// Starts a new stream with the same configuration, rate, and channel count.
    /// Rebuilds filter state and scratch buffers.
    pub fn reset(&mut self) {
        *self =
            Self::new(self.rate, self.up.len(), &self.config).expect("previously validated config");
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// Limits and verifies a whole input, returning time-aligned float audio.
///
/// Float input at or below the ceiling is copied unchanged with zero passes.
/// Optional PCM verification also measures bypassed audio, which can exceed the
/// ceiling after rounding. Each retry starts from the original input. The last
/// result is returned even if the pass budget is exhausted; inspect
/// [`Output::ceiling_met`] to decide whether to accept it.
///
/// Progress uses pass 0 for input measurement and starts at 1 for limiting.
/// Each limiting pass counts processing and verification across all channels.
/// Cancellation is checked at block boundaries, including verification, and
/// returns [`Error::Cancelled`] without partial output. Invalid settings return
/// [`Error::InvalidConfig`]; unsupported rates, layouts, or non-finite samples
/// return [`Error::InvalidInput`].
pub fn limit(
    input: &[&[f32]],
    sample_rate_hz: u32,
    config: &Config,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<Output, Error> {
    config.validate()?;
    let len = validate_layout(input)?;
    validate_rate(sample_rate_hz)?;
    cancelled(cancel)?;
    let work = len
        .checked_mul(input.len())
        .ok_or_else(|| Error::InvalidInput("input length overflow".into()))?;
    let total = work
        .checked_mul(2)
        .ok_or_else(|| Error::InvalidInput("input length overflow".into()))?;
    let input_peak = true_peak_with_control(
        input,
        sample_rate_hz,
        config.oversampling,
        cancel,
        &mut |mut p| {
            p.pass = 0;
            progress(p);
        },
    )?;
    let hop = (config.gain_hop_ms * sample_rate_hz as f64 / 1000.0)
        .round()
        .max(1.0) as usize;
    if input_peak.dbtp <= config.ceiling_dbtp {
        let mut channels = Vec::with_capacity(input.len());
        for channel in input {
            let mut copy = Vec::with_capacity(len);
            for block in channel.chunks(BLOCK) {
                cancelled(cancel)?;
                copy.extend_from_slice(block);
            }
            channels.push(copy);
        }
        let measured = if config.verify_bit_depth.is_some() {
            crate::meter::export_peak_with_control(
                input,
                sample_rate_hz,
                config.oversampling,
                config.verify_bit_depth,
                cancel,
                &mut |p| {
                    progress(Progress {
                        pass: 0,
                        done_samples: work + p.done_samples,
                        total_samples: total,
                    });
                },
            )?
        } else {
            input_peak.dbtp
        };
        cancelled(cancel)?;
        return Ok(Output {
            channels,
            input_true_peak_dbtp: input_peak.dbtp,
            output_true_peak_dbtp: measured,
            ceiling_dbtp: config.ceiling_dbtp,
            ceiling_met: measured <= config.ceiling_dbtp + 0.001,
            passes: 0,
            max_reduction_db: 0.0,
            gain_db: vec![0.0; len.div_ceil(hop)],
            gain_hop_samples: hop,
        });
    }
    let mut target = config.clone();
    target.ceiling_dbtp -= config.initial_margin_db;
    for pass in 1..=config.max_passes {
        let mut stream = Limiter::new(sample_rate_hz, input.len(), &target)?;
        let latency = stream.latency_samples();
        let mut channels = vec![Vec::with_capacity(len + latency * 2); input.len()];
        let mut gain = vec![Vec::with_capacity(len + latency * 2); input.len()];
        for start in (0..len).step_by(BLOCK) {
            cancelled(cancel)?;
            let end = (start + BLOCK).min(len);
            let block: Vec<&[f32]> = input.iter().map(|channel| &channel[start..end]).collect();
            stream.process_with_gain(&block, &mut channels, &mut gain)?;
            progress(Progress {
                pass,
                done_samples: end * input.len(),
                total_samples: total,
            });
            cancelled(cancel)?;
        }
        stream.flush_with_gain(&mut channels, &mut gain)?;
        for channel in &mut channels {
            channel.drain(..latency);
            channel.truncate(len);
        }
        for channel in &mut gain {
            channel.drain(..latency);
            channel.truncate(len);
        }
        cancelled(cancel)?;
        let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
        let measured = crate::meter::export_peak_with_control(
            &refs,
            sample_rate_hz,
            config.oversampling,
            config.verify_bit_depth,
            cancel,
            &mut |p| {
                progress(Progress {
                    pass,
                    done_samples: work + p.done_samples,
                    total_samples: total,
                });
            },
        )?;
        let ceiling_met = measured <= config.ceiling_dbtp + 0.001;
        if measured <= config.ceiling_dbtp || pass == config.max_passes {
            let mut series = vec![1.0_f32; len.div_ceil(hop)];
            let mut minimum = 1.0_f32;
            for channel in &gain {
                for (index, block) in channel.chunks(hop).enumerate() {
                    cancelled(cancel)?;
                    let value = block.iter().copied().fold(1.0_f32, f32::min);
                    series[index] = series[index].min(value);
                    minimum = minimum.min(value);
                }
            }
            for value in &mut series {
                *value = 20.0 * value.log10();
            }
            cancelled(cancel)?;
            return Ok(Output {
                channels,
                input_true_peak_dbtp: input_peak.dbtp,
                output_true_peak_dbtp: measured,
                ceiling_dbtp: config.ceiling_dbtp,
                ceiling_met,
                passes: pass,
                max_reduction_db: -20.0 * (minimum as f64).log10(),
                gain_db: series,
                gain_hop_samples: hop,
            });
        }
        target.ceiling_dbtp -= measured - config.ceiling_dbtp + PASS_MARGIN_DB;
    }
    unreachable!()
}
