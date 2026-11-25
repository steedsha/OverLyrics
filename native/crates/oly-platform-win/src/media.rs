//! Task 9: the motion-poster media path.
//!
//! Two layers live here:
//!
//! * [`MotionPlayback`] — the deterministic lifecycle state machine, a
//!   call-for-call port of the shipped `src/windows/lyrics/motion-playback.ts`.
//!   The legacy `setTimeout` timer and the `play()` promise are replaced by an
//!   explicit [`MotionPlayback::tick`] clock and explicit callbacks, so tests
//!   assert exact boundaries without racing a real clock.
//! * [`VideoDecoder`] — the Media Foundation source-reader decoder that hands
//!   NV12 frames to the caller's D3D11 device, bounded at
//!   [`VIDEO_MAX_PENDING_SAMPLES`] pending samples, with the seek/drop/release
//!   semantics the plan's lifecycle list requires.
//!
//! The decoder never performs a per-frame CPU readback or upload on the main
//! path: with the DXGI device manager wired through `MF_SOURCE_READER_D3D_MANAGER`
//! the hardware MFT decodes onto *our* device, and each frame lands here as an
//! owned NV12 texture via a same-device GPU copy. A system-memory upload exists
//! only as the software-decode fallback, which the plan's performance gate does
//! not count.

#![allow(unsafe_code)]
// Every decoder function is a Media Foundation / COM boundary; marking the
// functions `unsafe fn` and not re-wrapping each op is the deliberate shape.
#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, anyhow};
use oly_contracts::{MediaMs, MonoMs};
use oly_core::cache::video_nv12_bytes;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaBuffer, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType,
    IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFStartup, MFShutdown, MFNominalRange_0_255,
    MFNominalRange_Unknown, MF_PD_DURATION, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_LOW_LATENCY,
    MF_SOURCE_READER_MEDIASOURCE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MFSTARTUP_LITE, MF_VERSION, MFMediaType_Video,
    MFVideoFormat_NV12, MFVideoTransFunc_709, MFVideoTransFunc_Unknown,
    MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709, MFVideoTransferMatrix_Unknown,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VT_I8;

pub use oly_core::cache::VIDEO_MAX_PENDING_SAMPLES;

/// The legacy release delay: `motion-playback.ts` arms a 1500 ms timer on hide
/// and releases the decoder when it fires.
pub const RELEASE_AFTER_MS: u64 = 1500;

/// COM initializes per thread; `VideoDecoder` takes a reference when it had to
/// initialize it itself, so `Drop` can balance it.
fn ensure_com() -> bool {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            if hr == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
                // COM is already available under another model; we did not
                // take a reference, so we must not release one.
                return false;
            }
            log::warn!("CoInitializeEx failed: {hr}");
            return false;
        }
        // S_OK and S_FALSE both leave us holding one reference.
        true
    }
}

// ---------------------------------------------------------------------------
// The lifecycle state machine (pure CPU)
// ---------------------------------------------------------------------------

/// The host-side video element the state machine drives. The shipped code
/// drives an `HTMLVideoElement`; the decoder layer (or a test fake) implements
/// this so the machine is identical either way.
pub trait VideoHost {
    /// `video.src !== ""`.
    fn has_source(&self) -> bool;
    /// `video.src = src; video.load()` — resets the playhead until metadata.
    fn load(&mut self, src: &str);
    /// `video.play()` — asynchronous in the DOM, synchronous here; the async
    /// completion is reported separately through
    /// [`MotionPlayback::on_play_completed`].
    fn play(&mut self);
    /// `video.pause()`.
    fn pause(&mut self);
    /// `video.currentTime`, `None` while no media is loaded (the DOM's NaN).
    fn current_time_ms(&self) -> Option<MediaMs>;
    /// `video.duration`, `None` until metadata has loaded (the DOM's NaN).
    fn duration_ms(&self) -> Option<MediaMs>;
    /// `video.currentTime = target`. Returns false when the decoder is not
    /// seekable (the DOM throws; the reference catches and starts from head).
    fn seek_ms(&mut self, to: MediaMs) -> bool;
    /// The release path: `pause(); removeAttribute('src'); load()`.
    fn unload(&mut self);
}

