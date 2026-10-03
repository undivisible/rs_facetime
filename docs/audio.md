# Optional audio

The 0.2 API keeps call control and audio separate. Existing `BridgeClient` and
helper IPC actions are unchanged. Features remain additive and disabled by
default; `audio` and `coreaudio-capture` do not enable `private-api` or `cli`.
Rust 1.85+ is required. Version 0.2 is the development version in this checkout,
not a claim that a package has been published.

| Feature | Platforms | What it supplies |
| --- | --- | --- |
| `audio` | Rust platforms with `std` | PCM frames, bounded channel, backend traits |
| `coreaudio-capture` | Builds native code only for macOS | `CoreAudioTap`; enables `audio` |
| `coreaudio-capture` elsewhere | Portable API and stub | `UnsupportedPlatform` on authorized start |
| `private-api`, `cli` | Existing macOS call control | No new media or routing actions |

The native adapter needs Apple command-line tools and a macOS SDK with process
taps (14.2+). It uses the installed SDK without downloading a dependency. At
runtime it checks macOS 14.2 availability. SDK 27 headers were inspected during
development; this does not constitute runtime validation on every supported OS.

## Capture scope and permissions

`CoreAudioTap` captures output from exactly one caller-selected PID, output
device UID, and output stream index. There is no all-system fallback, default
device selection, automatic PID search, or process relaunch retargeting. The
caller must determine which process actually produces the desired audio; it
may be a service rather than the visible FaceTime process. PID selection is a
snapshot, so start promptly and refresh it after a process exits. A tap is not
associated with a call UUID and may contain ringing or mixed participants.
It does not expose FaceTime's internal PCM, RTP, per-participant tracks, or the
microphone signal.

Constructing options, checking `is_supported`, and enabling a feature do not
capture audio. Starting requires `CapturePermission::AllowSystemPrompt`; the
default `Refuse` returns `PermissionRequired` before native access. The host app
must include a nonempty `NSAudioCaptureUsageDescription` in its own Info.plist
and explain capture to its user. The native adapter checks that key; this
library cannot supply the host application's usage description or consent.
A CLI host needs an appropriate app bundle or embedded Info.plist of its own.

Apple documents a system audio recording prompt when an aggregate device with
a tap starts recording for the first time. The public API used here provides
no nonprompting authorization preflight: `AllowSystemPrompt` is the caller's
explicit acknowledgement of that possible side effect, not proof of granted
OS access. An explicit HAL permission failure maps to `PermissionDenied`;
other HAL failures preserve their OSStatus. A lack of frames is not evidence
that permission succeeded, that the target is correct, or that a call is silent.
Applications should impose their own startup/inactivity deadline and call stop.

The adapter creates a private, nonpersistent aggregate and a private, unmuted
tap. It does not modify default devices, install drivers, change SIP/TCC, save
audio, or start FaceTime. Microphone permission is a separate concern for a
caller backend that uses a microphone; this adapter does not capture one.

## Owning and processing PCM

Frames contain interleaved `f32` PCM with a fixed, validated rate and channel
count. The adapter accepts native Float32 interleaved or planar input and
interleaves planar samples; it does not resample, clip, normalize, or mix a
microphone. Unsupported native formats fail explicitly. Samples may exceed
the conventional -1 to 1 range; processing and any output conversion belong
to the caller.

`PcmFrame` owns its samples, so callers may process, transfer, or retain them
after the native callback returns. Sequence numbers count buffers, not sample
frames. Gaps indicate possible dropped buffers. `host_time` is the first sample's
monotonic host time expressed as a duration from an unspecified epoch, not a
wall-clock time. An absent native timestamp remains `None`.

```rust
use rs_facetime::audio::{pcm_channel, BufferConfig, FrameTimestamp, PcmFormat, PcmFrame};

let format = PcmFormat::new(48_000, 2)?;
let (sender, mut stream) = pcm_channel(format, BufferConfig::new(8, 4096)?)?;
sender.try_send(PcmFrame::new(
    format, vec![0.25, -0.25],
    FrameTimestamp { sequence: 0, host_time: None },
)?)?;
sender.finish(None);
if let Some(frame) = stream.try_next()? {
    // Perform DSP using frame.samples(); the caller owns frame.
}
# Ok::<(), rs_facetime::audio::AudioError>(())
```

`stream.next().await` and `poll_next` work without an async runtime dependency.
Only one consumer owns the stream. Applications can call their own processing
callback from that consumer; arbitrary caller callbacks never run on the HAL
audio thread. `try_next` returns `Ok(None)` while temporarily empty, then
`Err(Closed)` after draining a closed channel. Async/poll consumption yields
an optional terminal error once and then `None`. Dropping a pending `next`
future cancels that wait; dropping the stream terminates capture delivery.

`BufferConfig` bounds both queued buffers and samples per buffer, with at most
64 MiB of retained PCM plus slot storage per portable channel. Full queues drop
the newest buffer and increment `BufferStats::dropped_frames`. Invalid format,
size, or nonincreasing sequence values return errors rather than entering the
queue. Queues do not accumulate unbounded async tasks or block for capacity.
Their short mutex sections and frame allocations are not realtime-safe.

