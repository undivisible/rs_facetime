//! Optional PCM audio interfaces, separate from FaceTime call control.
//!
//! These interfaces do not expose FaceTime's internal call PCM or RTP. A capture
//! backend may capture application output, including ringing and mixed callers.
//! All capture is explicitly started by the caller; there is no default capture,
//! recording, device switching, or audio permission request in this module.
//!
//! [`pcm_channel`] is a bounded, runtime-independent handoff for owned PCM frames.
//! It uses a mutex and allocates frame storage, so it must not be called from a
//! real-time audio callback. A native backend must hand off callback data to a
//! worker through its own bounded real-time-safe mechanism first.

mod buffer;

#[cfg(feature = "coreaudio-capture")]
pub mod coreaudio;

pub use buffer::{pcm_channel, BufferConfig, BufferStats, PcmSender, PcmStream, SendOutcome};

use std::time::Duration;

/// Errors from audio configuration, capture, or a caller-provided routing backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("this audio backend is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("this audio backend requires {minimum} or later")]
    UnsupportedOs { minimum: &'static str },
    #[error("explicit audio capture permission is required")]
    PermissionRequired,
    #[error("audio capture permission was denied")]
    PermissionDenied,
    #[error("the host application is missing its audio capture usage description")]
    MissingUsageDescription,
    #[error("unsupported PCM format: {reason}")]
    UnsupportedFormat { reason: &'static str },
    #[error("invalid audio configuration: {reason}")]
    InvalidConfiguration { reason: &'static str },
    #[error("audio backend operation {operation} failed with status {code}")]
    Backend { operation: &'static str, code: i32 },
    #[error("native cleanup failed during {operation} with status {code}; resources may remain until process exit")]
    CleanupFailed { operation: &'static str, code: i32 },
    #[error("audio stream is closed")]
    Closed,
    #[error("the selected audio device is unavailable")]
    DeviceUnavailable,
    #[error("the selected audio source changed; an explicit restart is required")]
    SourceChanged,
    #[error("PCM format changed from {expected:?} to {actual:?}")]
    FormatChanged {
        expected: PcmFormat,
        actual: PcmFormat,
    },
}

/// Interleaved, native `f32` PCM. No integer or planar sample representation is
/// accepted by this API. Backends must convert their input before emitting frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    sample_rate: u32,
    channels: u16,
}

impl PcmFormat {
    /// Validate a sample rate of 1–384,000 Hz and 1–32 channels.
    /// A backend may support a smaller range and reject it when starting.
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, AudioError> {
        if sample_rate == 0 || sample_rate > 384_000 {
            return Err(AudioError::UnsupportedFormat {
                reason: "sample rate must be between 1 and 384000 Hz",
            });
        }
        if channels == 0 || channels > 32 {
            return Err(AudioError::UnsupportedFormat {
                reason: "channel count must be between 1 and 32",
            });
        }
        Ok(Self {
            sample_rate,
            channels,
        })
    }

    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    pub fn channels(self) -> u16 {
        self.channels
    }
}

/// Timing of the first sample in an owned PCM buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimestamp {
    /// Strictly increasing buffer sequence within one stream, starting at any
    /// value. This counts buffers, not individual samples. Gaps can indicate
    /// dropped buffers; sequence numbers must not wrap within a stream.
    pub sequence: u64,
    /// Optional monotonic host-clock time, with a backend-defined epoch. This is
    /// not wall-clock time and is not comparable across different hosts. `None`
    /// means the source did not provide a timestamp.
    pub host_time: Option<Duration>,
}

/// An owned, nonempty buffer of interleaved `f32` samples.
///
/// Construction may allocate to discard excess vector capacity. Construct frames
/// on a worker thread, never in a real-time callback. The queue additionally
/// checks its configured maximum sample count and the stream's format/sequence.
#[derive(Debug)]
pub struct PcmFrame {
    format: PcmFormat,
    samples: Vec<f32>,
    timestamp: FrameTimestamp,
}

impl PcmFrame {
    pub fn new(
        format: PcmFormat,
        samples: Vec<f32>,
        timestamp: FrameTimestamp,
    ) -> Result<Self, AudioError> {
        if samples.is_empty() || samples.len() % usize::from(format.channels) != 0 {
            return Err(AudioError::InvalidConfiguration {
                reason: "samples must contain a nonempty whole number of interleaved frames",
            });
        }
        Ok(Self {
            format,
            // Keep the retained sample capacity equal to its length, so a tiny
            // frame cannot smuggle an arbitrarily large allocation into a queue.
            samples: samples.into_boxed_slice().into_vec(),
            timestamp,
        })
    }

    pub fn format(&self) -> PcmFormat {
        self.format
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn timestamp(&self) -> FrameTimestamp {
        self.timestamp
    }

    /// Number of interleaved sample frames (samples per channel), not buffers.
    pub fn frame_count(&self) -> usize {
        self.samples.len() / usize::from(self.format.channels)
    }

    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }
}

/// A backend-specific persistent device UID, not a transient numeric device ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(uid: impl Into<String>) -> Result<Self, AudioError> {
        let uid = uid.into();
        if uid.trim().is_empty() || uid.contains('\0') {
            return Err(AudioError::InvalidConfiguration {
                reason: "device UID must be nonempty and contain no NUL bytes",
            });
        }
        Ok(Self(uid))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Scope controls offered by a backend, not proof of permission or availability.
/// No field implies access to FaceTime's internal media or a specific call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureCapabilities {
    pub process_scoped: bool,
    pub stream_scoped: bool,
    pub device_scoped: bool,
    pub system_audio: bool,
}

/// Opt-in capture boundary. Options belong to the concrete backend so it can
/// require an explicit process/device/stream rather than silently capture all
/// output. Implementations must document permission and OS requirements.
pub trait CaptureBackend {
    type Options;
    type Session: CaptureSession;

    fn capabilities(&self) -> CaptureCapabilities;

    /// Start capture after validating all options. The session owns capture
    /// resources and the stream owns received frames. Dropping the stream must
    /// stop further delivery and cause the backend to stop capture promptly.
    fn start(
        &self,
        options: Self::Options,
        buffer: BufferConfig,
    ) -> Result<(Self::Session, PcmStream), AudioError>;
}

/// Owner of a capture operation. Implementations must stop and release resources
/// on drop. Explicit stop is idempotent and reports cleanup errors when possible.
/// Stopping closes delivery; already queued frames may still be drained.
pub trait CaptureSession {
    fn format(&self) -> PcmFormat;
    fn stop(&mut self) -> Result<(), AudioError>;
}

/// Caller-configured audio route, typically the two sides of a virtual audio
/// device. This value describes a route; creating it does not install a driver,
/// select a FaceTime microphone, change a system default, or establish routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInjectionRoute {
    /// Output device to which the injection backend writes PCM.
    pub sink_output_device: DeviceId,
    /// Input device the caller separately selected in FaceTime.
    pub facetime_input_device: DeviceId,
}

