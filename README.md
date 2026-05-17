# rs_facetime

> **Unstable — still in development.** APIs and behavior may change without notice.

FaceTime Audio private API bridge for macOS (separate from [`rs_imessage`](https://github.com/undivisible/rs_imessage)).

## Status

| Piece | State |
|-------|--------|
| Rust IPC client + FaceTime.app launcher | `private-api` feature |
| `rs-facetime-bridge-helper.dylib` | `./scripts/build-bridge.sh` → `lib/` |

Protocol: v2 file-queue under `~/Library/Containers/com.apple.FaceTime/Data/.rs-facetime-rpc/`.

## Requirements

- macOS 14+
- FaceTime.app
- **SIP disabled** for dylib injection
- Helper dylib built for your OS/arch (see matrix below)

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
