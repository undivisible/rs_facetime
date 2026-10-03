#ifndef RS_FACETIME_PROCESS_TAP_H
#define RS_FACETIME_PROCESS_TAP_H
#include <stdint.h>

typedef struct RSTap RSTap;
// Adapter errors have a separate domain from OSStatus (out_status).
enum { RS_OK, RS_OS, RS_USAGE, RS_TARGET, RS_FORMAT, RS_BACKEND, RS_MEMORY,
       RS_CHANGED, RS_PERMISSION, RS_CLEANUP };
typedef struct {
    uint64_t sequence;
    uint64_t host_time_ns;
    uint32_t samples;
    uint32_t has_host_time;
} RSFrameInfo;

int32_t rs_tap_available(void);
int32_t rs_tap_open(int32_t pid, const char *uid, uint32_t stream_index,
                    uint32_t capacity, uint32_t max_samples,
                    RSTap **out, uint32_t *rate, uint16_t *channels, int32_t *out_status);
// Single non-realtime consumer only. Copies into a caller-owned max_samples buffer.
int32_t rs_tap_read(RSTap *tap, float *samples, RSFrameInfo *info);
uint64_t rs_tap_dropped(RSTap *tap);
int32_t rs_tap_health(RSTap *tap, int32_t *out_status);
// Disables accepting new buffers, then quiesces callback before freeing state.
// On failure, reports RS_CLEANUP and the failing OSStatus;
// if callback removal fails, retains state rather than risk use-after-free.
int32_t rs_tap_close(RSTap *tap, int32_t *out_status);
#endif
