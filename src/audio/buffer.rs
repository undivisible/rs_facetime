use super::{AudioError, PcmFormat, PcmFrame};
use std::future::{poll_fn, Future};
use std::mem::size_of;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Maximum retained PCM payload plus queue-slot storage per channel (64 MiB).
/// Allocator bookkeeping, backend buffers, and caller-owned frames are separate.
const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;

/// Fixed queue limits. A "queue frame" is one [`PcmFrame`] buffer; its sample
/// limit counts every channel's samples. Full queues drop the newest offered
/// frame. Limits cannot change while a channel is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferConfig {
    queue_frames: usize,
    max_samples_per_frame: usize,
}

impl BufferConfig {
    /// Set nonzero limits with at most 64 MiB of retained payload and queue slots.
    /// Checked arithmetic rejects oversized values before allocating anything.
    pub fn new(queue_frames: usize, max_samples_per_frame: usize) -> Result<Self, AudioError> {
        let retained_bytes = max_samples_per_frame
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(size_of::<Option<PcmFrame>>()))
            .and_then(|bytes| bytes.checked_mul(queue_frames));
        if queue_frames == 0
            || max_samples_per_frame == 0
            || !matches!(retained_bytes, Some(bytes) if bytes <= MAX_BUFFER_BYTES)
        {
            return Err(AudioError::InvalidConfiguration {
                reason: "queue limits must be nonzero and fit within 64 MiB including frame slots",
            });
        }
        Ok(Self {
            queue_frames,
            max_samples_per_frame,
        })
    }

    pub fn queue_frames(self) -> usize {
        self.queue_frames
    }

    pub fn max_samples_per_frame(self) -> usize {
        self.max_samples_per_frame
    }
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            queue_frames: 8,
            max_samples_per_frame: 16_384,
        }
    }
}

/// Result of offering a valid frame. Neither outcome guarantees device playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Accepted,
    /// The queue was full; this newest frame was discarded.
    Dropped,
}

/// Snapshot of this queue only; native callback buffers may have separate stats.
/// Counters saturate at `u64::MAX`. Rejected invalid frames are not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferStats {
    pub accepted_frames: u64,
    pub dropped_frames: u64,
    pub delivered_frames: u64,
    pub queued_frames: usize,
    pub closed: bool,
}

struct State {
    slots: Box<[Option<PcmFrame>]>,
    head: usize,
    queued: usize,
    last_sequence: Option<u64>,
    closed: bool,
    terminal_error: Option<AudioError>,
    waiter: Option<Waker>,
    accepted: u64,
    dropped: u64,
    delivered: u64,
}

struct Shared {
    format: PcmFormat,
    config: BufferConfig,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // No user processing runs under this mutex. Retaining access after a
        // poisoned lock lets drop/stop still release the queue and wake waiters.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn stats(&self) -> BufferStats {
        let state = self.lock();
        BufferStats {
            accepted_frames: state.accepted,
            dropped_frames: state.dropped,
            delivered_frames: state.delivered,
            queued_frames: state.queued,
            closed: state.closed,
        }
    }
}

impl State {
    fn pop(&mut self) -> Option<PcmFrame> {
        if self.queued == 0 {
            return None;
        }
        let frame = self.slots[self.head].take();
        self.head = (self.head + 1) % self.slots.len();
        self.queued -= 1;
        self.delivered = self.delivered.saturating_add(1);
        frame
    }
}

/// Unique producer for a bounded PCM queue. Drop closes the stream gracefully.
/// Sending/finishing locks a mutex and can deallocate samples or wake executor
/// code; these operations are **not real-time safe**.
pub struct PcmSender {
    shared: Arc<Shared>,
}

/// Single consumer for a bounded PCM queue. Drop closes the sender and discards
/// queued frames. Polling is runtime-independent; no task is spawned internally.
pub struct PcmStream {
    shared: Arc<Shared>,
}

