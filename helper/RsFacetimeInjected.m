//
//  rs-facetime-bridge-helper.dylib — injected into FaceTime.app
//  v2 file-queue IPC (compatible with rs_facetime Rust client).
//
//  Protocol reference: openclaw/imsg (MIT). CallKit/TUCallCenter patterns
//  informed by jesec/imessage-rs (MIT) — original implementation here.
//

#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <objc/message.h>
#import <unistd.h>
#import <fcntl.h>

static NSString *kLockFile = nil;
static NSString *kRpcDir = nil;
static NSString *kRpcInDir = nil;
static NSString *kRpcOutDir = nil;
static NSString *kDebugLogFile = nil;
static NSTimer *rpcInboxTimer = nil;
static NSMutableSet *processedRpcIds = nil;
static int lockFd = -1;

static void initPaths(void) {
    if (kLockFile) return;
    NSString *root = NSHomeDirectory();
    kLockFile = [root stringByAppendingPathComponent:@".rs-facetime-bridge-ready"];
    kRpcDir = [root stringByAppendingPathComponent:@".rs-facetime-rpc"];
    kRpcInDir = [kRpcDir stringByAppendingPathComponent:@"in"];
    kRpcOutDir = [kRpcDir stringByAppendingPathComponent:@"out"];
    kDebugLogFile = [root stringByAppendingPathComponent:@".rs-facetime-bridge.log"];
    if (!processedRpcIds) processedRpcIds = [NSMutableSet set];
}

static void debugLog(NSString *fmt, ...) {
    initPaths();
    va_list args;
    va_start(args, fmt);
    NSString *msg = [[NSString alloc] initWithFormat:fmt arguments:args];
    va_end(args);
    NSString *line = [NSString stringWithFormat:@"%@ %@\n", [NSDate date], msg];
    FILE *fp = fopen(kDebugLogFile.UTF8String, "a");
    if (fp) { fputs(line.UTF8String, fp); fclose(fp); }
}

static id callCenter(void) {
    Class cls = NSClassFromString(@"TUCallCenter");
    if (!cls) return nil;
    if ([cls respondsToSelector:@selector(sharedInstance)]) {
        return ((id (*)(id, SEL))objc_msgSend)(cls, @selector(sharedInstance));
    }
    return nil;
}

static NSDictionary *okData(NSDictionary *data) {
    return @{@"success": @YES, @"data": data ?: @{}};
}

static NSDictionary *fail(NSString *msg) {
    return @{@"success": @NO, @"error": msg ?: @"error"};
}

static NSDictionary *successV2(NSString *uuid, NSDictionary *data) {
    return @{@"v": @2, @"id": uuid ?: @"", @"success": @YES, @"data": data ?: @{}};
}

static NSDictionary *errorV2(NSString *uuid, NSString *msg) {
    return @{@"v": @2, @"id": uuid ?: @"", @"success": @NO, @"error": msg ?: @"error"};
}

static void ensureDir(NSString *path) {
    [[NSFileManager defaultManager] createDirectoryAtPath:path
                             withIntermediateDirectories:YES
                                              attributes:@{NSFilePosixPermissions: @0700}
                                                   error:nil];
}

#pragma mark - Actions

static NSDictionary *handlePing(NSDictionary *params) {
    (void)params;
    return okData(@{@"pong": @YES, @"process": [[NSBundle mainBundle] bundleIdentifier] ?: @""});
}

static NSDictionary *handleStatus(NSDictionary *params) {
    (void)params;
    id center = callCenter();
    NSUInteger count = 0;
    if (center && [center respondsToSelector:@selector(currentCalls)]) {
        NSArray *calls = ((id (*)(id, SEL))objc_msgSend)(center, @selector(currentCalls));
        if ([calls isKindOfClass:[NSArray class]]) count = calls.count;
    }
    return okData(@{
        @"bundle": [[NSBundle mainBundle] bundleIdentifier] ?: @"",
        @"current_call_count": @(count),
    });
}