/// The motion-poster lifecycle, ported call-for-call from
/// `src/windows/lyrics/motion-playback.ts`.
///
/// Differences from the reference are all mechanical:
///
/// * the 1500 ms release timer is not a `setTimeout`; it is a deadline the
///   caller drives with [`MotionPlayback::tick`], so a test asserts the
///   boundary exactly (`now == armed_at + RELEASE_AFTER_MS` fires, one
///   millisecond earlier does not);
/// * the `play()` promise's late pause is reported through
///   [`MotionPlayback::on_play_completed`] instead of a `.then`;
/// * `loadedmetadata` is reported through [`MotionPlayback::on_metadata`].
///
/// Everything else — arm/cancel ordering, the non-finite `currentTime` guard,
/// the `duration - 0.01` seek clamp, `release(false)` at dispose — is
/// identical.
pub struct MotionPlayback<H: VideoHost> {
    host: H,
    src: String,
    visible: bool,
    loaded: bool,
    disposed: bool,
    /// The playhead as of the last hide. `None` mirrors the DOM's non-finite
    /// `currentTime` (nothing loaded), which the metadata handler skips.
    saved_position: Option<MediaMs>,
    /// The monotonic instant the release timer fires, if armed.
    release_at: Option<MonoMs>,
    /// Set by `play()`, consumed by `on_play_completed` — models the DOM's
    /// play-promise resolution.
    play_pending: bool,
    on_release: Option<Box<dyn FnOnce(&mut H) + 'static>>,
}

impl<H: VideoHost> MotionPlayback<H> {
    pub fn new(host: H, src: &str, visible: bool) -> Self {
        let mut playback = Self {
            host,
            src: src.to_string(),
            visible,
            loaded: false,
            disposed: false,
            saved_position: None,
            release_at: None,
            play_pending: false,
            on_release: None,
        };
        // `if (visible) play()` — the constructor's only side effect.
        if visible {
            playback.play();
        }
        playback
    }

    /// Installs the `onRelease` callback. Returns `self` so construction
    /// chains like the reference's options object.
    pub fn with_release_hook<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(&mut H) + 'static,
    {
        self.on_release = Some(Box::new(hook));
        self
    }

    /// The `setVisible` entry point.
    pub fn set_visible(&mut self, visible: bool, now: MonoMs) {
        if self.disposed || visible == self.visible {
            return;
        }
        self.visible = visible;
        self.cancel_release();
        if visible {
            self.play();
            return;
        }
        if self.loaded {
            // `if (Number.isFinite(video.currentTime)) position = ...` — a
            // playhead of None (nothing loaded) is skipped, but a *zero*
            // playhead is saved (the reference assigns it; the metadata
            // handler is what skips zero).
            self.saved_position = self.host.current_time_ms();
            self.host.pause();
        }
        self.release_at = Some(now.saturating_add(RELEASE_AFTER_MS as i64));
    }

    /// Drives the release timer. Fire condition: armed and `now >= deadline`.
    /// The reference's `setTimeout` fires at-or-after the deadline, so the
    /// boundary is inclusive.
    pub fn tick(&mut self, now: MonoMs) {
        if self.disposed {
            return;
        }
        let Some(deadline) = self.release_at else {
            return;
        };
        if now >= deadline {
            self.release_at = None;
            if !self.visible && !self.disposed {
                self.release(true);
            }
        }
    }