/// Create a synthetic or backend-fed bounded PCM queue. This starts no capture
/// and requests no permissions. The channel format remains fixed for its life.
pub fn pcm_channel(
    format: PcmFormat,
    config: BufferConfig,
) -> Result<(PcmSender, PcmStream), AudioError> {
    if config.max_samples_per_frame < usize::from(format.channels()) {
        return Err(AudioError::InvalidConfiguration {
            reason: "sample limit must fit at least one complete interleaved frame",
        });
    }
    let shared = Arc::new(Shared {
        format,
        config,
        state: Mutex::new(State {
            slots: (0..config.queue_frames).map(|_| None).collect(),
            head: 0,
            queued: 0,
            last_sequence: None,
            closed: false,
            terminal_error: None,
            waiter: None,
            accepted: 0,
            dropped: 0,
            delivered: 0,
        }),
    });
    Ok((
        PcmSender {
            shared: Arc::clone(&shared),
        },
        PcmStream { shared },
    ))
}

impl PcmSender {
    /// Offer without waiting for queue capacity. Mutex contention can still
    /// briefly block. Full queues discard this frame and increment dropped stats.
    /// Sequence numbers must increase even after a dropped frame. Invalid frames
    /// return an error without closing the stream or advancing its sequence.
    pub fn try_send(&self, frame: PcmFrame) -> Result<SendOutcome, AudioError> {
        let mut state = self.shared.lock();
        if state.closed {
            return Err(AudioError::Closed);
        }
        if frame.format() != self.shared.format {
            return Err(AudioError::FormatChanged {
                expected: self.shared.format,
                actual: frame.format(),
            });
        }
        if frame.samples().len() > self.shared.config.max_samples_per_frame {
            return Err(AudioError::InvalidConfiguration {
                reason: "frame exceeds the configured sample limit",
            });
        }
        let sequence = frame.timestamp().sequence;
        if matches!(state.last_sequence, Some(previous) if sequence <= previous) {
            return Err(AudioError::InvalidConfiguration {
                reason: "frame sequence must strictly increase without wrapping",
            });
        }
        state.last_sequence = Some(sequence);
        if state.queued == state.slots.len() {
            state.dropped = state.dropped.saturating_add(1);
            return Ok(SendOutcome::Dropped);
        }
        let tail = (state.head + state.queued) % state.slots.len();
        state.slots[tail] = Some(frame);
        state.queued += 1;
        state.accepted = state.accepted.saturating_add(1);
        let waiter = state.waiter.take();
        drop(state);
        if let Some(waiter) = waiter {
            waiter.wake();
        }
        Ok(SendOutcome::Accepted)
    }

    /// Close idempotently. The first close wins: queued frames drain before an
    /// optional terminal error is delivered exactly once, followed by end-of-
    /// stream. This wakes a pending consumer without holding the queue lock.
    pub fn finish(&self, error: Option<AudioError>) {
        let mut state = self.shared.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        state.terminal_error = error;
        let waiter = state.waiter.take();
        drop(state);
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    /// True after finish, or after the consumer was dropped. Backend workers can
    /// check this to stop capture when the caller abandons the stream.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().closed
    }

    pub fn stats(&self) -> BufferStats {
        self.shared.stats()
    }
}

impl Drop for PcmSender {
    fn drop(&mut self) {
        self.finish(None);
    }
}

impl PcmStream {
    pub fn format(&self) -> PcmFormat {
        self.shared.format
    }

    pub fn stats(&self) -> BufferStats {
        self.shared.stats()
    }

    /// Return a frame, or `Ok(None)` when temporarily empty and open. After
    /// draining queued frames, a terminal error is returned once; subsequent
    /// calls return `Err(AudioError::Closed)`. Graceful closure returns `Closed`
    /// directly. Unlike `poll_next`, this does not register a wakeup.
    pub fn try_next(&mut self) -> Result<Option<PcmFrame>, AudioError> {
        let mut state = self.shared.lock();
        if let Some(frame) = state.pop() {
            return Ok(Some(frame));
        }
        if let Some(error) = state.terminal_error.take() {
            return Err(error);
        }
        if state.closed {
            return Err(AudioError::Closed);
        }
        Ok(None)
    }

