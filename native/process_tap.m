// A private, nonmuting tap of exactly one process and one output-device stream.
// The HAL callback performs only bounded copies and lock-free atomic operations.
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#include <mach/mach_time.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include "process_tap.h"

typedef struct { RSFrameInfo info; float *samples; } RSSlot;
struct RSTap {
    AudioObjectID process, device, stream, tap, aggregate;
    AudioDeviceIOProcID io;
    int32_t pid;
    AudioStreamBasicDescription format, device_format;
    uint32_t capacity, max_samples, stream_index;
    RSSlot *slots;
    float *storage;
    _Atomic uint32_t read_index, write_index;
    _Atomic uint64_t dropped;
    _Atomic bool malformed;
    _Atomic bool accepting;
    uint64_t sequence;
    mach_timebase_info_data_t timebase;
};

static int32_t backend(OSStatus status, int32_t *out_status) {
    *out_status = status;
    return status == kAudioDevicePermissionsError ? RS_PERMISSION : RS_BACKEND;
}

static OSStatus property(AudioObjectID object, AudioObjectPropertySelector selector,
                         AudioObjectPropertyScope scope, UInt32 *size, void *data) {
    AudioObjectPropertyAddress address = { selector, scope, kAudioObjectPropertyElementMain };
    return AudioObjectGetPropertyData(object, &address, 0, NULL, size, data);
}

static bool same_format(AudioStreamBasicDescription a, AudioStreamBasicDescription b) {
    return a.mSampleRate == b.mSampleRate && a.mFormatID == b.mFormatID &&
        a.mFormatFlags == b.mFormatFlags && a.mBytesPerPacket == b.mBytesPerPacket &&
        a.mFramesPerPacket == b.mFramesPerPacket && a.mBytesPerFrame == b.mBytesPerFrame &&
        a.mChannelsPerFrame == b.mChannelsPerFrame && a.mBitsPerChannel == b.mBitsPerChannel;
}

