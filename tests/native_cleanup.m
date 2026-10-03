// Deterministic failure injection for the real cleanup implementation. Every
// HAL teardown function is replaced; this executable never accesses a device.
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#include <assert.h>
#include <stdio.h>
static int calls[4], count;
static OSStatus stop_status, remove_status, aggregate_status, tap_status;
static OSStatus fake_stop(AudioObjectID device, AudioDeviceIOProcID io) {
    (void)device; (void)io; calls[count++] = 1; return stop_status;
}
static OSStatus fake_remove(AudioObjectID device, AudioDeviceIOProcID io) {
    (void)device; (void)io; calls[count++] = 2; return remove_status;
}
static OSStatus fake_aggregate(AudioObjectID device) {
    (void)device; calls[count++] = 3; return aggregate_status;
}
static OSStatus fake_tap(AudioObjectID tap) {
    (void)tap; calls[count++] = 4; return tap_status;
}
#define AudioDeviceStop fake_stop
#define AudioDeviceDestroyIOProcID fake_remove
#define AudioHardwareDestroyAggregateDevice fake_aggregate
#define AudioHardwareDestroyProcessTap fake_tap
#include "../native/process_tap.m"

static RSTap *session(void) {
    RSTap *s = calloc(1, sizeof(RSTap));
    assert(s);
    s->aggregate = 1; s->tap = 2; s->io = receive_audio;
    atomic_init(&s->accepting, true);
    return s;
}
int main(void) {
    int32_t status = 0;
    assert(rs_tap_close(NULL, &status) == RS_OK);
    assert(rs_tap_close(session(), &status) == RS_OK);
    assert(count == 4 && calls[0] == 1 && calls[1] == 2 && calls[2] == 3 && calls[3] == 4);
    count = 0; stop_status = 42;
    assert(rs_tap_close(session(), &status) == RS_CLEANUP && status == 42);
    assert(count == 4); // A stop failure must not skip removal or resource cleanup.
    count = 0; stop_status = 0; remove_status = 43;
    RSTap *retained = session();
    assert(rs_tap_close(retained, &status) == RS_CLEANUP && status == 43);
    assert(count == 2 && retained->aggregate == 1); // State retained for possible callbacks.
    assert(!atomic_load(&retained->accepting));
    free(retained); // Safe only in this test: no callback was ever installed.
    count = 0; remove_status = 0; aggregate_status = 44; tap_status = 45;
    assert(rs_tap_close(session(), &status) == RS_CLEANUP && status == 44);
    assert(count == 4); // Tap cleanup still attempted after aggregate failure.
    count = 0; aggregate_status = tap_status = 0;
    RSTap *partial = session(); partial->io = NULL;
    assert(rs_tap_close(partial, &status) == RS_OK);
    assert(count == 2 && calls[0] == 3 && calls[1] == 4);
    count = 0; status = 47;
    assert(failed_open(session(), RS_BACKEND, &status) == RS_BACKEND && status == 47);
    assert(count == 4); // Preserve the setup error only when cleanup succeeds.
    count = 0; remove_status = 48;
    retained = session();
    assert(failed_open(retained, RS_FORMAT, &status) == RS_CLEANUP && status == 48);
    assert(count == 2 && !atomic_load(&retained->accepting));
    free(retained); // Mock callback was never installed.
    puts("native cleanup: synthetic teardown ordering and failure checks passed");
    return 0;
}
