//! Microphone capture with cpal (WASAPI on Windows, CoreAudio on macOS).
//!
//! The input stream is opened once and then only paused/resumed, so starting a dictation costs a
//! few milliseconds instead of a device open. While paused, the OS does not consider the
//! microphone in use.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use crossbeam_channel::{Receiver, Sender};

struct Live {
    stream: cpal::Stream,
    rate: u32,
    device_id: Option<cpal::DeviceId>,
    name: String,
}

pub struct Mic {
    preferred: String,
    live: Option<Live>,
    tx: Sender<Vec<f32>>,
    rx: Receiver<Vec<f32>>,
    failed: Arc<AtomicBool>,
}

impl Mic {
    /// `preferred`: empty for the system default device, otherwise part of a device name.
    pub fn new(preferred: &str) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            preferred: preferred.trim().to_lowercase(),
            live: None,
            tx,
            rx,
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mono chunks at the rate returned by [`Self::start`].
    pub fn chunks(&self) -> &Receiver<Vec<f32>> {
        &self.rx
    }

    /// Opens the device ahead of the first dictation.
    pub fn warm_up(&mut self) {
        if let Err(e) = self.open() {
            log::warn!("microphone not available yet: {e:#}");
        }
    }

    /// Starts capturing; returns the sample rate of the chunks that will follow.
    pub fn start(&mut self) -> Result<u32> {
        if self.needs_reopen() {
            self.open()?;
        }
        while self.rx.try_recv().is_ok() {}
        let live = self.live.as_ref().context("no microphone")?;
        if let Err(e) = live.stream.play() {
            // The device may have gone away while paused; retry once with a fresh stream.
            log::warn!("failed to resume microphone ({e}); reopening");
            self.open()?;
            let live = self.live.as_ref().context("no microphone")?;
            live.stream.play().context("failed to start the microphone")?;
        }
        Ok(self.live.as_ref().map_or(16_000, |l| l.rate))
    }

    pub fn stop(&mut self) {
        if let Some(live) = &self.live
            && let Err(e) = live.stream.pause()
        {
            log::warn!("failed to pause microphone: {e}");
        }
    }

    fn needs_reopen(&self) -> bool {
        let Some(live) = &self.live else { return true };
        if self.failed.load(Ordering::SeqCst) {
            return true;
        }
        if self.preferred.is_empty() {
            // Follow the system default (e.g. a headset that was just plugged in).
            let current = cpal::default_host().default_input_device().and_then(|d| d.id().ok());
            if current.is_some() && current != live.device_id {
                log::info!("default microphone changed");
                return true;
            }
        }
        false
    }

    fn open(&mut self) -> Result<()> {
        self.live = None;
        self.failed.store(false, Ordering::SeqCst);
        let host = cpal::default_host();
        let device = self.pick_device(&host)?;
        let name = device.to_string();
        let config = device.default_input_config().context("microphone has no input configuration")?;
        let rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let stream_config: cpal::StreamConfig = config.into();

        let stream = match config.sample_format() {
            SampleFormat::F32 => self.build::<f32>(&device, stream_config, channels),
            SampleFormat::I16 => self.build::<i16>(&device, stream_config, channels),
            SampleFormat::I32 => self.build::<i32>(&device, stream_config, channels),
            SampleFormat::U16 => self.build::<u16>(&device, stream_config, channels),
            SampleFormat::U8 => self.build::<u8>(&device, stream_config, channels),
            SampleFormat::I8 => self.build::<i8>(&device, stream_config, channels),
            SampleFormat::F64 => self.build::<f64>(&device, stream_config, channels),
            other => bail!("unsupported sample format {other}"),
        }?;
        // Some backends start streams on creation; stay idle until a dictation starts.
        let _ = stream.pause();
        log::info!("microphone: {name} ({rate} Hz, {channels} ch, {})", config.sample_format());
        self.live = Some(Live { stream, rate, device_id: device.id().ok(), name });
        Ok(())
    }

    fn pick_device(&self, host: &cpal::Host) -> Result<cpal::Device> {
        if !self.preferred.is_empty() {
            let devices = host.input_devices().context("failed to list microphones")?;
            for device in devices {
                if device.to_string().to_lowercase().contains(&self.preferred) {
                    return Ok(device);
                }
            }
            log::warn!("no microphone matches `{}`; using the default", self.preferred);
        }
        host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))
    }

    fn build<T>(
        &self,
        device: &cpal::Device,
        config: cpal::StreamConfig,
        channels: usize,
    ) -> Result<cpal::Stream>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let tx = self.tx.clone();
        let failed = self.failed.clone();
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = if channels == 1 {
                    data.iter().map(|&s| f32::from_sample(s)).collect()
                } else {
                    data.chunks_exact(channels)
                        .map(|frame| {
                            frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32
                        })
                        .collect()
                };
                let _ = tx.send(mono);
            },
            move |err| {
                log::warn!("microphone error: {err}");
                failed.store(true, Ordering::SeqCst);
            },
            None,
        )?;
        Ok(stream)
    }

    pub fn device_name(&self) -> Option<&str> {
        self.live.as_ref().map(|l| l.name.as_str())
    }
}
