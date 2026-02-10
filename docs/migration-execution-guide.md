# Privchat SDK Migration Execution Guide

## 1. Target and Non-Negotiables

- New baseline: `privchat-rust` (`privchat-sdk` + `privchat-sdk-ffi`) only.
- FFI architecture: UniFFI object + async exports only.
- Local-first gate: no local data path before `run_bootstrap_sync()`.
- Local primary key: `message.id` only.
- Server dedupe key: `local_message_id` only (Snowflake generated), never used as local CRUD primary key.
- Storage isolation: per-user directories must remain independent and non-interfering.

## 2. Legacy Flow Compatibility (Must Keep)

Required runtime order:

1. `connect()`
2. `login()` or `register()`
3. `authenticate()`
4. `run_bootstrap_sync()`
5. local-first reads/writes and outbound queues

Enforcement:

- Any local-first API call before bootstrap completion returns `InvalidState("run_bootstrap_sync required before local-first operations")`.

## 3. Database and Encryption Migration

## 3.1 Stack Standard

- `rusqlite` + SQLCipher (bundled)
- `refinery` for migration ordering and version tracking
- `sled` for KV side storage where applicable

## 3.2 Migration File Convention (legacy-compatible)

- Naming: `V年月日时分秒__<name>.sql`
- Init default: `V20241119070909__init.sql`
- Ordering: timestamp lexical order
- Version source of truth: refinery schema history table

## 3.3 Optimization Rules

- DB access actorized (single-writer model via StorageActor/DbActor queue)
- synchronous `rusqlite` execution inside actor, async boundary outside
- no shared mutable DB handles crossing threads
- no lock guard across await

## 4. User-Isolated Storage Layout

Each user gets independent roots:

- database files
- queue files (normal + file queues)
- kv store
- media assets (images/audio/video/files)

Optimization:

- deterministic path derivation by uid
- no cross-user fallback reads
- session switch flushes in-memory context, not global stores

## 5. Queue Model Migration

## 5.1 Normal Messages

- one normal outbound queue per user
- enqueue requires existing `message.id`
- enqueue step assigns/updates `local_message_id` (Snowflake)

## 5.2 File Messages

- multiple file queues per user (route-key selected)
- queue selection policy deterministic and stable

## 5.3 Semantics

- local retries keyed by `message.id`
- server dedupe keyed by `local_message_id`
- ack and delivery state transitions recorded in local store

## 6. API Migration Strategy

## 6.1 Core APIs (already prioritized)

- session/connection/auth/bootstrap/shutdown
- sync (entities/channel/all)
- presence/typing
- local storage APIs (message/channel/user/group/reaction/mention/reminder)
- outbound queues and file queues

## 6.2 Remote Capability Migration

Use unified `rpc_call(route, body_json)` as base, then expose typed/high-frequency wrappers:

- friend/group/channel/message/device/profile/privacy/qrcode/file/sync

Optimization:

- avoid introducing extra concurrency model per wrapper
- all wrappers reuse actor + single transport path

## 7. Error Code Standardization

- protocol code type unified as `u32`
- map internal SDK errors to protocol error domains consistently
- never return ad-hoc string-only errors at FFI boundary

## 8. Observability and Gate Scripts

Required logs per step:

- create/connect/login/auth/bootstrap enter/exit
- queue enqueue/dequeue/ack
- rpc route + timeout/failure category

Mandatory gate after each batch:

1. `cargo test -p privchat-sdk -p privchat-sdk-ffi`
2. iOS auto repro script:
   - build
   - run simulator app (iPhone 17 Pro)
   - auto-login trigger
   - crash/log/stack capture

## 9. Migration Execution Phases

## Phase A (foundation)

- complete actor model + runtime ownership cleanup
- keep local-first gate strict
- keep old flow order strict

## Phase B (storage parity)

- full schema parity and SQLCipher behavior parity
- per-user directory parity
- queue parity (normal + multi-file)

## Phase C (API parity)

- map old public APIs to new FFI wrappers or explicit rpc bridge
- remove obsolete sync helper/by-handle leftovers

## Phase D (quality hardening)

- cancellation/shutdown semantics
- idempotency/reentrancy rules
- event stream contract (sequence/replay/backpressure)

## 10. Acceptance Criteria (Done Definition)

- old SDK critical flow is behavior-compatible
- no sync helper/by-handle/snapshot legacy bridge in FFI
- DB migration and encryption behavior verified
- queue semantics verified (message.id local primary; local_message_id server dedupe)
- iOS auto repro stable across repeated runs
