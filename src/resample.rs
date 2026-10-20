use std::f64::consts::PI;

use crate::{Error, validate_rate};

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..100 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
        if term < sum * f64::EPSILON {
            break;
        }
    }
    sum
}

fn prototype(factor: u32, base_rate_hz: u32) -> Result<Vec<f64>, Error> {
    validate_rate(base_rate_hz)?;
    if !matches!(factor, 4 | 8 | 16) {
        return Err(Error::InvalidConfig("factor must be 4, 8, or 16".into()));
    }
    let factor = factor as usize;
    let attenuation = 100.0;
    let beta = 0.1102 * (attenuation - 8.7);
    let transition = PI * 0.186 / factor as f64;
    let order = ((attenuation - 8.0) / (2.285 * transition)).ceil() as usize;
    // The Kaiser estimate is approximate. A 10% length margin meets 100 dB
    // at the transition edge; multiples of 2L give integer delay both ways.
    let order = ((order as f64 * 1.1).ceil() as usize).div_ceil(2 * factor) * 2 * factor;
    let mid = order / 2;
    let window_scale = bessel_i0(beta);
    let mut taps = vec![0.0; order + 1];
    for n in 0..=mid {
        let distance = n as f64 - mid as f64;
        let sinc = if distance == 0.0 {
            1.0 / factor as f64
        } else {
            (PI * distance / factor as f64).sin() / (PI * distance)
        };
        let r = distance / mid as f64;
        let window = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / window_scale;
        taps[n] = sinc * window;
        taps[order - n] = taps[n];
    }
    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap /= sum;
    }
    Ok(taps)
}

#[derive(Clone, Debug)]
struct History {
    samples: Vec<f64>,
    next: usize,
    len: usize,
}

impl History {
    fn new(len: usize) -> Self {
        Self {
            samples: vec![0.0; 2 * len],
            next: 0,
            len,
        }
    }

    fn push(&mut self, sample: f32) {
        self.next = if self.next == 0 {
            self.len - 1
        } else {
            self.next - 1
        };
        self.samples[self.next] = sample as f64;
        self.samples[self.next + self.len] = sample as f64;
    }

    fn dot(&self, taps: &[f64]) -> f32 {
        let samples = &self.samples[self.next..self.next + taps.len()];
        // Fixed pairwise groups shorten the reduction dependency chain while
        // preserving f64 precision and identical ordering across stream blocks.
        let mut sum = 0.0;
        let mut coefficients = taps.chunks_exact(8);
        let mut history = samples.chunks_exact(8);
        for (t, x) in coefficients.by_ref().zip(history.by_ref()) {
            let a = t[0] * x[0] + t[1] * x[1];
            let b = t[2] * x[2] + t[3] * x[3];
            let c = t[4] * x[4] + t[5] * x[5];
            let d = t[6] * x[6] + t[7] * x[7];
            sum += (a + b) + (c + d);
        }
        for (&tap, &sample) in coefficients.remainder().iter().zip(history.remainder()) {
            sum += tap * sample;
        }
        sum as f32
    }

    fn reset(&mut self) {
        self.samples.fill(0.0);
        self.next = 0;
    }
}

fn validate_block(input: &[f32], finished: bool) -> Result<(), Error> {
    if finished {
        return Err(Error::InvalidInput(
            "process after flush requires reset".into(),
        ));
    }
    if input.iter().any(|sample| !sample.is_finite()) {
        return Err(Error::InvalidInput("non-finite sample".into()));
    }
    Ok(())
}

/// Causal, zero-padded interpolation. State size depends only on the filter.
#[derive(Clone, Debug)]
pub struct Upsampler {
    phases: Vec<Vec<f64>>,
    history: History,
    delay: usize,
    active: bool,
    finished: bool,
}