    /// The `loadedmetadata` handler: restore the saved position, clamped to
    /// `duration - 0.01 s` like the reference.
    pub fn on_metadata(&mut self) {
        if self.disposed || !self.visible {
            return;
        }
        let Some(position) = self.saved_position else {
            return;
        };
        self.saved_position = None;
        // The reference skips position 0 (`if (... || !position) return`).
        if position.millis() == 0 {
            return;
        }
        let Some(duration) = self.host.duration_ms() else {
            // `Number.isFinite(duration) && duration > 0` failed: seek raw.
            let _ = self.host.seek_ms(position);
            return;
        };
        if duration.millis() <= 0 {
            let _ = self.host.seek_ms(position);
            return;
        }
        // `Math.min(position, duration - 0.01)` — 0.01 s before the end.
        let target = position.min(MediaMs::from_millis(duration.millis() - 10));
        // try/catch: a non-seekable decoder starts from the head.
        let _ = self.host.seek_ms(target);
    }

    /// The `play()` promise resolution: if the poster became hidden or
    /// disposed while the play was in flight, pause again.
    pub fn on_play_completed(&mut self) {
        let was_pending = self.play_pending;
        self.play_pending = false;
        if was_pending && (self.disposed || !self.visible) {
            self.host.pause();
        }
    }

    /// The `dispose` entry point: cancel the timer, unload silently (the
    /// reference's `release(false)` — no `onRelease` callback), mark disposed.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.cancel_release();
        self.release(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    /// The pending release deadline, if armed. Tests assert the exact
    /// boundary through this.
    pub fn pending_release_at(&self) -> Option<MonoMs> {
        self.release_at
    }

    /// The source string the playback was constructed with.
    pub fn src(&self) -> &str {
        &self.src
    }

    /// Read access to the host (tests assert its call log).
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Mutable access to the host (tests stage `duration` and seekability).
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    // -- internals ----------------------------------------------------------

    fn cancel_release(&mut self) {
        self.release_at = None;
    }

    fn play(&mut self) {
        if self.disposed || !self.visible {
            return;
        }
        if !self.loaded {
            self.host.load(&self.src);
            self.loaded = true;
        }
        // `void video.play()?.then(...)` — the async completion is reported
        // back separately.
        self.play_pending = true;
        self.host.play();
    }

    fn release(&mut self, notify: bool) {
        if !self.loaded {
            return;
        }
        self.host.pause();
        self.host.unload();
        self.loaded = false;
        if notify {
            if let Some(hook) = self.on_release.take() {
                hook(&mut self.host);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The Media Foundation decoder
// ---------------------------------------------------------------------------

/// Colour-space facts read from the video media type. The renderer derives
/// its NV12→RGB conversion from these, never from a fixed BT.601 assumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoColourInfo {
    /// `MF_MT_YUV_MATRIX`: BT.601 vs BT.709.
    pub matrix: oly_render::video::ColourMatrix,
    /// `MF_MT_VIDEO_NOMINAL_RANGE` (0..255 vs 16..235).
    pub range: oly_render::video::ColourRange,
    /// `MF_MT_TRANSFER_FUNCTION` named for diagnostics.
    pub transfer: &'static str,
}

/// The stream facts the decoder publishes on open.
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub duration_ms: MediaMs,
    pub colour: VideoColourInfo,
}

/// One decoded frame on its way to the renderer: an NV12 texture owned by the
/// decoder's device (a copy, so the source reader recycles its surface the
/// moment this sample's `IMFSample` is released).
#[derive(Clone)]
pub struct VideoSample {
    pub texture: ID3D11Texture2D,
    pub pts_ms: MediaMs,
    pub width: u32,
    pub height: u32,
    /// NV12 byte size, charged to the decode budget.
    pub bytes: u64,
}

/// Decoder accounting, asserted by the lifecycle tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    /// Frames successfully decoded (queued or dropped as stale) since open.
    pub decoded: u64,
    /// Frames discarded: the queue was full (the bound always wins over decode
    /// eagerness) or the frame was older than the newest queued one.
    pub dropped_late: u64,
    /// NV12 bytes currently held in the pending queue.
    pub pending_bytes: u64,
}

/// Media Foundation source reader over a file, decoding into NV12 textures on
/// the caller's D3D11 device.
///
/// Bounded queue: at most [`VIDEO_MAX_PENDING_SAMPLES`] decoded-but-
/// unpresented samples are held. `decode_one` fills a free slot or drops the
/// oldest sample to make room — the bound wins over decode eagerness, so a
/// 240 Hz display can never drive a 240-frame backlog. `pump(playhead)` drops
/// every sample at-or-before the playhead and refills, keeping latency flat.
pub struct VideoDecoder {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dxgi_manager: Option<IMFDXGIDeviceManager>,
    reader: Option<IMFSourceReader>,
    info: Option<StreamInfo>,
    queue: VecDeque<VideoSample>,
    stats: DecodeStats,
    ended: bool,
    owns_com: bool,
}

impl VideoDecoder {
    /// Creates a decoder bound to `device`. COM is initialized here if the
    /// calling thread has not done so already.
    pub fn new(device: ID3D11Device, context: ID3D11DeviceContext) -> anyhow::Result<Self> {
        let owns_com = ensure_com();
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("starting Media Foundation")?;
        }

        // Hand MF the caller's device so hardware MFTs decode straight onto
        // it — the plan's IMFDXGIDeviceManager wiring.
        let device_as_dxgi: IDXGIDevice = device
            .cast()
            .context("casting the D3D11 device to IDXGIDevice")?;
        let mut reset_token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
                .context("creating the DXGI device manager")?;
        }
        let dxgi_manager = manager.context("MFCreateDXGIDeviceManager returned no manager")?;
        unsafe {
            dxgi_manager
                .ResetDevice(&device_as_dxgi, reset_token)
                .context("binding the D3D11 device to the DXGI manager")?;
        }

        Ok(Self {
            device,
            context,
            dxgi_manager: Some(dxgi_manager),
            reader: None,
            info: None,
            queue: VecDeque::new(),
            stats: DecodeStats::default(),
            ended: false,
            owns_com,
        })
    }