    /// Poll for the next frame. After queued frames drain, an optional terminal
    /// error is emitted once, then every poll returns `Ready(None)`. Only the
    /// latest poller's waker is retained, and it is called outside the mutex.
    pub fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<PcmFrame, AudioError>>> {
        // RawWaker clone/drop/wake hooks may run arbitrary executor code. Do all
        // of those outside the mutex, including replacing an older registration.
        let next_waiter = cx.waker().clone();
        let mut state = self.shared.lock();
        let result = if let Some(frame) = state.pop() {
            Poll::Ready(Some(Ok(frame)))
        } else if let Some(error) = state.terminal_error.take() {
            Poll::Ready(Some(Err(error)))
        } else if state.closed {
            Poll::Ready(None)
        } else {
            let old_waiter = state.waiter.replace(next_waiter);
            drop(state);
            drop(old_waiter);
            return Poll::Pending;
        };
        let old_waiter = state.waiter.take();
        drop(state);
        drop(old_waiter);
        drop(next_waiter);
        result
    }

    /// Await one frame without requiring a particular async runtime. Dropping
    /// this future cancels only this wait; dropping the stream closes delivery.
    /// Call the capture session's `stop` to synchronously release its resources.
    pub fn recv(&mut self) -> impl Future<Output = Option<Result<PcmFrame, AudioError>>> + '_ {
        poll_fn(|cx| self.poll_next(cx))
    }
}

impl Drop for PcmStream {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.closed = true;
        state.terminal_error = None;
        // Move samples and any registered waker out before dropping them.
        let slots = std::mem::take(&mut state.slots);
        state.queued = 0;
        let waiter = state.waiter.take();
        drop(state);
        drop(slots);
        drop(waiter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FrameTimestamp;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    fn format() -> PcmFormat {
        PcmFormat::new(48_000, 2).unwrap()
    }

    fn frame(sequence: u64) -> PcmFrame {
        PcmFrame::new(
            format(),
            vec![0.25, -0.25],
            FrameTimestamp {
                sequence,
                host_time: None,
            },
        )
        .unwrap()
    }

    #[derive(Default)]
    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct CheckUnlocked {
        shared: std::sync::Weak<Shared>,
        checks: Arc<AtomicUsize>,
    }

    impl CheckUnlocked {
        fn check(&self) {
            if let Some(shared) = self.shared.upgrade() {
                assert!(
                    shared.state.try_lock().is_ok(),
                    "waker ran under queue lock"
                );
                self.checks.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl Wake for CheckUnlocked {
        fn wake(self: Arc<Self>) {
            self.check();
        }
    }

    impl Drop for CheckUnlocked {
        fn drop(&mut self) {
            self.check();
        }
    }

    #[test]
    fn rejects_zero_overflow_and_excessive_bounds() {
        for (queue, samples) in [
            (0, 2),
            (1, 0),
            (usize::MAX, 1),
            (1, usize::MAX),
            (1, MAX_BUFFER_BYTES / 4),
            (MAX_BUFFER_BYTES, 1),
        ] {
            assert!(BufferConfig::new(queue, samples).is_err());
        }
        assert!(BufferConfig::new(1, 1).is_ok());
        let largest_frame = (MAX_BUFFER_BYTES - size_of::<Option<PcmFrame>>()) / 4;
        assert!(BufferConfig::new(1, largest_frame).is_ok());
        assert!(BufferConfig::new(1, largest_frame + 1).is_err());
        assert!(pcm_channel(format(), BufferConfig::new(1, 1).unwrap()).is_err());
    }

    #[test]
    fn drop_newest_preserves_order_and_reports_loss() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::new(2, 2).unwrap()).unwrap();
        assert_eq!(sender.try_send(frame(0)).unwrap(), SendOutcome::Accepted);
        assert_eq!(sender.try_send(frame(1)).unwrap(), SendOutcome::Accepted);
        assert_eq!(sender.try_send(frame(2)).unwrap(), SendOutcome::Dropped);
        assert_eq!(stream.try_next().unwrap().unwrap().timestamp().sequence, 0);
        assert!(sender.try_send(frame(2)).is_err());
        assert_eq!(sender.try_send(frame(3)).unwrap(), SendOutcome::Accepted);
        assert_eq!(stream.try_next().unwrap().unwrap().timestamp().sequence, 1);
        assert_eq!(stream.try_next().unwrap().unwrap().timestamp().sequence, 3);
        assert!(stream.try_next().unwrap().is_none());
        assert_eq!(
            stream.stats(),
            BufferStats {
                accepted_frames: 3,
                dropped_frames: 1,
                delivered_frames: 3,
                queued_frames: 0,
                closed: false
            }
        );
    }

    #[test]
    fn invalid_format_or_size_does_not_consume_sequence() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::new(1, 2).unwrap()).unwrap();
        let timestamp = FrameTimestamp {
            sequence: 0,
            host_time: None,
        };
        let wrong_format = PcmFormat::new(44_100, 2).unwrap();
        let wrong = PcmFrame::new(wrong_format, vec![0.0; 2], timestamp).unwrap();
        assert!(matches!(
            sender.try_send(wrong),
            Err(AudioError::FormatChanged { .. })
        ));
        let oversized = PcmFrame::new(format(), vec![0.0; 4], timestamp).unwrap();
        assert!(sender.try_send(oversized).is_err());
        assert_eq!(sender.try_send(frame(0)).unwrap(), SendOutcome::Accepted);
        assert!(stream.try_next().unwrap().is_some());
    }