static NSDictionary *handleStartCall(NSDictionary *params) {
    NSString *handle = params[@"handle"];
    if (![handle isKindOfClass:[NSString class]] || handle.length == 0) {
        return fail(@"handle required");
    }

    NSString *escaped = [handle stringByAddingPercentEncodingWithAllowedCharacters:
        [NSCharacterSet URLPathAllowedCharacterSet]];
    NSURL *audioURL = [NSURL URLWithString:[NSString stringWithFormat:@"facetime-audio://%@", escaped]];
    if (audioURL) {
        [[NSWorkspace sharedWorkspace] openURL:audioURL];
        return okData(@{@"started_via": @"facetime-audio-url", @"handle": handle});
    }

    id center = callCenter();
    if (!center) return fail(@"TUCallCenter unavailable");

    Class reqClass = NSClassFromString(@"TUDialRequest");
    if (reqClass) {
        id request = [[reqClass alloc] init];
        SEL setHandle = NSSelectorFromString(@"setHandle:");
        SEL setDestination = NSSelectorFromString(@"setDestinationType:");
        if ([request respondsToSelector:setHandle]) {
            ((void (*)(id, SEL, id))objc_msgSend)(request, setHandle, handle);
        }
        if ([request respondsToSelector:setDestination]) {
            ((void (*)(id, SEL, long))objc_msgSend)(request, setDestination, 2L);
        }
        if ([center respondsToSelector:@selector(dialWithRequest:completion:)]) {
            ((void (*)(id, SEL, id, id))objc_msgSend)(center, @selector(dialWithRequest:completion:),
                request, ^(id call, NSError *err) {
                    debugLog(@"dial completion err=%@", err);
                });
            return okData(@{@"started_via": @"TUDialRequest", @"handle": handle});
        }
    }

    return fail(@"could not start call (no URL scheme or dialWithRequest)");
}

static NSDictionary *handleEndCall(NSDictionary *params) {
    (void)params;
    id center = callCenter();
    if (!center) return fail(@"TUCallCenter unavailable");
    if ([center respondsToSelector:@selector(disconnectAllCalls)]) {
        ((void (*)(id, SEL))objc_msgSend)(center, @selector(disconnectAllCalls));
        return okData(@{@"disconnected": @"all"});
    }
    if ([center respondsToSelector:@selector(currentCalls)]) {
        NSArray *calls = ((id (*)(id, SEL))objc_msgSend)(center, @selector(currentCalls));
        for (id call in calls) {
            if ([center respondsToSelector:@selector(disconnectCall:)]) {
                ((void (*)(id, SEL, id))objc_msgSend)(center, @selector(disconnectCall:), call);
            }
        }
        return okData(@{@"disconnected": @"current"});
    }
    return fail(@"no disconnect API");
}

static NSDictionary *handleAnswerCall(NSDictionary *params) {
    NSString *uuid = params[@"callUUID"] ?: params[@"call_uuid"];
    if (![uuid isKindOfClass:[NSString class]] || uuid.length == 0) {
        return fail(@"callUUID required");
    }
    id center = callCenter();
    if (!center || ![center respondsToSelector:@selector(callWithCallUUID:)]) {
        return fail(@"TUCallCenter unavailable");
    }
    NSUUID *nsuuid = [[NSUUID alloc] initWithUUIDString:uuid];
    id call = ((id (*)(id, SEL, id))objc_msgSend)(center, @selector(callWithCallUUID:), nsuuid);
    if (!call) return fail(@"call not found");
    if ([center respondsToSelector:@selector(answerOrJoinCall:)]) {
        ((void (*)(id, SEL, id))objc_msgSend)(center, @selector(answerOrJoinCall:), call);
        return okData(@{@"answered": uuid});
    }
    if ([center respondsToSelector:@selector(answerCall:)]) {
        ((void (*)(id, SEL, id))objc_msgSend)(center, @selector(answerCall:), call);
        return okData(@{@"answered": uuid});
    }
    return fail(@"answer API unavailable");
}

static NSDictionary *handleLeaveCall(NSDictionary *params) {
    NSString *uuid = params[@"callUUID"] ?: params[@"call_uuid"];
    id center = callCenter();
    if (!center) return fail(@"TUCallCenter unavailable");
    if ([uuid isKindOfClass:[NSString class]] && uuid.length > 0) {
        NSUUID *nsuuid = [[NSUUID alloc] initWithUUIDString:uuid];
        id call = ((id (*)(id, SEL, id))objc_msgSend)(center, @selector(callWithCallUUID:), nsuuid);
        if (!call) return fail(@"call not found");
        if ([center respondsToSelector:@selector(disconnectCall:)]) {
            ((void (*)(id, SEL, id))objc_msgSend)(center, @selector(disconnectCall:), call);
            return okData(@{@"left": uuid});
        }
    }
    return handleEndCall(params);
}