The native callback uses a separately preallocated SPSC ring and performs no
allocation, logging, blocking locks, or Rust/user callbacks. Its full-ring and
oversize-block drops are available through `native_dropped_frames`. The worker
polls at approximately 5 ms and delivers at most `queue_frames` buffers per
iteration; this is not a latency guarantee. Choose bounds to suit the expected
HAL block size and consumer speed; oversized HAL blocks are discarded whole.
The native adapter additionally caps queue_frames at 1024. It retains
`(queue_frames + 1) * max_samples_per_frame * 4` bytes in the ring, one scratch
buffer, and up to one in-flight owned frame, in addition to the Rust queue.
Thus memory is bounded but the 64 MiB portable limit is not the whole-session
limit. Caller-retained frames are outside either bound.

## Starting and stopping the provided adapter

The following is an API sketch, deliberately not a runnable capture example.
Run it only after separate user approval for the actual target and OS prompt.
Replace the target with one the application has explicitly identified.

```rust,no_run
use rs_facetime::audio::{BufferConfig, CaptureBackend, CaptureSession, DeviceId};
use rs_facetime::audio::coreaudio::{
    CapturePermission, CoreAudioTap, ProcessTapOptions, ProcessTapTarget,
};

let target = ProcessTapTarget::new(12345, DeviceId::new("chosen-output-device-uid")?, 0)?;
let (mut capture, mut stream) = CoreAudioTap.start(
    ProcessTapOptions { target, permission: CapturePermission::AllowSystemPrompt },
    BufferConfig::default(),
)?;
// Consume stream on your own worker/task, respecting an application deadline.
// Never call start/stop on an audio callback or block an async executor thread.
capture.stop()?;
# Ok::<(), rs_facetime::audio::AudioError>(())
```

Keep the capture guard alive while receiving. Dropping it cancels the worker,
joins it, stops the IOProc and destroys the private aggregate and tap. Dropping
the receiver also causes the worker to stop. Explicit `stop` is idempotent and
reports teardown errors; `Drop` is best effort. Stop/start call synchronous HAL
APIs and may block. A cleanup failure is also reported as a terminal stream
error when no earlier error exists. Failed startup prioritizes `CleanupFailed`
over its original error if releasing partially initialized resources fails,
because it cannot return a session guard. If HAL refuses to remove its IOProc, the
adapter retains callback state rather than freeing memory still in use; the
failure must be surfaced to the application, and OS cleanup may require process
exit. Callback acceptance is disabled before teardown, so subsequent callbacks
discard input even when HAL retains the callback. The HAL tap can remain active
until exit; do not equate a failed stop with confirmed HAL cessation. Repeated
stop calls preserve the original cleanup error.

The worker checks process identity, device liveness, the selected stream index's
object mapping, and source format about
every 100 ms. A changed or unavailable source terminates the stream, with no
automatic reconfiguration. The application must explicitly choose a new target
and restart. An idle process may remain valid without producing any frames.

## Sending audio through a caller-owned route

`AudioInjectionBackend` is an integration boundary, not a supplied FaceTime
injection implementation. A backend must actually write PCM to an installed,
configured route (for example, the output side of a virtual audio device), and
FaceTime must use the corresponding input. `AudioInjectionRoute` names both
device UIDs to make that relationship explicit. Constructing the route does
not establish it or verify FaceTime selection. `try_write` must validate format
and size and use bounded buffering; `Accepted` means queued, not heard by a
remote party. The backend owns conversion, underrun behavior, device lifetime,
permission handling, and independently verifying its route.

FaceTime's documented Video menu lets a user choose a microphone and output
device. That supports manual routing; it is not documentation of a public
programmatic per-FaceTime input-selection API. Driver installation, FaceTime
device selection, and any microphone capture are separate user actions. This
crate neither installs a virtual driver nor changes a system default. No live
FaceTime capture or injection has been verified as part of this implementation.

## Safe verification

All automated checks use generated PCM, refused permission, platform stubs, or
mock teardown functions. No test starts a native capture session or a call.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo check --all-features
cargo build --all-features
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo run --example audio_synthetic --features audio
sh scripts/test-audio-native.sh  # macOS; ASan/UBSan, synthetic callback and cleanup
```

CI also tests all features on Linux with Rust 1.85 to cover the unsupported
platform stub. Building the native adapter is not evidence of authorized live
capture, permissions, device compatibility, or FaceTime end-to-end routing.

## Primary references

Reviewed 2026-10-03, alongside the installed Apple SDK headers:

- [Capturing system audio with Core Audio taps](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps): process-output scope, aggregate input, macOS 14.2, usage description and prompt.
- [CATapDescription](https://developer.apple.com/documentation/coreaudio/catapdescription): process, device and stream scope, private and mute configuration.
- [AudioHardwareCreateProcessTap](https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap(_:_:)): API availability, also confirmed by `AudioHardwareTapping.h`.
- [Creating an Audio Server Driver Plug-in](https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in): an actual virtual audio device requires a driver implementation and installation.
- [Choose a camera or microphone for FaceTime calls on Mac](https://support.apple.com/guide/facetime/choose-a-camera-or-microphone-fctm26739220/mac): manual microphone/output-device selection.
