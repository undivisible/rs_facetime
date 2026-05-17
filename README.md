# rs_facetime

FaceTime Audio private API bridge Rust crate for macOS.

**License:** [Mozilla Public License 2.0](LICENSE)

## Status

| Piece | State |
|-------|--------|
| Rust IPC client + FaceTime.app launcher | Implemented (`private-api` feature) |
| `rs-facetime-bridge-helper.dylib` | Built in-repo: `./scripts/build-bridge.sh` → `lib/` |

Protocol mirrors the v2 file-queue model used by
[openclaw/imsg](https://github.com/openclaw/imsg) (MIT), with FaceTime-specific
paths under `~/Library/Containers/com.apple.FaceTime/Data/`.

## Requirements

- macOS 14+ (15/26 may need arch-matched dylibs — see matrix below)
- FaceTime.app available
- **SIP disabled** (`csrutil disable` in Recovery)
- Helper dylib built for your OS + CPU slice

## Library

```toml
rs_facetime = { git = "https://github.com/undivisible/rs_facetime", default-features = false, features = ["private-api"] }
```

```rust
use rs_facetime::private_api::BridgeClient;

let bridge = BridgeClient::connect()?;
bridge.ping()?;
// bridge.start_call("+15551234567")?;  // once dylib implements start-call
```

`RS_FACETIME_BRIDGE_DYLIB` overrides dylib search path.

## Dylib build matrix

Ship **one dylib per (macOS major, architecture)**. arm64e Macs often need a
dylib linked with the same SDK as the host FaceTime binary.

| Target | Notes |
|--------|--------|
| macOS 14 arm64 | Baseline for Apple Silicon dev kits |
| macOS 15 arm64 / arm64e | Test on Sequoia before production |
| macOS 26 arm64e | Rebuild when Apple bumps TAPI / IMCore private symbols |

Reference implementations (study only, do not copy GPL server code):

- [jesec/imessage-rs](https://github.com/jesec/imessage-rs) — FaceTime injection notes
- Hosted APIs (e.g. Blooio) document FaceTime Audio as a separate surface

Build the helper (arm64e, macOS 14+):

```bash
./scripts/build-bridge.sh
# → lib/rs-facetime-bridge-helper.dylib
```

Source: `helper/RsFacetimeInjected.m` (v2 file-queue IPC + TUCallCenter / `facetime-audio://`).

## Related

- **[rs_imsg](https://github.com/undivisible/rs_imsg)** — iMessage (`private-api` uses imsg MIT dylib)