impl Upsampler {
    /// Accepts factors 4, 8, or 16 and any base rate supported by the crate.
    /// Invalid factors return [`Error::InvalidConfig`]; invalid rates return [`Error::InvalidInput`].
    pub fn new(factor: u32, base_rate_hz: u32) -> Result<Self, Error> {
        let taps = prototype(factor, base_rate_hz)?;
        let factor = factor as usize;
        let phases: Vec<Vec<f64>> = (0..factor)
            .map(|phase| {
                taps.iter()
                    .skip(phase)
                    .step_by(factor)
                    .map(|tap| tap * factor as f64)
                    .collect()
            })
            .collect();
        let history = History::new(phases[0].len());
        Ok(Self {
            phases,
            history,
            delay: (taps.len() - 1) / 2,
            active: false,
            finished: false,
        })
    }

    /// Appends exactly `input.len() * factor` samples, retaining the causal delay.
    /// An invalid block leaves both state and output unchanged. After flush, reset first.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), Error> {
        validate_block(input, self.finished)?;
        for &sample in input {
            self.push(sample, output);
        }
        self.active |= !input.is_empty();
        Ok(())
    }

    fn push(&mut self, sample: f32, output: &mut Vec<f32>) {
        self.history.push(sample);
        output.extend(self.phases.iter().map(|phase| self.history.dot(phase)));
    }

    /// Appends `2 * delay()` tail samples once; an empty stream has no tail.
    /// Samples outside the supplied stream are zero, including before its start.
    pub fn flush(&mut self, output: &mut Vec<f32>) {
        if self.active && !self.finished {
            for _ in 0..self.history.len - 1 {
                self.push(0.0, output);
            }
        }
        self.finished = true;
    }

    /// Exact group delay in oversampled output samples.
    pub fn delay(&self) -> usize {
        self.delay
    }

    /// Clears history and permits processing a new stream after flush.
    pub fn reset(&mut self) {
        self.history.reset();
        self.active = false;
        self.finished = false;
    }
}

/// Causal FIR decimation, retaining every Lth filtered sample starting at index 0.
#[derive(Clone, Debug)]
pub struct Downsampler {
    taps: Vec<f64>,
    factor: usize,
    history: History,
    phase: usize,
    active: bool,
    finished: bool,
}

impl Downsampler {
    /// Accepts factors 4, 8, or 16; `base_rate_hz` is the output sample rate.
    /// Invalid factors return [`Error::InvalidConfig`]; invalid rates return [`Error::InvalidInput`].
    pub fn new(factor: u32, base_rate_hz: u32) -> Result<Self, Error> {
        let taps = prototype(factor, base_rate_hz)?;
        let history = History::new(taps.len());
        Ok(Self {
            taps,
            factor: factor as usize,
            history,
            phase: 0,
            active: false,
            finished: false,
        })
    }

    /// Appends filtered base-rate samples. Partial L-sample groups carry across calls.
    /// An invalid block leaves both state and output unchanged. After flush, reset first.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), Error> {
        validate_block(input, self.finished)?;
        for &sample in input {
            self.push(sample, output);
        }
        self.active |= !input.is_empty();
        Ok(())
    }

    fn push(&mut self, sample: f32, output: &mut Vec<f32>) {
        self.history.push(sample);
        if self.phase == 0 {
            output.push(self.history.dot(&self.taps));
        }
        self.phase = (self.phase + 1) & (self.factor - 1);
    }

    /// Drains the FIR using zero padding, preserving the decimation phase.
    /// Repeated flushes and flushing an empty stream append nothing.
    pub fn flush(&mut self, output: &mut Vec<f32>) {
        if self.active && !self.finished {
            for _ in 0..self.taps.len() - 1 {
                self.push(0.0, output);
            }
        }
        self.finished = true;
    }

    /// Exact group delay in base-rate output samples.
    pub fn delay(&self) -> usize {
        (self.taps.len() - 1) / (2 * self.factor)
    }

    /// Clears history and decimation phase, permitting a new stream after flush.
    pub fn reset(&mut self) {
        self.history.reset();
        self.phase = 0;
        self.active = false;
        self.finished = false;
    }
}
