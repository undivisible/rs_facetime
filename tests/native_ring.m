// Synthetic-only checks. No function that opens, enumerates, starts, or stops
// an audio device is called. Include implementation to exercise the real copy path.
#include "../native/process_tap.m"
#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>

enum { CONCURRENT_BLOCKS = 10000 };
typedef struct { RSTap *tap; _Atomic bool done; } Producer;

static void *produce_generated_audio(void *context) {
    Producer *producer = context;
    for (unsigned i = 0; i < CONCURRENT_BLOCKS; i++) {
        float samples[] = { (float)i, (float)i + 0.25f, (float)i + 0.5f, (float)i + 0.75f };
        AudioBufferList input = { .mNumberBuffers = 1,
            .mBuffers = {{ 2, sizeof(samples), samples }} };
        receive_audio(0, NULL, &input, NULL, NULL, NULL, producer->tap);
    }
    atomic_store_explicit(&producer->done, true, memory_order_release);
    return NULL;
}

static void test_concurrent_ring(void) {
    RSTap s = {0};
    RSSlot slots[9] = {0};
    float storage[9][4] = {0};
    for (unsigned i = 0; i < 9; i++) slots[i].samples = storage[i];
    s.capacity = 9; s.max_samples = 4; s.slots = slots;
    s.timebase.numer = s.timebase.denom = 1;
    s.format = (AudioStreamBasicDescription){ .mSampleRate = 48000,
        .mFormatID = kAudioFormatLinearPCM,
        .mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        .mBytesPerPacket = 8, .mFramesPerPacket = 1, .mBytesPerFrame = 8,
        .mChannelsPerFrame = 2, .mBitsPerChannel = 32 };
    atomic_init(&s.read_index, 0); atomic_init(&s.write_index, 0);
    atomic_init(&s.dropped, 0); atomic_init(&s.malformed, false);
    atomic_init(&s.accepting, true);
    Producer producer = { .tap = &s };
    atomic_init(&producer.done, false);
    pthread_t thread;
    assert(pthread_create(&thread, NULL, produce_generated_audio, &producer) == 0);
    uint64_t received = 0, previous = 0;
    for (;;) {
        // Observe completion before reading: a final published slot must still
        // drain if the producer finishes immediately after an empty read.
        bool done = atomic_load_explicit(&producer.done, memory_order_acquire);
        float samples[4];
        RSFrameInfo info;
        if (rs_tap_read(&s, samples, &info)) {
            assert(info.samples == 4 && !info.has_host_time);
            assert(!received || info.sequence > previous);
            for (unsigned channel = 0; channel < 4; channel++)
                assert(samples[channel] == (float)info.sequence + channel * 0.25f);
            previous = info.sequence;
            received++;
        } else if (done) {
            break;
        } else {
            sched_yield();
        }
    }
    assert(pthread_join(thread, NULL) == 0);
    assert(received > 0 && received + rs_tap_dropped(&s) == CONCURRENT_BLOCKS);
    assert(!atomic_load(&s.malformed));
}

