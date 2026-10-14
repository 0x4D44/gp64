//! CPAL boundary. The emulator and the callback share only a lock-free ring.
use crate::{
    output::{MuteGain, OutputState},
    ring::{ring, Consumer, Producer},
};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct Output {
    pub rate: u32,
    /// Taken once by the emulation thread.
    pub producer: Option<Producer>,
    /// Host mute: the callback outputs silence but keeps consuming.
    pub muted: Arc<AtomicBool>,
    _stream: cpal::Stream,
}
impl Output {
    pub fn open(muted: bool) -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device; use --mute")?;
        let supported = device.default_output_config()?;
        let rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.clone().into();
        // About 400 ms of headroom; the callback keeps the fill near its target.
        let (producer, consumer) = ring((rate as usize * 400 / 1000).max(1));
        let muted = Arc::new(AtomicBool::new(muted));
        type Build = fn(
            &cpal::Device,
            &cpal::StreamConfig,
            u32,
            Consumer,
            Arc<AtomicBool>,
        ) -> Result<cpal::Stream>;
        let build: Build = match supported.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>,
            cpal::SampleFormat::I16 => stream::<i16>,
            cpal::SampleFormat::U16 => stream::<u16>,
            cpal::SampleFormat::I8 => stream::<i8>,
            cpal::SampleFormat::U8 => stream::<u8>,
            cpal::SampleFormat::I32 => stream::<i32>,
            cpal::SampleFormat::U32 => stream::<u32>,
            cpal::SampleFormat::I64 => stream::<i64>,
            cpal::SampleFormat::U64 => stream::<u64>,
            cpal::SampleFormat::F64 => stream::<f64>,
            format => bail!("unsupported audio format {format}; use --mute"),
        };
        let stream = build(&device, &config, rate, consumer, muted.clone())?;
        stream.play()?;
        Ok(Self {
            rate,
            producer: Some(producer),
            muted,
            _stream: stream,
        })
    }
}
fn stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    rate: u32,
    mut consumer: Consumer,
    muted: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let mut state = OutputState::new(rate);
    let mut gain = MuteGain::new(rate, muted.load(Ordering::Relaxed));
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            let muted = muted.load(Ordering::Relaxed);
            state.render(&mut consumer, frames, |index, sample| {
                let sample = gain.apply(muted, sample);
                data[index * channels..][..channels].fill(T::from_sample(sample));
            });
        },
        |error| eprintln!("gp64: audio output: {error}"),
        None,
    )?)
}