    #[test]
    fn sender_drop_drains_and_closes() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        sender.try_send(frame(1)).unwrap();
        drop(sender);
        assert_eq!(stream.try_next().unwrap().unwrap().timestamp().sequence, 1);
        assert!(matches!(stream.try_next(), Err(AudioError::Closed)));
        assert!(matches!(stream.try_next(), Err(AudioError::Closed)));
    }

    #[test]
    fn terminal_error_is_delivered_once_after_queue_and_first_finish_wins() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        sender.try_send(frame(1)).unwrap();
        sender.finish(Some(AudioError::PermissionDenied));
        sender.finish(Some(AudioError::DeviceUnavailable));
        assert!(stream.try_next().unwrap().is_some());
        assert!(matches!(
            stream.try_next(),
            Err(AudioError::PermissionDenied)
        ));
        assert!(matches!(stream.try_next(), Err(AudioError::Closed)));
        assert!(matches!(sender.try_send(frame(2)), Err(AudioError::Closed)));
    }

    #[test]
    fn receiver_drop_closes_sender_and_releases_frames() {
        let (sender, stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        sender.try_send(frame(1)).unwrap();
        drop(stream);
        assert!(sender.is_closed());
        assert_eq!(sender.stats().queued_frames, 0);
        assert!(matches!(sender.try_send(frame(2)), Err(AudioError::Closed)));
    }

    #[test]
    fn poll_registers_latest_waiter_and_wakes_on_send_and_close() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        let first = Arc::new(CountWake::default());
        let second = Arc::new(CountWake::default());
        let first_waker = Waker::from(Arc::clone(&first));
        let second_waker = Waker::from(Arc::clone(&second));
        assert!(stream
            .poll_next(&mut Context::from_waker(&first_waker))
            .is_pending());
        assert!(stream
            .poll_next(&mut Context::from_waker(&second_waker))
            .is_pending());
        sender.try_send(frame(1)).unwrap();
        assert_eq!(first.0.load(Ordering::SeqCst), 0);
        assert_eq!(second.0.load(Ordering::SeqCst), 1);
        assert!(matches!(
            stream.poll_next(&mut Context::from_waker(&second_waker)),
            Poll::Ready(Some(Ok(_)))
        ));
        assert!(stream
            .poll_next(&mut Context::from_waker(&second_waker))
            .is_pending());
        sender.finish(Some(AudioError::SourceChanged));
        assert_eq!(second.0.load(Ordering::SeqCst), 2);
        assert!(matches!(
            stream.poll_next(&mut Context::from_waker(&second_waker)),
            Poll::Ready(Some(Err(AudioError::SourceChanged)))
        ));
        assert!(matches!(
            stream.poll_next(&mut Context::from_waker(&second_waker)),
            Poll::Ready(None)
        ));
        assert!(matches!(
            stream.poll_next(&mut Context::from_waker(&second_waker)),
            Poll::Ready(None)
        ));
    }

    #[test]
    fn recv_wait_is_cancel_safe_and_handles_preexisting_data() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        let counter = Arc::new(CountWake::default());
        let waker = Waker::from(counter);
        let mut cx = Context::from_waker(&waker);
        let mut waiting = Box::pin(stream.recv());
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        drop(waiting);
        sender.try_send(frame(1)).unwrap();
        let mut next = Box::pin(stream.recv());
        assert!(matches!(
            next.as_mut().poll(&mut cx),
            Poll::Ready(Some(Ok(_)))
        ));
        drop(next);
        drop(sender);
        assert!(matches!(stream.poll_next(&mut cx), Poll::Ready(None)));
    }

    #[test]
    fn executor_wake_and_drop_hooks_run_outside_queue_lock() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::default()).unwrap();
        let checks = Arc::new(AtomicUsize::new(0));
        let first = Waker::from(Arc::new(CheckUnlocked {
            shared: Arc::downgrade(&sender.shared),
            checks: Arc::clone(&checks),
        }));
        assert!(stream
            .poll_next(&mut Context::from_waker(&first))
            .is_pending());
        drop(first);

        let second = Waker::from(Arc::new(CheckUnlocked {
            shared: Arc::downgrade(&sender.shared),
            checks: Arc::clone(&checks),
        }));
        assert!(stream
            .poll_next(&mut Context::from_waker(&second))
            .is_pending());
        assert_eq!(checks.load(Ordering::SeqCst), 1); // replaced first waker
        drop(second);
        drop(sender);
        assert_eq!(checks.load(Ordering::SeqCst), 3); // wake and final drop
    }

    #[test]
    fn producer_thread_and_consumer_preserve_bounds_and_order() {
        let (sender, mut stream) = pcm_channel(format(), BufferConfig::new(3, 2).unwrap()).unwrap();
        let producer = std::thread::spawn(move || {
            for sequence in 0..1000 {
                sender.try_send(frame(sequence)).unwrap();
                assert!(sender.stats().queued_frames <= 3);
            }
        });
        let mut previous = None;
        let mut delivered = 0;
        loop {
            match stream.try_next() {
                Ok(Some(frame)) => {
                    let sequence = frame.timestamp().sequence;
                    assert!(!matches!(previous, Some(old) if sequence <= old));
                    previous = Some(sequence);
                    delivered += 1;
                }
                Ok(None) => std::thread::yield_now(),
                Err(AudioError::Closed) => break,
                Err(error) => panic!("unexpected stream error: {error}"),
            }
        }
        producer.join().unwrap();
        let stats = stream.stats();
        assert_eq!(stats.accepted_frames + stats.dropped_frames, 1000);
        assert_eq!(stats.accepted_frames, delivered);
        assert_eq!(stats.delivered_frames, delivered);
        assert_eq!(stats.queued_frames, 0);
    }
}