static bool supported(AudioStreamBasicDescription f) {
    bool planar = (f.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    return f.mFormatID == kAudioFormatLinearPCM &&
        (f.mFormatFlags & kAudioFormatFlagIsFloat) &&
        (f.mFormatFlags & kAudioFormatFlagIsPacked) &&
        !(f.mFormatFlags & (kAudioFormatFlagIsBigEndian | kAudioFormatFlagIsSignedInteger)) &&
        f.mSampleRate >= 1 && f.mSampleRate <= 384000 &&
        f.mSampleRate == (uint32_t)f.mSampleRate &&
        f.mChannelsPerFrame >= 1 && f.mChannelsPerFrame <= 32 &&
        f.mBitsPerChannel == 32 && f.mFramesPerPacket == 1 &&
        f.mBytesPerFrame == sizeof(float) * (planar ? 1 : f.mChannelsPerFrame) &&
        f.mBytesPerPacket == f.mBytesPerFrame;
}

static bool selected_stream_matches(const RSTap *s, const AudioObjectID *streams, UInt32 bytes) {
    return bytes % sizeof(AudioObjectID) == 0 &&
        s->stream_index < bytes / sizeof(AudioObjectID) && streams[s->stream_index] == s->stream;
}

static OSStatus receive_audio(AudioObjectID device, const AudioTimeStamp *now,
                              const AudioBufferList *input, const AudioTimeStamp *when,
                              AudioBufferList *output, const AudioTimeStamp *output_time,
                              void *context) {
    (void)device; (void)now; (void)output; (void)output_time;
    RSTap *s = context;
    if (!atomic_load_explicit(&s->accepting, memory_order_acquire)) return noErr;
    uint64_t sequence = s->sequence++;
    uint32_t channels = s->format.mChannelsPerFrame;
    bool planar = (s->format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    if (!input || input->mNumberBuffers != (planar ? channels : 1)) {
        atomic_store_explicit(&s->malformed, true, memory_order_relaxed);
        return noErr;
    }
    uint32_t bytes = input->mBuffers[0].mDataByteSize;
    if (!bytes) return noErr;
    if (bytes % s->format.mBytesPerFrame) {
        atomic_store_explicit(&s->malformed, true, memory_order_relaxed);
        return noErr;
    }
    uint32_t frames = bytes / s->format.mBytesPerFrame;
    for (uint32_t i = 0; i < input->mNumberBuffers; i++) {
        const AudioBuffer *b = &input->mBuffers[i];
        if (!b->mData || b->mDataByteSize != bytes || b->mNumberChannels != (planar ? 1 : channels)) {
            atomic_store_explicit(&s->malformed, true, memory_order_relaxed);
            return noErr;
        }
    }
    uint32_t write = atomic_load_explicit(&s->write_index, memory_order_relaxed);
    uint32_t next = (write + 1) % s->capacity;
    if (frames > s->max_samples / channels ||
        next == atomic_load_explicit(&s->read_index, memory_order_acquire)) {
        atomic_fetch_add_explicit(&s->dropped, 1, memory_order_relaxed);
        return noErr;
    }
    RSSlot *slot = &s->slots[write];
    uint32_t samples = frames * channels;
    if (planar) {
        for (uint32_t c = 0; c < channels; c++) {
            const float *source = input->mBuffers[c].mData;
            for (uint32_t f = 0; f < frames; f++) slot->samples[f * channels + c] = source[f];
        }
    } else {
        memcpy(slot->samples, input->mBuffers[0].mData, samples * sizeof(float));
    }
    slot->info.sequence = sequence;
    slot->info.samples = samples;
    slot->info.has_host_time = when && (when->mFlags & kAudioTimeStampHostTimeValid);
    // Wide intermediate avoids overflow after long uptime. Bounded integer math.
    slot->info.host_time_ns = slot->info.has_host_time
        ? (uint64_t)(((__uint128_t)when->mHostTime * s->timebase.numer) / s->timebase.denom) : 0;
    atomic_store_explicit(&s->write_index, next, memory_order_release);
    return noErr;
}

int32_t rs_tap_available(void) {
    if (@available(macOS 14.2, *)) return 1;
    return 0;
}

int32_t rs_tap_close(RSTap *s, int32_t *out_status) {
    if (!s) return RS_OK;
    atomic_store_explicit(&s->accepting, false, memory_order_release);
    OSStatus first = noErr;
    if (s->io) {
        first = AudioDeviceStop(s->aggregate, s->io);
        OSStatus removed = AudioDeviceDestroyIOProcID(s->aggregate, s->io);
        // Never free callback state unless HAL has removed the callback. The
        // state contains no Rust pointers, so even this failure remains safe.
        if (removed != noErr) { *out_status = removed; return RS_CLEANUP; }
    }
    if (s->aggregate) {
        OSStatus status = AudioHardwareDestroyAggregateDevice(s->aggregate);
        if (first == noErr) first = status;
    }
    if (s->tap) {
        if (@available(macOS 14.2, *)) {
            OSStatus status = AudioHardwareDestroyProcessTap(s->tap);
            if (first == noErr) first = status;
        }
    }
    free(s->storage); free(s->slots); free(s);
    if (first != noErr) { *out_status = first; return RS_CLEANUP; }
    return RS_OK;
}

// A startup failure must not hide failed teardown: there will be no guard for
// the caller to stop. Prioritize cleanup failure and disclose retained resources.
static int32_t failed_open(RSTap *s, int32_t original, int32_t *out_status) {
    int32_t cleanup_status = 0;
    int32_t cleanup = rs_tap_close(s, &cleanup_status);
    if (cleanup != RS_OK) { *out_status = cleanup_status; return RS_CLEANUP; }
    return original;
}

int32_t rs_tap_open(int32_t pid, const char *uid, uint32_t stream_index,
                    uint32_t capacity, uint32_t max_samples,
                    RSTap **out, uint32_t *rate, uint16_t *channels, int32_t *out_status) {
    *out = NULL; *out_status = 0;
    if (!rs_tap_available()) return RS_OS;
    @autoreleasepool {
        // This check does not request access. AudioDeviceStart below can prompt,
        // and is reached only after explicit authorization in the Rust API.
        id usage = [[NSBundle mainBundle] objectForInfoDictionaryKey:@"NSAudioCaptureUsageDescription"];
        if (![usage isKindOfClass:[NSString class]] || ![(NSString *)usage length]) return RS_USAGE;
        if (pid <= 0 || !uid || !uid[0] || capacity < 1 || capacity > 1024 ||
            max_samples < 1 || max_samples > 16777216 ||
            (uint64_t)capacity * max_samples * sizeof(float) > 67108864) return RS_MEMORY;
        RSTap *s = calloc(1, sizeof(RSTap));
        if (!s) return RS_MEMORY;
        s->pid = pid; s->capacity = capacity + 1; s->max_samples = max_samples;
        s->stream_index = stream_index;
        atomic_init(&s->read_index, 0); atomic_init(&s->write_index, 0);
        atomic_init(&s->dropped, 0); atomic_init(&s->malformed, false);
        atomic_init(&s->accepting, false);
        mach_timebase_info(&s->timebase);
        int32_t result = RS_OK;
        OSStatus status = noErr;
        NSString *device_uid = [NSString stringWithUTF8String:uid];
        if (!atomic_is_lock_free(&s->read_index) || !atomic_is_lock_free(&s->dropped) ||
            !atomic_is_lock_free(&s->malformed) || !atomic_is_lock_free(&s->accepting) ||
            s->timebase.denom == 0) {
            result = RS_OS; goto fail;
        }
        s->slots = calloc(s->capacity, sizeof(RSSlot));
        s->storage = calloc((size_t)s->capacity * max_samples, sizeof(float));
        if (!s->slots || !s->storage) { result = RS_MEMORY; goto fail; }
        for (uint32_t i = 0; i < s->capacity; i++) s->slots[i].samples = s->storage + (size_t)i * max_samples;
        AudioObjectPropertyAddress address = { kAudioHardwarePropertyTranslatePIDToProcessObject,
            kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain };
        UInt32 size = sizeof(s->process);
        status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, sizeof(pid), &pid, &size, &s->process);
        if (status != noErr) goto hal_fail;
        if (!s->process) { result = RS_TARGET; goto fail; }
        if (!device_uid) { result = RS_TARGET; goto fail; }
        CFStringRef cf_uid = (__bridge CFStringRef)device_uid;
        address.mSelector = kAudioHardwarePropertyTranslateUIDToDevice;
        size = sizeof(s->device);
        status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, sizeof(cf_uid), &cf_uid, &size, &s->device);
        if (status != noErr) goto hal_fail;
        if (!s->device) { result = RS_TARGET; goto fail; }
        // Validate the selected stream without enumerating other processes/devices.
        AudioObjectID streams[64];
        size = sizeof(streams);
        status = property(s->device, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput, &size, streams);
        if (status != noErr) goto hal_fail;
        if (stream_index >= size / sizeof(AudioObjectID)) { result = RS_TARGET; goto fail; }
        s->stream = streams[stream_index];
        size = sizeof(s->device_format);
        status = property(s->stream, kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, &size, &s->device_format);
        if (status != noErr) goto hal_fail;
        if (@available(macOS 14.2, *)) {
            CATapDescription *description = [[CATapDescription alloc] initWithProcesses:@[@(s->process)]
                andDeviceUID:device_uid withStream:stream_index];
            description.exclusive = NO; // Never capture all processes except a list.
            description.privateTap = YES;
            description.muteBehavior = CATapUnmuted;
            description.name = @"rs_facetime explicit process capture";
            // Keep compatibility with SDK 14.2 while disabling process restore
            // on newer systems that expose it. Never retarget a relaunched app.
            if ([description respondsToSelector:NSSelectorFromString(@"setProcessRestoreEnabled:")])
                [description setValue:@NO forKey:@"processRestoreEnabled"];
            status = AudioHardwareCreateProcessTap(description, &s->tap);
            if (status != noErr) goto hal_fail;
            size = sizeof(s->format);
            status = property(s->tap, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, &size, &s->format);
            if (status != noErr) goto hal_fail;
            if (!supported(s->format)) { result = RS_FORMAT; goto fail; }
            if (max_samples < s->format.mChannelsPerFrame) { result = RS_MEMORY; goto fail; }
            NSDictionary *aggregate = @{
                @kAudioAggregateDeviceNameKey: @"rs_facetime private capture",
                @kAudioAggregateDeviceUIDKey: [[NSUUID UUID] UUIDString],
                @kAudioAggregateDeviceIsPrivateKey: @YES,
                @kAudioAggregateDeviceTapAutoStartKey: @NO,
                @kAudioAggregateDeviceTapListKey: @[@{
                    @kAudioSubTapUIDKey: description.UUID.UUIDString,
                    @kAudioSubTapDriftCompensationKey: @YES }]
            };
            status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate, &s->aggregate);
            if (status != noErr) goto hal_fail;
            status = AudioDeviceCreateIOProcID(s->aggregate, receive_audio, s, &s->io);
            if (status != noErr) goto hal_fail;
            atomic_store_explicit(&s->accepting, true, memory_order_release);
            status = AudioDeviceStart(s->aggregate, s->io);
            if (status != noErr) goto hal_fail;
        }
        *rate = (uint32_t)s->format.mSampleRate;
        *channels = (uint16_t)s->format.mChannelsPerFrame;
        *out = s;
        return RS_OK;
    hal_fail:
        result = backend(status, out_status);
    fail:
        return failed_open(s, result, out_status);
    }
}

