//! Lightweight WASAPI loopback monitor for low-frequency bass-envelope reactivity.
//!
//! Runs on a background thread, isolates sub-150 Hz energy with a 1-pole
//! low-pass filter, and publishes a smoothed 0..1 breathing envelope via an
//! atomic float. Fails gracefully to 0.0 if no audio device is available.

#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};

pub const DEFAULT_FLASH_THRESHOLD: f32 = 1.04;
pub const DEFAULT_FLASH_STRENGTH: f32 = 1.20;
pub const DEFAULT_WOBBLE_THRESHOLD: f32 = 1.10;
pub const DEFAULT_WOBBLE_STRENGTH: f32 = 1.20;

pub struct AudioMonitor {
    level: Arc<AtomicU32>,
    beat: Arc<AtomicU32>,
    wobble: Arc<AtomicU32>,
    flash_threshold: Arc<AtomicU32>,
    wobble_threshold: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AudioMonitor {
    pub fn spawn() -> Self {
        let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let beat = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let wobble = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let flash_threshold = Arc::new(AtomicU32::new(DEFAULT_FLASH_THRESHOLD.to_bits()));
        let wobble_threshold = Arc::new(AtomicU32::new(DEFAULT_WOBBLE_THRESHOLD.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let level_bg = level.clone();
        let beat_bg = beat.clone();
        let wobble_bg = wobble.clone();
        let flash_thr_bg = flash_threshold.clone();
        let wobble_thr_bg = wobble_threshold.clone();
        let stop_bg = stop.clone();

        let worker = thread::Builder::new()
            .name("wasapi-loopback".into())
            .spawn(move || {
                let _ = run_loopback(
                    &level_bg,
                    &beat_bg,
                    &wobble_bg,
                    &flash_thr_bg,
                    &wobble_thr_bg,
                    &stop_bg,
                );
            })
            .ok();

        Self {
            level,
            beat,
            wobble,
            flash_threshold,
            wobble_threshold,
            stop,
            worker,
        }
    }

    pub fn set_flash_threshold(&self, threshold: f32) {
        self.flash_threshold
            .store(threshold.clamp(1.01, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_wobble_threshold(&self, threshold: f32) {
        self.wobble_threshold
            .store(threshold.clamp(1.02, 2.5).to_bits(), Ordering::Relaxed);
    }

    /// Returns the current smoothed bass energy envelope in `0.0..=1.0`.
    pub fn bass_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    /// Returns the adaptive drum-kick onset intensity in `0.0..=1.0`.
    pub fn beat_pulse(&self) -> f32 {
        f32::from_bits(self.beat.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    /// Returns the spring-damped elastic subwoofer displacement in `-0.45..=1.35`.
    pub fn beat_wobble(&self) -> f32 {
        f32::from_bits(self.wobble.load(Ordering::Relaxed)).clamp(-0.45, 1.35)
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_loopback(
    level: &AtomicU32,
    beat: &AtomicU32,
    wobble: &AtomicU32,
    flash_threshold: &AtomicU32,
    wobble_threshold: &AtomicU32,
    stop: &AtomicBool,
) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let res = loopback_inner(level, beat, wobble, flash_threshold, wobble_threshold, stop);
    unsafe {
        CoUninitialize();
    }
    res
}

fn loopback_inner(
    level: &AtomicU32,
    beat: &AtomicU32,
    wobble: &AtomicU32,
    flash_threshold: &AtomicU32,
    wobble_threshold: &AtomicU32,
    stop: &AtomicBool,
) -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };

    let fmt_ptr: *mut WAVEFORMATEX = unsafe { client.GetMixFormat()? };
    if fmt_ptr.is_null() {
        return Ok(());
    }
    let (channels, sample_rate, bits_per_sample, format_tag) = unsafe {
        let f = &*fmt_ptr;
        (
            f.nChannels.max(1) as usize,
            f.nSamplesPerSec.max(8000) as f32,
            f.wBitsPerSample,
            f.wFormatTag,
        )
    };

    // 50 ms shared loopback buffer (in 100-ns units).
    let init_res = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            500_000,
            0,
            fmt_ptr,
            None,
        )
    };
    unsafe {
        CoTaskMemFree(Some(fmt_ptr as *const _));
    }
    init_res?;

    let capture: IAudioCaptureClient = unsafe { client.GetService()? };
    unsafe { client.Start()? };

    // 1-pole low-pass filter around 140 Hz to isolate kick/bass energy.
    let lp_alpha = 1.0 - (-2.0 * std::f32::consts::PI * 140.0 / sample_rate).exp();
    let mut lp_state = 0.0f32;
    let mut env = 0.0f32;
    let mut avg_rms = 0.0f32;
    let mut peak_flash = 0.030f32;
    let mut peak_onset = 0.050f32;
    let mut prev_wobble_kick = 0.0f32;
    let mut density_stack = 0.0f32;
    let mut pulse = 0.0f32;
    let mut spr_pos = 0.0f32;
    let mut spr_vel = 0.0f32;
    let is_float32 = bits_per_sample == 32 && (format_tag == 3 || format_tag == 0xFFFE);
    let is_pcm16 = bits_per_sample == 16 && (format_tag == 1 || format_tag == 0xFFFE);

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(12));

        let mut energy_sum = 0.0f32;
        let mut frame_count = 0usize;

        loop {
            let packet_len = match unsafe { capture.GetNextPacketSize() } {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            let _ = packet_len;

            let mut data_ptr = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe {
                capture.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
            }
            .is_err()
            {
                break;
            }

            let frames = num_frames as usize;
            if frames > 0 && !data_ptr.is_null() && (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) == 0
            {
                if is_float32 {
                    let samples = unsafe {
                        std::slice::from_raw_parts(data_ptr as *const f32, frames * channels)
                    };
                    for ch_frame in samples.chunks_exact(channels) {
                        let mono: f32 = ch_frame.iter().sum::<f32>() / channels as f32;
                        lp_state += lp_alpha * (mono - lp_state);
                        energy_sum += lp_state * lp_state;
                    }
                    frame_count += frames;
                } else if is_pcm16 {
                    let samples = unsafe {
                        std::slice::from_raw_parts(data_ptr as *const i16, frames * channels)
                    };
                    for ch_frame in samples.chunks_exact(channels) {
                        let mono: f32 = ch_frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>()
                            / channels as f32;
                        lp_state += lp_alpha * (mono - lp_state);
                        energy_sum += lp_state * lp_state;
                    }
                    frame_count += frames;
                }
            }

            let _ = unsafe { capture.ReleaseBuffer(num_frames) };
        }

        let rms = if frame_count > 0 {
            (energy_sum / frame_count as f32).sqrt()
        } else {
            0.0
        };
        let target = (rms * 4.5).clamp(0.0, 1.0);

        // Smooth bass envelope (for breathing/speed).
        let coeff = if target > env { 0.30 } else { 0.07 };
        env += coeff * (target - env);
        level.store(env.to_bits(), Ordering::Relaxed);

        // Two-tier adaptive drum-kick detector:
        // Tier 1 (lower threshold): triggers softer peripheral flash without wobble.
        // Tier 2 (stricter threshold): triggers BOTH flash and smooth elastic blob wobble,
        // smoothly stacking intensity when kicks arrive densely instead of snapping.
        let flash_thr =
            f32::from_bits(flash_threshold.load(Ordering::Relaxed)).clamp(1.01, 2.0);
        let wobble_thr =
            f32::from_bits(wobble_threshold.load(Ordering::Relaxed)).clamp(1.02, 2.5);

        let flash_onset = (rms - avg_rms * flash_thr).max(0.0);
        peak_flash = (peak_flash * 0.996).max(flash_onset).max(0.022);
        let flash_kick = (flash_onset / peak_flash).clamp(0.0, 1.0).powf(1.25) * 0.86;

        let wobble_onset = (rms - avg_rms * wobble_thr).max(0.0);
        peak_onset = (peak_onset * 0.9965).max(wobble_onset).max(0.032);
        let wobble_kick = (wobble_onset / peak_onset).clamp(0.0, 1.0).powf(1.20);

        let avg_alpha = if rms > avg_rms { 0.035 } else { 0.065 };
        avg_rms += avg_alpha * (rms - avg_rms);

        if flash_kick > pulse {
            pulse += 0.65 * (flash_kick - pulse);
        } else {
            pulse *= 0.85;
        }
        beat.store(pulse.to_bits(), Ordering::Relaxed);

        // Original silky underdamped spring (k=115, c=11) with continuous
        // dense-kick intensity stacking (zero position snapping) and a lifted
        // low-beat curve that keeps peak impulse identical at wobble_kick = 1.0.
        let dt = 0.012f32;
        density_stack *= 0.985;
        if wobble_kick > 0.16 {
            if wobble_kick > prev_wobble_kick + 0.02 && spr_pos.abs() > 0.06 {
                density_stack = (density_stack + 0.22).min(1.0);
            }
            let stack_gain = 1.0 + density_stack * 0.40;
            let norm = ((wobble_kick - 0.16) / 0.84).clamp(0.0, 1.0);
            let impulse = norm.powf(1.15) * 9.2 * stack_gain;
            if impulse > spr_vel {
                spr_vel = impulse.min(15.5);
            } else if wobble_kick > prev_wobble_kick + 0.04 {
                spr_vel = (spr_vel + impulse * 0.28).min(15.5);
            }
        }
        prev_wobble_kick = wobble_kick;

        let max_pos = 0.85 + density_stack * 0.25;
        let accel = -115.0 * spr_pos - 11.0 * spr_vel;
        spr_vel += accel * dt;
        spr_pos = (spr_pos + spr_vel * dt).clamp(-0.40, max_pos);
        wobble.store(spr_pos.to_bits(), Ordering::Relaxed);
    }

    let _ = unsafe { client.Stop() };
    Ok(())
}