/// Implemented by the caller's virtual-device or other explicitly configured
/// routing backend. The crate provides no concrete FaceTime injection backend.
///
/// Implementations must verify the supplied route and format, expose permission
/// and unsupported errors, use bounded buffering, and never change default
/// devices or request permissions implicitly. A successful open does not prove
/// that FaceTime has selected or consumed this route.
pub trait AudioInjectionBackend {
    type Session: AudioInjectionSession;

    fn open(
        &self,
        route: AudioInjectionRoute,
        format: PcmFormat,
        buffer: BufferConfig,
    ) -> Result<Self::Session, AudioError>;
}

/// Owner of a caller-provided output route. Drop must release its resources.
pub trait AudioInjectionSession {
    fn format(&self) -> PcmFormat;
    fn route(&self) -> &AudioInjectionRoute;

    /// Offer a frame without waiting for device progress or buffer capacity.
    /// Implementations must reject mismatched formats and oversize frames, and
    /// report a full buffer as `Dropped`; `Accepted` means queued, not played or
    /// delivered to a call. Thread safety/real-time safety is backend-specific.
    fn try_write(&mut self, frame: PcmFrame) -> Result<SendOutcome, AudioError>;

    /// Idempotently stop output and release the route's resources.
    fn stop(&mut self) -> Result<(), AudioError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_format_validates_ranges() {
        for (rate, channels) in [(0, 2), (384_001, 2), (48_000, 0), (48_000, 33)] {
            assert!(matches!(
                PcmFormat::new(rate, channels),
                Err(AudioError::UnsupportedFormat { .. })
            ));
        }
        assert!(PcmFormat::new(1, 1).is_ok());
        assert!(PcmFormat::new(384_000, 32).is_ok());
    }

    #[test]
    fn frames_validate_interleaving_and_discard_excess_capacity() {
        let format = PcmFormat::new(48_000, 2).unwrap();
        let timestamp = FrameTimestamp {
            sequence: 0,
            host_time: None,
        };
        assert!(PcmFrame::new(format, vec![], timestamp).is_err());
        assert!(PcmFrame::new(format, vec![0.0; 3], timestamp).is_err());
        let mut samples = Vec::with_capacity(100);
        samples.extend([0.25, -0.25]);
        let frame = PcmFrame::new(format, samples, timestamp).unwrap();
        assert_eq!(frame.frame_count(), 1);
        let samples = frame.into_samples();
        assert_eq!(samples.capacity(), 2);
        assert_eq!(samples, [0.25, -0.25]);
    }

    #[test]
    fn device_uids_are_explicit_nonempty_strings() {
        for uid in ["", "  ", "device\0uid"] {
            assert!(DeviceId::new(uid).is_err());
        }
        assert_eq!(
            DeviceId::new("virtual-audio-device").unwrap().as_str(),
            "virtual-audio-device"
        );
    }
}