int32_t rs_tap_read(RSTap *s, float *samples, RSFrameInfo *info) {
    uint32_t read = atomic_load_explicit(&s->read_index, memory_order_relaxed);
    if (read == atomic_load_explicit(&s->write_index, memory_order_acquire)) return 0;
    RSSlot *slot = &s->slots[read];
    *info = slot->info;
    memcpy(samples, slot->samples, info->samples * sizeof(float));
    atomic_store_explicit(&s->read_index, (read + 1) % s->capacity, memory_order_release);
    return 1;
}

uint64_t rs_tap_dropped(RSTap *s) { return atomic_load_explicit(&s->dropped, memory_order_relaxed); }

int32_t rs_tap_health(RSTap *s, int32_t *out_status) {
    if (atomic_load_explicit(&s->malformed, memory_order_relaxed)) return RS_CHANGED;
    UInt32 alive = 0, size = sizeof(alive);
    OSStatus status = property(s->device, kAudioDevicePropertyDeviceIsAlive, kAudioObjectPropertyScopeGlobal, &size, &alive);
    if (status != noErr) return backend(status, out_status);
    if (!alive) return RS_TARGET;
    int32_t pid = 0;
    size = sizeof(pid);
    status = property(s->process, kAudioProcessPropertyPID, kAudioObjectPropertyScopeGlobal, &size, &pid);
    if (status != noErr) return backend(status, out_status);
    if (pid != s->pid) return RS_CHANGED;
    AudioObjectID streams[64];
    size = sizeof(streams);
    status = property(s->device, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput, &size, streams);
    if (status != noErr) return backend(status, out_status);
    if (!selected_stream_matches(s, streams, size)) return RS_CHANGED;
    AudioStreamBasicDescription current = {0};
    size = sizeof(current);
    status = property(s->stream, kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, &size, &current);
    if (status != noErr) return backend(status, out_status);
    if (!same_format(current, s->device_format)) return RS_CHANGED;
    size = sizeof(current);
    status = property(s->tap, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, &size, &current);
    if (status != noErr) return backend(status, out_status);
    return same_format(current, s->format) ? RS_OK : RS_CHANGED;
}
