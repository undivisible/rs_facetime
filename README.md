# rs_facetime

> **Unstable — still in development.** APIs and behavior may change without notice.

FaceTime Audio private API bridge for macOS (separate from [`rs_imessage`](https://github.com/undivisible/rs_imessage)).

## Status

| Piece | State |
|-------|--------|
| Rust IPC client + FaceTime.app launcher | `private-api` feature |
| `rs-facetime-bridge-helper.dylib` | `./scripts/build-bridge.sh` → `lib/` |
| Owned PCM frames, bounded stream, capture/routing traits | Opt-in `audio` feature |
| Explicit process/device/stream output tap | Experimental `coreaudio-capture`, macOS 14.2+ |
| Audio sent into FaceTime | Caller-provided routing backend; no bundled virtual driver |

Protocol: v2 file-queue under `~/Library/Containers/com.apple.FaceTime/Data/.rs-facetime-rpc/`.

## Call-control requirements

- macOS 14+
- FaceTime.app
- **SIP disabled** for dylib injection
- Helper dylib built for your OS/arch (see matrix below)

The optional audio modules are independent of `private-api`: they do not inject
the helper, require SIP changes, or use its JSON/file queue. See [audio usage and
limitations](docs/audio.md). Default features remain empty. Audio capture and
FaceTime input routing have **not** been verified end-to-end with a live call.

## Optional PCM audio (0.2)

```toml
rs_facetime = { version = "0.2", default-features = false, features = ["audio"] }
```

`audio` supplies portable PCM types, bounded async/polling streams, and separate
capture and injection backend traits. Add `coreaudio-capture` for the provided
process-output adapter (Apple SDK 14.2+ to build, macOS 14.2+ to capture). Rust
1.85+ is required. No capture, permission prompt, device change, driver install,
or recording happens by enabling either feature.

Try the generated-audio example without accessing a device:

```bash
cargo run --example audio_synthetic --features audio
```

## Library

```toml
rs_facetime = { version = "0.1", default-features = false, features = ["private-api"] }
```

```rust
use rs_facetime::private_api::BridgeClient;

let bridge = BridgeClient::connect()?;
bridge.ping()?;
```

`RS_FACETIME_BRIDGE_DYLIB` overrides dylib search path.

## CLI

Requires the `cli` feature (includes `private-api`). Build the dylib first.

```bash
./scripts/build-bridge.sh
cargo build --features cli
./target/debug/rs_facetime sip
./target/debug/rs_facetime connect
./target/debug/rs_facetime ping --json
./target/debug/rs_facetime start-call "+15551234567"
./target/debug/rs_facetime status --json
./target/debug/rs_facetime end-call
```

| Command | Description |
|---------|-------------|
| `sip` | Show System Integrity Protection status |
| `ready` | Check bridge lock file (no launch) |
| `connect` | Inject dylib and launch FaceTime.app |
| `ping` | Bridge liveness |
| `status` | Active calls / process info |
| `start-call <handle>` | Place FaceTime Audio call |
| `end-call` | Disconnect active calls |
| `answer-call --call-uuid <uuid>` | Answer incoming call |
| `leave-call --call-uuid <uuid>` | Leave specific call |

Use `--json` on any command for one JSON object per line on stdout.

## Build

```bash
./scripts/build-bridge.sh
```

Source: `helper/RsFacetimeInjected.m` (arm64e default).

| Target | Notes |
|--------|--------|
| macOS 14 arm64 | Baseline |
| macOS 15+ arm64e | Match FaceTime.app slice |

## Related

- [rs_imessage](https://github.com/undivisible/rs_imessage) — iMessage (`private-api` uses imsg MIT dylib)

## License

ISC
