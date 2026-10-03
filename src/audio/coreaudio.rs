//! Experimental CoreAudio process-output capture (macOS 14.2+).
//!
//! This is not a FaceTime media API. The caller must identify the process that
//! actually produces audio, its output device, and its stream. A tap can include
//! ringing and mixed participants. It does not identify calls or remote speakers.
//! No microphone, default-device changes, helper injection, or disk recording is
//! involved. See `docs/audio.md` for permission, packaging, and routing requirements.

use super::{
    AudioError, BufferConfig, CaptureBackend, CaptureCapabilities, CaptureSession, DeviceId,
    PcmFormat, PcmStream,
};

/// Authorization for the side effect of starting system audio capture.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CapturePermission {
    /// Fail before invoking any native capture APIs.
    #[default]
    Refuse,
    /// The application obtained the user's approval to capture this target and
    /// to allow macOS to display its system audio recording permission prompt.
    /// This assertion does not grant or bypass OS permission.
    AllowSystemPrompt,
}

/// Exactly one current PID and output-device stream; never a global tap.
#[derive(Debug, Clone)]
pub struct ProcessTapTarget {
    pid: i32,
    device: DeviceId,
    stream_index: u32,
}

impl ProcessTapTarget {
    /// `stream_index` is the zero-based index in that device's output stream list.
    /// PID selection is caller-owned and must be refreshed after process exit.
    pub fn new(pid: u32, device: DeviceId, stream_index: u32) -> Result<Self, AudioError> {
        if pid == 0 || pid > i32::MAX as u32 || stream_index >= 64 {
            return Err(AudioError::InvalidConfiguration {
                reason: "PID must be positive and fit i32; stream index must be below 64",
            });
        }
        Ok(Self {
            pid: pid as i32,
            device,
            stream_index,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid as u32
    }
    pub fn device(&self) -> &DeviceId {
        &self.device
    }
    pub fn stream_index(&self) -> u32 {
        self.stream_index
    }
}

/// Capture requires both an explicit target and explicit permission policy.
#[derive(Debug, Clone)]
pub struct ProcessTapOptions {
    pub target: ProcessTapTarget,
    pub permission: CapturePermission,
}

/// Provided process-tap backend. Constructing it does not access audio.
#[derive(Debug, Default)]
pub struct CoreAudioTap;

impl CoreAudioTap {
    /// Check OS availability only. Does not inspect devices, check TCC, or prompt.
    pub fn is_supported() -> bool {
        #[cfg(target_os = "macos")]
        // SAFETY: availability-only shim has no arguments or audio side effects.
        unsafe {
            native::rs_tap_available() != 0
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

impl CaptureBackend for CoreAudioTap {
    type Options = ProcessTapOptions;
    type Session = CoreAudioCapture;

    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            process_scoped: true,
            stream_scoped: true,
            device_scoped: true,
            system_audio: true,
        }
    }

    /// May show an OS permission prompt only with `AllowSystemPrompt`. Starting
    /// can block in HAL or the prompt; call outside an async executor's task thread.
    fn start(
        &self,
        options: Self::Options,
        buffer: BufferConfig,
    ) -> Result<(CoreAudioCapture, PcmStream), AudioError> {
        if options.permission != CapturePermission::AllowSystemPrompt {
            return Err(AudioError::PermissionRequired);
        }
        #[cfg(target_os = "macos")]
        {
            native::start(options.target, buffer)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (options, buffer);
            Err(AudioError::UnsupportedPlatform)
        }
    }
}

/// Owns native capture lifetime. Dropping either the stream or this guard ends
/// capture. `stop` cancels delivery, joins the worker and releases the IOProc,
/// private aggregate and tap. Queued PCM drains before stream termination.
///
/// HAL teardown is synchronous and can block; stop/drop outside a realtime or
/// async executor thread. Call `stop` explicitly to observe cleanup errors;
/// `Drop` performs best-effort cleanup. Never stop from an audio callback.
pub struct CoreAudioCapture {
    format: PcmFormat,
    #[cfg(target_os = "macos")]
    stopped: Option<Result<(), AudioError>>,
    #[cfg(target_os = "macos")]
    worker: Option<std::thread::JoinHandle<Result<(), AudioError>>>,
    #[cfg(target_os = "macos")]
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    native_drops: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CoreAudioCapture {
    /// Callback blocks discarded because the native ring was full or a block
    /// exceeded max_samples_per_frame. Separate from `PcmStream` queue statistics.
    pub fn native_dropped_frames(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            self.native_drops.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }
}

impl CaptureSession for CoreAudioCapture {
    fn format(&self) -> PcmFormat {
        self.format
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        #[cfg(target_os = "macos")]
        {
            if let Some(result) = &self.stopped {
                return result.clone();
            }
            self.cancel
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(worker) = self.worker.take() {
                worker.thread().unpark();
                let result = worker.join().unwrap_or(Err(AudioError::Backend {
                    operation: "capture worker panicked",
                    code: 0,
                }));
                self.stopped = Some(result.clone());
                return result;
            }
        }
        Ok(())
    }
}

impl Drop for CoreAudioCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::audio::{pcm_channel, FrameTimestamp, PcmFrame};
    use std::{
        ffi::{c_char, c_void, CString},
        ptr::NonNull,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    #[repr(C)]
    #[derive(Default)]
    struct FrameInfo {
        sequence: u64,
        host_time_ns: u64,
        samples: u32,
        has_host_time: u32,
    }

    extern "C" {
        pub(super) fn rs_tap_available() -> i32;
        fn rs_tap_open(
            pid: i32,
            uid: *const c_char,
            stream: u32,
            capacity: u32,
            max_samples: u32,
            out: *mut *mut c_void,
            rate: *mut u32,
            channels: *mut u16,
            status: *mut i32,
        ) -> i32;
        fn rs_tap_read(tap: *mut c_void, samples: *mut f32, info: *mut FrameInfo) -> i32;
        fn rs_tap_dropped(tap: *mut c_void) -> u64;
        fn rs_tap_health(tap: *mut c_void, status: *mut i32) -> i32;
        fn rs_tap_close(tap: *mut c_void, status: *mut i32) -> i32;
    }

    fn check(kind: i32, status: i32, operation: &'static str) -> Result<(), AudioError> {
        Err(match kind {
            0 => return Ok(()),
            1 => AudioError::UnsupportedOs {
                minimum: "macOS 14.2",
            },
            2 => AudioError::MissingUsageDescription,
            3 => AudioError::DeviceUnavailable,
            4 => AudioError::UnsupportedFormat {
                reason: "tap requires native-endian packed Float32 PCM",
            },
            6 => AudioError::InvalidConfiguration {
                reason: "native buffer allocation or bounds failed",
            },
            7 => AudioError::SourceChanged,
            8 => AudioError::PermissionDenied,
            9 => AudioError::CleanupFailed {
                operation,
                code: status,
            },
            _ => AudioError::Backend {
                operation,
                code: status,
            },
        })
    }

    struct NativeTap(Option<NonNull<c_void>>);
    // SAFETY: moved once to the single consumer worker. Its callback uses only
    // C-owned storage and atomics. No Rust references cross the callback boundary.
    unsafe impl Send for NativeTap {}

    impl NativeTap {
        fn ptr(&self) -> *mut c_void {
            self.0.expect("open native tap").as_ptr()
        }
        fn close(&mut self) -> Result<(), AudioError> {
            let Some(ptr) = self.0.take() else {
                return Ok(());
            };
            let mut status = 0;
            // SAFETY: unique owner, worker no longer reads after this call. C
            // removes the callback before freeing, retaining state on failure.
            let kind = unsafe { rs_tap_close(ptr.as_ptr(), &mut status) };
            check(kind, status, "stop process tap")
        }
    }
    impl Drop for NativeTap {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }

    type Worker = thread::JoinHandle<Result<(), AudioError>>;
    type WorkerTask = Box<dyn FnOnce() -> Result<(), AudioError> + Send>;

    // Keep ownership outside the spawn closure until the OS accepts the worker.
    // Builder::spawn drops its closure on error; a directly captured NativeTap
    // would otherwise hide cleanup failure in Drop. The lock is used only for
    // this startup handoff, never on the HAL callback or per audio frame.
    fn spawn_owned<R: Send + 'static>(
        resource: R,
        spawn: impl FnOnce(WorkerTask) -> std::io::Result<Worker>,
        run: impl FnOnce(R) -> Result<(), AudioError> + Send + 'static,
        cleanup: impl FnOnce(&mut R) -> Result<(), AudioError>,
    ) -> Result<Worker, AudioError> {
        let held = Arc::new(Mutex::new(Some(resource)));
        let worker_held = Arc::clone(&held);
        let task = Box::new(move || {
            let resource = worker_held
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .expect("resource transferred once");
            run(resource)
        });
        match spawn(task) {
            Ok(worker) => Ok(worker),
            Err(error) => {
                let resource = held.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(mut resource) = resource {
                    cleanup(&mut resource)?;
                }
                Err(AudioError::Backend {
                    operation: "spawn capture worker",
                    code: error.raw_os_error().unwrap_or(0),
                })
            }
        }
    }

    pub(super) fn start(
        target: ProcessTapTarget,
        buffer: BufferConfig,
    ) -> Result<(CoreAudioCapture, PcmStream), AudioError> {
        let uid =
            CString::new(target.device.as_str()).map_err(|_| AudioError::InvalidConfiguration {
                reason: "device UID contains NUL",
            })?;
        // Native and Rust queues are separately bounded. Reject before calling HAL.
        if buffer.queue_frames() > 1024 || buffer.max_samples_per_frame() > 16_777_216 {
            return Err(AudioError::InvalidConfiguration {
                reason: "native buffer bounds exceeded",
            });
        }
        let mut ptr = std::ptr::null_mut();
        let (mut rate, mut channels, mut status) = (0, 0, 0);
        // SAFETY: valid borrowed C string; all output pointers are initialized;
        // bounds checked above. This is the sole permission-capable entry point.
        let kind = unsafe {
            rs_tap_open(
                target.pid,
                uid.as_ptr(),
                target.stream_index,
                buffer.queue_frames() as u32,
                buffer.max_samples_per_frame() as u32,
                &mut ptr,
                &mut rate,
                &mut channels,
                &mut status,
            )
        };
        check(kind, status, "start process tap")?;
        let mut tap = NativeTap(Some(NonNull::new(ptr).ok_or(AudioError::Backend {
            operation: "null process tap",
            code: 0,
        })?));
        let prepared = PcmFormat::new(rate, channels).and_then(|format| {
            pcm_channel(format, buffer).map(|(sender, stream)| (format, sender, stream))
        });
        let (format, sender, stream) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                tap.close()?;
                return Err(error);
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let native_drops = Arc::new(AtomicU64::new(0));
        let worker_cancel = Arc::clone(&cancel);
        let drops = Arc::clone(&native_drops);
        let worker = spawn_owned(
            tap,
            |task| {
                thread::Builder::new()
                    .name("rs-facetime-audio".into())
                    .spawn(task)
            },
            move |mut tap| {
                let mut scratch = vec![0.0; buffer.max_samples_per_frame()];
                let mut last_health = Instant::now();
                let mut failure = None;
                'capture: while !worker_cancel.load(Ordering::Acquire) && !sender.is_closed() {
                    for _ in 0..buffer.queue_frames() {
                        if worker_cancel.load(Ordering::Acquire) || sender.is_closed() {
                            break 'capture;
                        }
                        let mut info = FrameInfo::default();
                        // SAFETY: single consumer; scratch sized to the native maximum.
                        if unsafe { rs_tap_read(tap.ptr(), scratch.as_mut_ptr(), &mut info) } == 0 {
                            break;
                        }
                        let timestamp = FrameTimestamp {
                            sequence: info.sequence,
                            host_time: (info.has_host_time != 0)
                                .then(|| Duration::from_nanos(info.host_time_ns)),
                        };
                        let frame = PcmFrame::new(
                            format,
                            scratch[..info.samples as usize].to_vec(),
                            timestamp,
                        );
                        if let Err(error) = frame.and_then(|frame| sender.try_send(frame)) {
                            if !matches!(error, AudioError::Closed) {
                                failure = Some(error);
                            }
                            break 'capture;
                        }
                    }
                    // SAFETY: C atomically publishes this counter, with live unique owner.
                    drops.store(unsafe { rs_tap_dropped(tap.ptr()) }, Ordering::Relaxed);
                    if last_health.elapsed() >= Duration::from_millis(100) {
                        let mut status = 0;
                        // SAFETY: read-only health query on this live session.
                        let kind = unsafe { rs_tap_health(tap.ptr(), &mut status) };
                        if let Err(error) = check(kind, status, "process tap source health") {
                            failure = Some(error);
                            break;
                        }
                        last_health = Instant::now();
                    }
                    thread::park_timeout(Duration::from_millis(5));
                }
                // SAFETY: native state stays live until close directly below.
                drops.store(unsafe { rs_tap_dropped(tap.ptr()) }, Ordering::Relaxed);
                let cleanup = tap.close();
                // Native failures and cleanup errors are observable on the stream.
                sender.finish(failure.or_else(|| cleanup.as_ref().err().cloned()));
                cleanup
            },
            NativeTap::close,
        )?;
        Ok((
            CoreAudioCapture {
                format,
                stopped: None,
                worker: Some(worker),
                cancel,
                native_drops,
            },
            stream,
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failed_spawn_retains_resource_for_explicit_cleanup() {
            for cleanup_fails in [false, true] {
                let cleaned = Arc::new(AtomicBool::new(false));
                let resource = Arc::clone(&cleaned);
                let result = spawn_owned(
                    resource,
                    |_task| Err(std::io::Error::from_raw_os_error(35)),
                    |_| panic!("failed spawn must not execute the worker"),
                    |resource| {
                        resource.store(true, Ordering::Release);
                        if cleanup_fails {
                            Err(AudioError::CleanupFailed {
                                operation: "synthetic close",
                                code: 42,
                            })
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(cleaned.load(Ordering::Acquire));
                if cleanup_fails {
                    assert!(matches!(
                        result,
                        Err(AudioError::CleanupFailed { code: 42, .. })
                    ));
                } else {
                    assert!(matches!(result, Err(AudioError::Backend { code: 35, .. })));
                }
            }
        }

        #[test]
        fn successful_spawn_transfers_resource_to_worker() {
            let ran = Arc::new(AtomicBool::new(false));
            let resource = Arc::clone(&ran);
            let worker = spawn_owned(
                resource,
                |task| thread::Builder::new().spawn(task),
                |resource| {
                    resource.store(true, Ordering::Release);
                    Ok(())
                },
                |_| panic!("successful spawn must not clean up caller's resource"),
            )
            .unwrap();
            worker.join().unwrap().unwrap();
            assert!(ran.load(Ordering::Acquire));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_permission_is_required_before_native_access() {
        let target =
            ProcessTapTarget::new(1, DeviceId::new("synthetic-device").unwrap(), 0).unwrap();
        let result = CoreAudioTap.start(
            ProcessTapOptions {
                target,
                permission: CapturePermission::default(),
            },
            BufferConfig::default(),
        );
        assert!(matches!(result, Err(AudioError::PermissionRequired)));
    }

    #[test]
    fn invalid_target_is_rejected_without_native_access() {
        let device = DeviceId::new("synthetic-device").unwrap();
        assert!(ProcessTapTarget::new(0, device.clone(), 0).is_err());
        assert!(ProcessTapTarget::new(u32::MAX, device.clone(), 0).is_err());
        assert!(ProcessTapTarget::new(1, device, 64).is_err());
    }

    #[cfg(target_os = "macos")]
    fn synthetic_session(
        result: Result<(), AudioError>,
    ) -> (
        CoreAudioCapture,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_observed = Arc::clone(&observed);
        let worker = std::thread::spawn(move || {
            while !worker_cancel.load(Ordering::Acquire) {
                std::thread::park_timeout(std::time::Duration::from_millis(5));
            }
            worker_observed.store(true, Ordering::Release);
            result
        });
        (
            CoreAudioCapture {
                format: PcmFormat::new(48_000, 2).unwrap(),
                stopped: None,
                worker: Some(worker),
                cancel,
                native_drops: Arc::new(AtomicU64::new(0)),
            },
            observed,
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stop_joins_synthetic_worker_and_preserves_cleanup_failure() {
        let failure = AudioError::CleanupFailed {
            operation: "synthetic stop",
            code: 42,
        };
        let (mut session, observed) = synthetic_session(Err(failure.clone()));
        assert_eq!(session.stop(), Err(failure.clone()));
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(session.stop(), Err(failure));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn drop_joins_synthetic_worker() {
        let (session, observed) = synthetic_session(Ok(()));
        drop(session);
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_is_explicit() {
        let target =
            ProcessTapTarget::new(1, DeviceId::new("synthetic-device").unwrap(), 0).unwrap();
        let result = CoreAudioTap.start(
            ProcessTapOptions {
                target,
                permission: CapturePermission::AllowSystemPrompt,
            },
            BufferConfig::default(),
        );
        assert!(matches!(result, Err(AudioError::UnsupportedPlatform)));
    }
}