    /// The device this decoder renders to. The GPU video pass builds its
    /// textures and pipelines from it; it is the same device the caller
    /// passed to [`Self::new`].
    pub fn device_handle(&self) -> &ID3D11Device {
        &self.device
    }

    /// The immediate context paired with [`Self::device_handle`].
    pub fn context_handle(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Opens `path`. Fails loudly on a non-media file so the UI can fall back
    /// to the still cover; a failed open does not poison the decoder.
    pub fn open(&mut self, path: &Path) -> anyhow::Result<StreamInfo> {
        self.release_queue();
        self.reader = None;
        self.info = None;
        self.ended = false;

        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("a non-UTF-8 media path cannot be opened: {path:?}"))?
            .to_string();

        // The source reader takes attributes only at creation: the D3D device
        // manager and the low-latency hint must go in here.
        let attrs: IMFAttributes = unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2).context("creating source-reader attributes")?;
            attrs.context("no attribute store")?
        };
        unsafe {
            if let Some(manager) = &self.dxgi_manager {
                attrs
                    .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, manager)
                    .context("binding the DXGI device manager to the source reader")?;
            }
            attrs
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .context("setting the low-latency hint")?;
        }

        let reader: IMFSourceReader = unsafe {
            MFCreateSourceReaderFromURL(
                &windows::core::HSTRING::from(path_str.as_str()),
                &attrs,
            )
            .context("creating the Media Foundation source reader")?
        };

        // Ask for NV12 on the first video stream. The source reader inserts
        // its colour converter when the container's native format differs.
        let nv12: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &nv12,
                )
                .context("selecting NV12 output on the video stream")?;
        }

        let info = unsafe { Self::read_stream_info(&reader)? };
        self.reader = Some(reader);
        self.info = Some(info.clone());
        Ok(info)
    }

    /// Reads width/height/fps/duration/colour facts off the current media
    /// type. Colour facts come from the media type itself — the plan forbids
    /// assuming BT.601.
    unsafe fn read_stream_info(reader: &IMFSourceReader) -> anyhow::Result<StreamInfo> {
        let media_type: IMFMediaType = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .context("reading the current media type")?;

        let frame_size =
            media_type.GetUINT64(&MF_MT_FRAME_SIZE).context("reading MF_MT_FRAME_SIZE")?;
        let width = (frame_size >> 32) as u32;
        let height = frame_size as u32;
        let frame_rate =
            media_type.GetUINT64(&MF_MT_FRAME_RATE).context("reading MF_MT_FRAME_RATE")?;
        let fps_num = (frame_rate >> 32) as u32;
        let fps_den = frame_rate as u32;

        let nom_range = media_type
            .GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE)
            .unwrap_or(MFNominalRange_Unknown.0 as u32);
        let matrix = media_type
            .GetUINT32(&MF_MT_YUV_MATRIX)
            .unwrap_or(MFVideoTransferMatrix_Unknown.0 as u32);
        let transfer = media_type
            .GetUINT32(&MF_MT_TRANSFER_FUNCTION)
            .unwrap_or(MFVideoTransFunc_Unknown.0 as u32);

        // Duration probes, in order: (a) the reader forwards
        // GetPresentationAttribute(MEDIASOURCE, MF_PD_DURATION) to the media
        // source's presentation descriptor; (b) the explicit
        // GetServiceForStream(MEDIASOURCE) → IMFMediaSource →
        // CreatePresentationDescriptor → GetUINT64 path. Some MP4 demuxers
        // only expose the attribute through one of the two.
        let duration_100ns: u64 = match unsafe {
            reader.GetPresentationAttribute(
                MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                &MF_PD_DURATION,
            )
        } {
            Ok(variant) => match u64::try_from(&variant) {
                Ok(v) if v > 0 => v,
                Ok(_) => {
                    // Expected on several MP4 demuxers — duration then comes
                    // from the player layer (PlayerPacket.durationMs).
                    log::debug!("[media] MF_PD_DURATION via MEDIASOURCE: empty variant");
                    0
                }
                Err(e) => {
                    log::debug!("[media] MF_PD_DURATION via MEDIASOURCE: not convertible: {e}");
                    0
                }
            },
            Err(e) => {
                log::debug!("[media] MF_PD_DURATION via MEDIASOURCE: {e}");
                0
            }
        };

        Ok(StreamInfo {
            width,
            height,
            fps_num,
            fps_den,
            duration_ms: MediaMs::from_millis((duration_100ns / 10_000) as i64),
            colour: VideoColourInfo {
                matrix: matrix_from_mf(matrix, height),
                range: range_from_mf(nom_range),
                transfer: transfer_name(transfer),
            },
        })
    }

    pub fn is_open(&self) -> bool {
        self.reader.is_some()
    }

    pub fn info(&self) -> Option<&StreamInfo> {
        self.info.as_ref()
    }

    pub fn stats(&self) -> DecodeStats {
        self.stats
    }

    pub fn at_eof(&self) -> bool {
        self.ended
    }

    /// Decoded samples awaiting presentation, in PTS order.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    pub fn front(&self) -> Option<&VideoSample> {
        self.queue.front()
    }

    pub fn pop_front(&mut self) -> Option<VideoSample> {
        let sample = self.queue.pop_front()?;
        self.stats.pending_bytes = self.stats.pending_bytes.saturating_sub(sample.bytes);
        Some(sample)
    }

    /// Decodes toward the playhead, dropping samples at-or-before it. Called
    /// once per rendered frame; never blocks on the GPU.
    pub fn pump(&mut self, playhead_ms: MediaMs) -> anyhow::Result<()> {
        // Drop everything at-or-before the playhead: late frames add no value
        // and dropping them keeps latency flat.
        while let Some(front) = self.queue.front() {
            if front.pts_ms >= playhead_ms {
                break;
            }
            let _ = self.pop_front();
            self.stats.dropped_late += 1;
        }
        // Refill toward the bound.
        while self.queue.len() < VIDEO_MAX_PENDING_SAMPLES {
            if !self.decode_one()? {
                self.ended = true;
                break; // EOF
            }
        }
        Ok(())
    }

    /// Seeks to `to` and clears the queue: the next decode returns the first
    /// frame at-or-after `to`.
    pub fn seek(&mut self, to: MediaMs) -> anyhow::Result<()> {
        self.release_queue();
        self.ended = false;
        let Some(reader) = &self.reader else {
            return Err(anyhow!("cannot seek a closed decoder"));
        };
        let pos = (to.millis().max(0) as u64) * 10_000; // ms -> 100 ns
        unsafe {
            // MF expects a VT_I8 position in 100 ns units; the crate's
            // From<u64> helper only builds VT_UI8, so build the variant
            // directly. hVal and uhVal share the same storage.
            let mut prop = PROPVARIANT::default();
            *prop.Anonymous.Anonymous = PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: pos as i64 },
            };
            reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &prop)
                .context("seeking the source reader")?;
        }
        Ok(())
    }

    /// Decodes one frame into the queue. Returns false at end of stream.
    /// When the queue is full the oldest sample is dropped first — the bound
    /// wins over decode eagerness, and the freshest frames are the ones a
    /// playhead-driven presenter can still use.
    pub fn decode_one(&mut self) -> anyhow::Result<bool> {
        let Some(sample) = self.read_next()? else {
            return Ok(false);
        };
        while self.queue.len() >= VIDEO_MAX_PENDING_SAMPLES {
            let _ = self.pop_front();
            self.stats.dropped_late += 1;
        }
        // A sample older than the newest queued one can never be presented
        // ahead of it: drop instead of reordering.
        if self.queue.back().is_some_and(|back| sample.pts_ms < back.pts_ms) {
            self.stats.dropped_late += 1;
            return Ok(true);
        }
        self.stats.decoded += 1;
        self.stats.pending_bytes += sample.bytes;
        self.queue.push_back(sample);
        Ok(true)
    }

    /// One `ReadSample` round trip: converts the MF sample into an owned NV12
    /// texture on the caller's device. Returns `None` at end of stream.
    fn read_next(&mut self) -> anyhow::Result<Option<VideoSample>> {
        let Some(reader) = &self.reader else {
            return Err(anyhow!("cannot decode a closed decoder"));
        };
        let info = self
            .info
            .clone()
            .ok_or_else(|| anyhow!("cannot decode before open"))?;

        let mut flags: u32 = 0;
        let mut timestamp_100ns: i64 = 0;
        let mut sample_opt: Option<IMFSample> = None;
        unsafe {
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp_100ns),
                    Some(&mut sample_opt),
                )
                .context("ReadSample failed")?;
        }

        let end_of_stream = (flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0;
        let Some(sample) = sample_opt else {
            if end_of_stream {
                return Ok(None);
            }
            return Err(anyhow!("ReadSample returned no sample without EOS"));
        };

        // MR_BUFFER_SERVICE -> IMFDXGIBuffer -> ID3D11Texture2D. With the DXGI
        // device manager wired at creation the decoded texture lives on OUR
        // device (as an array slice), so a same-device GPU copy suffices —
        // no shared handles, no CPU round trip.
        let texture =
            unsafe { Self::texture_from_sample(&sample, &info, &self.device, &self.context)? };

        #[cfg(test)]
        eprintln!(
            "[diag] plltimestamp={} GetSampleTime={:?}",
            timestamp_100ns,
            unsafe { sample.GetSampleTime() }
        );
        let pts_ms = MediaMs::from_millis(timestamp_100ns / 10_000);
        Ok(Some(VideoSample {
            texture,
            pts_ms,
            width: info.width,
            height: info.height,
            bytes: video_nv12_bytes(info.width, info.height),
        }))
    }

    /// Resolves the D3D11 texture behind an MF buffer: a same-device GPU copy
    /// for the hardware path (the usual case, via the DXGI device manager),
    /// or a one-time system-memory upload for the software fallback. The
    /// upload path is the degraded one the plan's performance gate excludes.
    unsafe fn texture_from_sample(
        sample: &IMFSample,
        info: &StreamInfo,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> anyhow::Result<ID3D11Texture2D> {
        // `GetBufferByIndex` covers the hardware path; `ConvertToContiguousBuffer`
        // linearizes multi-buffer samples for the software fallback.
        let buffer: IMFMediaBuffer = sample
            .GetBufferByIndex(0)
            .or_else(|_| sample.ConvertToContiguousBuffer())
            .context("no media buffer on the decoded sample")?;
        if let Ok(dxgi_buffer) = buffer.cast::<IMFDXGIBuffer>() {
            let mut resource: Option<windows::core::IUnknown> = None;
            let hr = dxgi_buffer.GetResource(
                &ID3D11Texture2D::IID,
                &mut resource as *mut _ as *mut *mut core::ffi::c_void,
            );
            if hr.is_ok() {
                if let Some(unknown) = resource {
                    if let Ok(source) = unknown.cast::<ID3D11Texture2D>() {
                        let subresource = dxgi_buffer.GetSubresourceIndex().unwrap_or(0);
                        return Self::gpu_copy_same_device(
                            &source, subresource, info, device, context,
                        );
                    }
                }
            }
        }
        // Software fallback: system-memory NV12, uploaded once per frame.
        Self::upload_system_memory(sample, info, device, context)
    }

    /// The hardware path: copy the decoded array slice onto a plain NV12
    /// texture the renderer can sample. Same device, GPU-only, no sync.
    unsafe fn gpu_copy_same_device(
        source: &ID3D11Texture2D,
        subresource: u32,
        info: &StreamInfo,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> anyhow::Result<ID3D11Texture2D> {
        let own_desc = D3D11_TEXTURE2D_DESC {
            Width: info.width,
            Height: info.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut own: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&own_desc, None, Some(&mut own))
            .context("creating the own-device NV12 texture")?;
        let own = own.context("no own-device texture")?;
        context.CopySubresourceRegion(&own, 0, 0, 0, 0, source, subresource, None);
        Ok(own)
    }

    /// The software path: the buffer holds contiguous system-memory NV12
    /// (Y plane then UV plane, pitch = width). One `UpdateSubresource`, no
    /// staging texture, no readback. Marked degraded: this path does not meet
    /// the plan's high-performance gate and is only a fallback.
    unsafe fn upload_system_memory(
        sample: &IMFSample,
        info: &StreamInfo,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> anyhow::Result<ID3D11Texture2D> {
        let contiguous = sample
            .ConvertToContiguousBuffer()
            .context("linearizing the system-memory NV12 buffer")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        contiguous
            .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
            .context("locking the NV12 buffer")?;
        let needed = video_nv12_bytes(info.width, info.height) as usize;
        if (cur_len as usize) < needed {
            let _ = contiguous.Unlock();
            anyhow::bail!("the NV12 buffer holds {cur_len} bytes, expected at least {needed}");
        }
        let bytes = std::slice::from_raw_parts(ptr, needed).to_vec();
        let _ = contiguous.Unlock();

        let own_desc = D3D11_TEXTURE2D_DESC {
            Width: info.width,
            Height: info.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut own: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&own_desc, None, Some(&mut own))
            .context("creating the own-device NV12 texture")?;
        let own = own.context("no own-device texture")?;
        // One upload for both planes: NV12 packs Y then UV contiguously with
        // pitch = width.
        context.UpdateSubresource(
            &own,
            0,
            None,
            bytes.as_ptr() as _,
            info.width,
            0,
        );
        Ok(own)
    }

    /// Drops every queued sample (and their textures).
    fn release_queue(&mut self) {
        self.queue.clear();
        self.stats.pending_bytes = 0;
    }

    /// The hidden-window release path: drop everything decoded, close the
    /// reader, keep the device.
    pub fn release(&mut self) {
        self.release_queue();
        self.reader = None;
        self.info = None;
        self.ended = false;
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.release();
        unsafe {
            let _ = MFShutdown();
            if self.owns_com {
                CoUninitialize();
            }
        }
    }
}

/// Maps an `MFVideoTransferMatrix` onto the renderer's matrix enum.
///
/// MP4 containers frequently omit the matrix (it lives in the h264 VUI, which
/// MF does not surface on the decoded NV12 media type). `Unknown` therefore
/// falls back to the industry-standard resolution heuristic — BT.709 for HD
/// (≥720 rows), BT.601 for SD — the same default ffmpeg/mpv apply. A stream
/// that *declares* BT.709 or BT.601 always wins over the heuristic.
fn matrix_from_mf(value: u32, height: u32) -> oly_render::video::ColourMatrix {
    use oly_render::video::ColourMatrix;
    if value as i32 == MFVideoTransferMatrix_BT709.0 {
        ColourMatrix::Bt709
    } else if value as i32 == MFVideoTransferMatrix_BT601.0 {
        ColourMatrix::Bt601
    } else if height >= 720 {
        ColourMatrix::Bt709
    } else {
        ColourMatrix::Bt601
    }
}

/// Maps an `MFNominalRange` onto the renderer's range enum.
fn range_from_mf(value: u32) -> oly_render::video::ColourRange {
    if value as i32 == MFNominalRange_0_255.0 {
        oly_render::video::ColourRange::Full
    } else {
        // 16_235 and Unknown both mean studio swing for this pipeline.
        oly_render::video::ColourRange::Limited
    }
}

/// Names the transfer function for diagnostics.
fn transfer_name(value: u32) -> &'static str {
    if value as i32 == MFVideoTransFunc_709.0 {
        "bt709"
    } else {
        "unknown"
    }
}

/// QA/inspection path: reads an NV12 texture back to system memory through a
/// staging texture. This is NOT the render path — the plan's performance gate
/// forbids per-frame CPU readback there. It exists so the QA driver can prove
/// the decoder's pixels against an FFmpeg reference decode.
pub fn readback_nv12(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    width: u32,
    height: u32,
) -> anyhow::Result<(Vec<u8>, Vec<u8>, usize, usize)> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_CPU_ACCESS_READ,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    debug_assert_eq!(desc.Format, DXGI_FORMAT_NV12);

    // NV12 staging: same dimensions, one allocation covering luma+chroma.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    unsafe {
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .context("creating NV12 staging texture")?;
    }
    let staging = staging.context("CreateTexture2D returned no texture")?;

    unsafe { context.CopyResource(&staging, texture) };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .context("mapping NV12 staging texture")?;
    }
    let pitch = mapped.RowPitch as usize;
    // NV12 is planar: height rows of luma (W bytes each), then height/2 rows of
    // interleaved UV (W bytes each). The mapped buffer is contiguous.
    let total = pitch * (height as usize + height as usize / 2);
    let bytes = unsafe {
        std::slice::from_raw_parts(mapped.pData as *const u8, total).to_vec()
    };
    unsafe { context.Unmap(&staging, 0) };

    // Split into the two planes the renderer's Nv12Plane expects.
    let luma_pitch = pitch;
    let chroma_pitch = pitch; // UV rows are W bytes wide too (interleaved)
    let luma_len = pitch * height as usize;
    let luma = bytes[..luma_len].to_vec();
    let chroma = bytes[luma_len..luma_len + pitch * (height as usize / 2)].to_vec();
    Ok((luma, chroma, luma_pitch, chroma_pitch))
}