static NSDictionary *dispatchAction(NSString *action, NSDictionary *params) {
    if ([action isEqualToString:@"ping"]) return handlePing(params);
    if ([action isEqualToString:@"status"]) return handleStatus(params);
    if ([action isEqualToString:@"start-call"]) return handleStartCall(params);
    if ([action isEqualToString:@"end-call"]) return handleEndCall(params);
    if ([action isEqualToString:@"answer-call"]) return handleAnswerCall(params);
    if ([action isEqualToString:@"leave-call"]) return handleLeaveCall(params);
    return fail([NSString stringWithFormat:@"unknown action: %@", action]);
}

#pragma mark - v2 IPC

static NSDictionary *processEnvelope(NSDictionary *envelope) {
    NSString *uuid = [envelope[@"id"] isKindOfClass:[NSString class]] ? envelope[@"id"] : @"";
    NSString *action = envelope[@"action"];
    NSDictionary *params = [envelope[@"params"] isKindOfClass:[NSDictionary class]]
        ? envelope[@"params"] : @{};
    if (![action isKindOfClass:[NSString class]] || action.length == 0) {
        return errorV2(uuid, @"missing action");
    }
    NSDictionary *legacy = dispatchAction(action, params);
    if (![legacy[@"success"] boolValue]) {
        return errorV2(uuid, legacy[@"error"] ?: @"error");
    }
    return successV2(uuid, legacy[@"data"]);
}

static void processInboxFile(NSString *uuid) {
    @autoreleasepool {
        initPaths();
        if ([processedRpcIds containsObject:uuid]) return;
        [processedRpcIds addObject:uuid];

        NSString *inPath = [kRpcInDir stringByAppendingPathComponent:
            [uuid stringByAppendingPathExtension:@"json"]];
        NSString *outPath = [kRpcOutDir stringByAppendingPathComponent:
            [uuid stringByAppendingPathExtension:@"json"]];

        NSData *body = [NSData dataWithContentsOfFile:inPath];
        if (!body.length) {
            [[NSFileManager defaultManager] removeItemAtPath:inPath error:nil];
            return;
        }

        NSDictionary *envelope = [NSJSONSerialization JSONObjectWithData:body options:0 error:nil];
        NSDictionary *response = [envelope isKindOfClass:[NSDictionary class]]
            ? processEnvelope(envelope)
            : errorV2(uuid, @"invalid json");

        NSData *outData = [NSJSONSerialization dataWithJSONObject:response options:0 error:nil];
        if (outData) {
            NSString *tmp = [outPath stringByAppendingString:@".tmp"];
            [outData writeToFile:tmp atomically:YES];
            rename(tmp.UTF8String, outPath.UTF8String);
        }
        [[NSFileManager defaultManager] removeItemAtPath:inPath error:nil];
        if (processedRpcIds.count > 512) [processedRpcIds removeAllObjects];
    }
}

static void scanInbox(void) {
    @autoreleasepool {
        initPaths();
        NSArray *entries = [[NSFileManager defaultManager] contentsOfDirectoryAtPath:kRpcInDir error:nil];
        for (NSString *name in entries) {
            if (![name hasSuffix:@".json"]) continue;
            processInboxFile([name stringByDeletingPathExtension]);
        }
    }
}

static void publishReadyLock(void) {
    initPaths();
    lockFd = open(kLockFile.UTF8String, O_CREAT | O_WRONLY, 0644);
    if (lockFd >= 0) {
        NSString *pid = [NSString stringWithFormat:@"%d\n", getpid()];
        write(lockFd, pid.UTF8String, pid.length);
    }
    debugLog(@"ready lock at %@", kLockFile);
}

static void startV2Watcher(void) {
    initPaths();
    ensureDir(kRpcDir);
    ensureDir(kRpcInDir);
    ensureDir(kRpcOutDir);
    rpcInboxTimer = [NSTimer timerWithTimeInterval:0.1 repeats:YES block:^(__unused NSTimer *t) {
        scanInbox();
    }];
    [[NSRunLoop mainRunLoop] addTimer:rpcInboxTimer forMode:NSRunLoopCommonModes];
}

__attribute__((constructor))
static void injectedInit(void) {
    NSString *bundle = [[NSBundle mainBundle] bundleIdentifier];
    debugLog(@"injected into %@", bundle);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
        publishReadyLock();
        startV2Watcher();
    });
}

__attribute__((destructor))
static void injectedCleanup(void) {
    [rpcInboxTimer invalidate];
    rpcInboxTimer = nil;
    initPaths();
    if (lockFd >= 0) { close(lockFd); lockFd = -1; }
    [[NSFileManager defaultManager] removeItemAtPath:kLockFile error:nil];
}
