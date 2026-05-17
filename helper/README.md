# FaceTime bridge dylib

`RsFacetimeInjected.m` implements v2 file-queue IPC under the FaceTime.app container
(`.rs-facetime-rpc/`, `.rs-facetime-bridge-ready`).

Design references (not copied verbatim):

- [openclaw/imsg](https://github.com/openclaw/imsg) — v2 inbox/outbox IPC (MIT)
- [jesec/imessage-rs](https://github.com/jesec/imessage-rs) — `TUCallCenter` usage (MIT)