int main(void) {
    test_concurrent_ring();
    RSTap s = {0};
    RSSlot slots[3] = {0};
    float storage[3][8] = {0};
    for (unsigned i = 0; i < 3; i++) slots[i].samples = storage[i];
    s.capacity = 3; s.max_samples = 8; s.slots = slots;
    atomic_init(&s.read_index, 0); atomic_init(&s.write_index, 0);
    atomic_init(&s.dropped, 0); atomic_init(&s.malformed, false);
    atomic_init(&s.accepting, true);
    s.timebase.numer = 125; s.timebase.denom = 3;
    s.format = (AudioStreamBasicDescription){ .mSampleRate = 48000,
        .mFormatID = kAudioFormatLinearPCM,
        .mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        .mBytesPerPacket = 8, .mFramesPerPacket = 1, .mBytesPerFrame = 8,
        .mChannelsPerFrame = 2, .mBitsPerChannel = 32 };
    assert(supported(s.format));
    AudioTimeStamp when = { .mHostTime = 300, .mFlags = kAudioTimeStampHostTimeValid };
    float samples[] = { 1, 2, 3, 4 };
    AudioBufferList input = { .mNumberBuffers = 1, .mBuffers = {{ 2, sizeof(samples), samples }} };
    receive_audio(0, NULL, &input, &when, NULL, NULL, &s);
    samples[0] = 5;
    receive_audio(0, NULL, &input, NULL, NULL, NULL, &s);
    receive_audio(0, NULL, &input, NULL, NULL, NULL, &s);
    assert(rs_tap_dropped(&s) == 1);
    RSFrameInfo info;
    float out[8] = {0};
    assert(rs_tap_read(&s, out, &info) == 1);
    assert(info.sequence == 0 && info.samples == 4 && info.host_time_ns == 12500);
    assert(info.has_host_time && out[0] == 1 && out[3] == 4);
    assert(rs_tap_read(&s, out, &info) == 1 && out[0] == 5);
    assert(info.sequence == 1 && !info.has_host_time);
    assert(rs_tap_read(&s, out, &info) == 0);
    receive_audio(0, NULL, &input, NULL, NULL, NULL, &s);
    assert(rs_tap_read(&s, out, &info) == 1 && info.sequence == 3);

    // Planar input is interleaved into owned slots; ring wraps safely.
    s.format.mFormatFlags |= kAudioFormatFlagIsNonInterleaved;
    s.format.mBytesPerPacket = s.format.mBytesPerFrame = 4;
    float left[] = { 10, 30 }, right[] = { 20, 40 };
    struct { UInt32 count; AudioBuffer buffers[2]; } planar = {
        2, {{1, sizeof(left), left}, {1, sizeof(right), right}}
    };
    assert(supported(s.format));
    receive_audio(0, NULL, (const AudioBufferList *)&planar, NULL, NULL, NULL, &s);
    assert(rs_tap_read(&s, out, &info) == 1);
    assert(out[0] == 10 && out[1] == 20 && out[2] == 30 && out[3] == 40);
    for (unsigned i = 0; i < 100; i++) {
        receive_audio(0, NULL, (const AudioBufferList *)&planar, NULL, NULL, NULL, &s);
        assert(rs_tap_read(&s, out, &info) == 1);
    }
    // Oversize frames drop without copying; malformed layout becomes terminal.
    s.max_samples = 2;
    receive_audio(0, NULL, (const AudioBufferList *)&planar, NULL, NULL, NULL, &s);
    assert(rs_tap_dropped(&s) == 2 && rs_tap_read(&s, out, &info) == 0);
    planar.buffers[1].mDataByteSize = 4;
    receive_audio(0, NULL, (const AudioBufferList *)&planar, NULL, NULL, NULL, &s);
    assert(atomic_load(&s.malformed));
    int32_t status = 0;
    assert(rs_tap_health(&s, &status) == RS_CHANGED); // Returns before HAL queries.
    s.format.mFormatFlags |= kAudioFormatFlagIsBigEndian;
    assert(!supported(s.format));
    // Cancellation is checked before touching even an invalid input buffer.
    atomic_store(&s.accepting, false);
    atomic_store(&s.malformed, false);
    uint64_t sequence = s.sequence;
    receive_audio(0, NULL, NULL, NULL, NULL, NULL, &s);
    assert(!atomic_load(&s.malformed) && s.sequence == sequence);
    // Reordered stream objects must not silently retarget capture by index.
    s.stream_index = 1; s.stream = 12;
    AudioObjectID original[] = {11, 12}, reordered[] = {12, 11};
    assert(selected_stream_matches(&s, original, sizeof(original)));
    assert(!selected_stream_matches(&s, reordered, sizeof(reordered)));
    assert(!selected_stream_matches(&s, original, sizeof(AudioObjectID)));
    assert(!selected_stream_matches(&s, original, 3));
    puts("native ring: concurrent ownership/order/drop accounting and synthetic buffer checks passed");
    return 0;
}
