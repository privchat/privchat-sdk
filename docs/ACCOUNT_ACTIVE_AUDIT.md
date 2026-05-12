# ACCOUNT_ACTIVE_AUDIT — `Account.isActive` Persistence Root Cause

R8.7-audit. **Read-only investigation.** No Rust / Kotlin SDK / App
changes; no migrations; no semantic redesign committed yet. Output is
this document, finalized with a recommended fix path.

---

## §0. Scope / Non-goals

### In scope

- Trace every write path that affects the source of `Account.isActive`
  in the Rust SDK
- Trace the read path from Rust → UniFFI → Kotlin SDK → App
- Identify what triggers `isActive=false` on App boot even though
  tokens persisted, forcing R8.6c-era App-side fallbacks
- Recommend a fix path, weighed against user's audit-brief options

### Non-goals (will NOT touch)

- Rust SDK source (no edits, no schema migration, no semantic changes)
- Kotlin SDK source
- App-side `PrivChatApp.kt:543` / `PrivChatSDKManager.kt:346` fallbacks
- UniFFI bindings regen
- Any hot-fix while audit is in flight

---

## §1. Current symptom (from R8.6c notes)

Two App-side fallbacks were added during R8.6c to work around the
SDK persistence:

1. **`PrivChatApp.kt:543`** — boot-time uid picker:
   ```kotlin
   val uid = accounts.firstOrNull { it.isActive }?.uid
       ?: accounts.firstOrNull()?.uid  // <-- fallback added in R8.6c
   ```
2. **`PrivChatSDKManager.kt:346`** — `hasActiveLocalAccount()` changed
   from `.any { it.isActive }` → `.isNotEmpty()`.

Observed symptom (per R8.6c carryover note): "every restart needs SMS
re-login because `accounts.firstOrNull { it.isActive }` returned null
even though tokens were on disk". The token-refresh handler also
skipped with `cause=no_local_account`, leaving IM stuck at
Connected and search/messaging RPCs failing.

After fallbacks: `recovered ok → AUTHENTICATED → SYNC_READY`. Tokens
were on disk; only the `isActive` boolean was wrong.

---

## §2. Persistence schema

Storage backend: **sled** (embedded key/value, NOT SQLite).

Two parallel "current active account" stores exist:

### §2.1 sled `accounts` tree

`privchat-sdk/crates/privchat-sdk/src/local_store.rs`:

```rust
const GLOBAL_TREE_ACCOUNTS: &str = "accounts";  // line 52
const K_ACTIVE_UID: &[u8] = b"active_uid";       // line 64
```

The `accounts` tree contains:

| Key shape | Value | Meaning |
|-----------|-------|---------|
| `b"active_uid"` (K_ACTIVE_UID) | uid bytes | Currently-active uid pointer |
| `b"schema_version"` (K_SCHEMA_VERSION) | i32 BE | Storage schema version |
| `b"acct/<uid>/created_at"` | i64 BE millis | Per-account: created timestamp |
| `b"acct/<uid>/last_login_at"` | i64 BE millis | Per-account: most recent login |

**There is no per-account `is_active` field stored.** `is_active` is
*computed* at read time.

### §2.2 Plain file `current_user_file`

`local_store.rs:3778–3782` and `3784–3797`:

```rust
pub fn save_current_uid(&self, uid: &str) -> Result<()> {
    std::fs::write(self.current_user_file(), uid.as_bytes())?;
    Ok(())
}

pub fn load_current_uid(&self) -> Result<Option<String>> {
    let path = self.current_user_file();
    if !path.exists() { return Ok(None); }
    ...
}
```

A plain on-disk file (path resolved by `current_user_file()`)
containing only the uid string. Used by the SDK boot path
(`storage.load_current_uid()`) to pick which user's account dir to
mount.

### §2.3 Two stores, one logical pointer

The codebase has **two parallel "current active uid" stores**. Neither
is wrong on its own; the issue is that they're not write-mirrored by
every code path that should keep them in sync.

---

## §3. Write paths

### §3.1 `save_login` (`local_store.rs:470–564`)

```rust
pub fn save_login(&self, uid: &str, login: &LoginResult) -> Result<()> {
    ...
    let accounts = global_db.open_tree(GLOBAL_TREE_ACCOUNTS)?;
    accounts.insert(K_SCHEMA_VERSION, ...)?;
    accounts.insert(K_ACTIVE_UID, uid.as_bytes())?;   // <-- WRITES sled
    accounts.insert(k_acct_created_at(uid), ...)?;     // if new
    accounts.insert(k_acct_last_login(uid), ...)?;
    self.clear_legacy_session(uid)?;
    self.save_current_uid(uid)?;                       // <-- WRITES file
    Ok(())
}
```

**Co-writes BOTH** `K_ACTIVE_UID` (sled) AND `current_user_file`
(file). Caller (in `lib.rs:5807-5808` for BUILTIN login,
`lib.rs:5979` for external/PLATFORM first-time auth) follows up with
`flush_user(uid)` which `flush()`es both `account_db` and `global_db`
— sled durability is guaranteed before login returns success.

### §3.2 `set_current_uid` (`lib.rs:11213-11239` actor handler)

```rust
Command::SetCurrentUid { uid, resp } => {
    let result = match state.storage.list_local_accounts().await {
        Ok((_, entries)) => {
            if !entries.iter().any(|entry| entry.uid == uid) {
                Err(Error::InvalidState(...))
            } else {
                let saved = state.storage.save_current_uid(uid.clone()).await;
                //                       ^^^^^^^^^^^^^^^^^ writes FILE only
                let loaded = state.storage.load_session(uid.clone()).await;
                ...
                state.current_uid = Some(uid);
                ...
            }
        }
        ...
    };
}
```

**Writes only `current_user_file`, NOT `K_ACTIVE_UID`.** This is the
key divergence point.

### §3.3 `authenticate` (`lib.rs:5960–5990`)

```rust
let existing_session = self.storage.load_session(uid.clone()).await.ok().flatten();
if existing_session.is_some() {
    self.storage.update_access_token(uid.clone(), token_for_persist, None).await?;
    // 注释明确: update_access_token 不写 K_CUR_UID
    self.storage.save_current_uid(uid.clone()).await?;  // <-- FILE only
} else {
    self.storage.save_login(uid.clone(), LoginResult { ... }).await?;  // <-- BOTH
}
self.storage.flush_user(uid.clone()).await?;
```

External-auth (PLATFORM SMS, QR) re-handshake path. When a session
already exists for the uid (subsequent boots / token refreshes), only
the file is updated; `K_ACTIVE_UID` is NOT touched.

### §3.4 `update_access_token` (`local_store.rs:630–652`)

Writes only the account-specific `auth` tree (encrypted access_token
+ optional expires_at). Touches neither `K_ACTIVE_UID` nor the file.

### §3.5 Write-matrix summary

| Operation | K_ACTIVE_UID (sled) | current_user_file | Common trigger |
|-----------|:-------------------:|:-----------------:|----------------|
| `save_login` | **write** | **write** | First-time login |
| `authenticate` (no existing session) | **write** | **write** | External-auth first time |
| `authenticate` (existing session) | — | **write** | Re-handshake on boot |
| `set_current_uid` | — | **write** | App's switch-account / boot picker |
| `update_access_token` | — | — | Token refresh |
| `clear_session` | **clear** | — | Explicit logout (ClearLocalState only) |
| `wipe_user_full` | **clear** | **clear** | WipeCurrentUserFull only |
| `clear_current_uid` | — | **clear** | Internal helper |

Lookup matrix (read path):

| API | Reads | Computed `is_active` |
|-----|-------|----------------------|
| `list_local_accounts` | **K_ACTIVE_UID** | `is_active = (K_ACTIVE_UID == entry.uid)` |
| `load_current_uid` | **current_user_file** | n/a |

**The asymmetry**: writes go through both stores OR only one; reads
go through whichever store the API in question chose. Specifically
`is_active` is computed exclusively from `K_ACTIVE_UID`, while the
SDK's own boot-time "which user to mount" decision uses
`current_user_file`.

---

## §4. Read / list path

`local_store.list_local_accounts()` (`local_store.rs:914-964`):

```rust
pub fn list_local_accounts(&self) -> Result<(Option<String>, Vec<LocalAccountEntry>)> {
    let accounts = global_db.open_tree(GLOBAL_TREE_ACCOUNTS)?;
    let active_uid = get_string(&accounts, K_ACTIVE_UID)?;  // single-pointer read
    // ... iterate acct/<uid>/last_login_at keys
    Ok((active_uid, out))
}
```

`LocalAccountEntry` itself contains only `uid / created_at /
last_login_at` — **no `is_active` field**.

The actor wrapper in `lib.rs:11194-11211` computes `is_active`:

```rust
Command::ListLocalAccounts { resp } => {
    let result = state.storage.list_local_accounts().await.map(
        |(active_uid, entries)| {
            entries.into_iter().map(|entry| LocalAccountSummary {
                is_active: active_uid.as_ref()
                    .map(|uid| uid == &entry.uid)
                    .unwrap_or(false),  // <-- if K_ACTIVE_UID None → all false
                uid: entry.uid,
                created_at: entry.created_at,
                last_login_at: entry.last_login_at,
            }).collect()
        },
    );
    let _ = resp.send(result);
}
```

The actor wrapper returns `Vec<LocalAccountSummary>` with `is_active`
computed from `K_ACTIVE_UID`. If `K_ACTIVE_UID` is `None` OR doesn't
match any entry, **every entry gets `is_active=false`** — matching the
App-observed symptom.

---

## §5. FFI / Kotlin mapping

### §5.1 Rust FFI

`privchat-sdk-ffi/src/lib.rs:1728-1731`:

```rust
pub struct LocalAccountSummary {
    pub uid: String,
    pub created_at: i64,
    pub last_login_at: i64,
    pub is_active: bool,
}
```

`map_local_account_summary` (`ffi/src/lib.rs:2997-3004`):

```rust
fn map_local_account_summary(v: SdkLocalAccountSummary) -> LocalAccountSummary {
    LocalAccountSummary {
        uid: v.uid,
        created_at: v.created_at,
        last_login_at: v.last_login_at,
        is_active: v.is_active,
    }
}
```

**1:1 pass-through.** `is_active` is forwarded verbatim from the actor
wrapper.

### §5.2 UniFFI Kotlin binding

Auto-generated `LocalAccountSummary` data class with `isActive: Boolean`
field. Mapping is automatic via UniFFI procmacros; field names
`is_active` (Rust) → `isActive` (Kotlin) by standard snake→camel rule.

**No bug in FFI layer.** The FFI faithfully reflects whatever the
actor wrapper returns.

### §5.3 Kotlin SDK / App read

The App calls `PrivChatSDKManager.listLocalAccounts()` which forwards
to the FFI `Client.list_local_accounts()`. Each `LocalAccountSummary`
flows through unchanged. `.isActive` on the App side equals whatever
`active_uid == entry.uid` evaluated to at the actor level.

---

## §6. Lifecycle hooks — does anything proactively clear active?

Exhaustive search of all writes to `K_ACTIVE_UID`:

- **`accounts.insert(K_ACTIVE_UID, ...)`** at `local_store.rs:544` —
  inside `save_login`
- **`accounts.remove(K_ACTIVE_UID)`** at `local_store.rs:617` — inside
  `clear_session`
- **`accounts.remove(K_ACTIVE_UID)`** at `local_store.rs:3831` —
  inside `wipe_user_full`

`clear_session` is only called from `Command::ClearLocalState`
(`lib.rs:10001`). `Command::ClearLocalState` is exposed to the App as
`clearLocalState()` — **the App-side codebase has zero call sites**
(grepped). `wipe_user_full` is only called from
`Command::WipeCurrentUserFull` — same story.

**`fire_forced_logout` does NOT touch `K_ACTIVE_UID`**
(`lib.rs:790-839`). It sets state flags, disconnects, emits a
ForcedLogout event, but leaves storage alone.

**There is no shutdown / background / process-bg handler that clears
`K_ACTIVE_UID`.** The Rust SDK is not actively wiping the flag.

---

## §7. Root cause

The audit's framing distinguishes two questions:

### §7.1 "Why does the App see `isActive=false` despite tokens on disk?"

Going through hypotheses A–G from the audit brief:

| # | Hypothesis | Verdict |
|---|------------|---------|
| A | DB has no `is_active` field | ❌ **Partial truth.** No per-account boolean. is_active is computed from K_ACTIVE_UID singleton at read time. |
| B | DB has field, login didn't write `true` | ❌ `save_login` writes `K_ACTIVE_UID` AND flushes |
| C | Wrote `true`, list didn't map | ❌ Actor wrapper computation is correct |
| D | FFI/Kotlin maps to default `false` | ❌ FFI 1:1 pass-through verified |
| E | Shutdown/bg actively clears active | ❌ No such handler exists |
| F | Active is runtime-only, not persisted | ❌ **Partial truth.** It IS persisted (K_ACTIVE_UID and file), but write paths are inconsistent |
| G | Active is session-level, not account-level | ❌ Storage is per-account-pointer, not session |

**Reality** is closer to **hypothesis F**: the active pointer IS
persisted, but there are **two parallel stores** (`K_ACTIVE_UID` in
sled, `current_user_file` on disk) and not every write path updates
both. Specifically:

- **`set_current_uid` writes only the file, not `K_ACTIVE_UID`**
- This means: after ANY `setCurrentUid(uid)` call to a uid that is
  not the most recently `save_login`'d uid, `K_ACTIVE_UID` becomes
  stale relative to the actual current account.
- `is_active` is computed from `K_ACTIVE_UID`, so the list reports
  the wrong account as active (or none, if `K_ACTIVE_UID` was never
  written).

### §7.2 "Why does it manifest as 'every restart needs SMS re-login'?"

This is the part that doesn't fully reduce to the §7.1 design issue
in single-account scenarios — single-account flow should keep
`K_ACTIVE_UID` correctly pointing at the lone uid. **Possible
explanations**:

1. **Multi-account experimentation during R8.6c development**: the
   user may have logged in as A, switched to B via `setCurrentUid`,
   then on restart the boot path reads file=B but K_ACTIVE_UID=A,
   list reports A as active. App selects A via fallback but tokens
   for the displayed "current" account get mismatched.
2. **Stale dev state on the test device**: an aborted login,
   migrated session, or partial wipe between dev builds could leave
   `K_ACTIVE_UID=None` while account entries (last_login_at) exist.
3. **A path I did not find**: low-likelihood but acknowledged — the
   audit could not reproduce the symptom by reading code alone; it
   only confirmed the architectural pre-condition for the symptom
   (the dual-store divergence) is real.

In all three cases, the design fix is the same: **eliminate the
dual-store**.

---

## §8. Fix options

The audit-brief's three options map onto the actual codebase as:

### §8.1 Option A (≈ brief's Option 1): per-account `is_active` boolean stored

Add an `is_active: bool` column / sled key per account; flip it on
every login / switch / logout.

- **Pros**: explicit; "active" is now an account-state, not a singleton
- **Cons**: requires migration (existing data has no field); multi-account
  flip-flop logic must touch N entries on every switch; introduces a
  new invariant (only one entry should have `is_active=true` at a
  time) that any future writer must respect

**Audit weight: low.** Replaces a singleton-pointer design with N-entry
boolean tracking; harder to keep consistent, not easier.

### §8.2 Option B (≈ brief's Option 2): keep single active pointer, consolidate to one store

The codebase already has the singleton-pointer pattern; the bug is
that two parallel pointers (`K_ACTIVE_UID` sled key + `current_user_file`
plain file) coexist and don't always co-write.

**Sub-option B.1**: Use `current_user_file` as single source of truth
- `list_local_accounts` reads `load_current_uid()` instead of
  `K_ACTIVE_UID`
- All "active" reads go through the file
- `K_ACTIVE_UID` can be deleted (or left as deprecated, harmless)
- `save_login` keeps writing both (or stops writing K_ACTIVE_UID)
- `set_current_uid` already writes the file — already correct
- **Migration**: zero. File-based pointer already exists for every
  account that's been current at any point. Existing K_ACTIVE_UID
  becomes ignored.

**Sub-option B.2**: Use `K_ACTIVE_UID` as single source of truth
- Make `set_current_uid` ALSO write `K_ACTIVE_UID`
- Make `authenticate` (existing session path) also write `K_ACTIVE_UID`
- Delete `current_user_file` (or keep as fast-boot cache that's
  re-derived)
- **Migration**: read the file once on first boot, write to
  `K_ACTIVE_UID`, then deprecate the file

### §8.3 Option C (= brief's Option 3): keep App-side fallback

Do nothing in the SDK. The App-side fallbacks at
`PrivChatApp.kt:543` + `PrivChatSDKManager.kt:346` already mask the
symptom.

- **Pros**: zero SDK change
- **Cons**: the dual-store divergence remains a real bug for future
  multi-account features, admin tools, or any other consumer of
  `is_active`. Future SDK consumers (TS SDK?) won't have the App's
  fallback knowledge.

---

## §9. Recommended path

**Recommend Option B.1** — make `current_user_file` the single source
of truth for "active uid", and let `list_local_accounts` derive
`is_active` from it.

Reasoning:

1. **Matches user's audit-brief lean** ("我倾向 audit 后优先考虑
   Option 2") — single persisted active pointer, R7-style.
2. **Zero migration**: the file is already written by every active
   write path (`save_login`, `authenticate`, `set_current_uid`).
   Switching `list_local_accounts` to read it changes only the read
   side.
3. **`set_current_uid` is already correct** under this scheme — no new
   write needed there.
4. **K_ACTIVE_UID can be left in place** (harmless dead code) for the
   first ship; a follow-up cleanup PR can remove it. This makes the
   fix a 1-function change in `list_local_accounts` + actor wrapper.
5. **No multi-account semantic invariant to maintain** (unlike Option
   A's per-account booleans).

Concrete sketch (NOT executed in this audit round):

```rust
// In local_store.list_local_accounts (or the actor wrapper):
let active_uid = self.load_current_uid()?;   // was: get K_ACTIVE_UID from sled
let mut out = Vec::new();
// ... iterate accounts tree (unchanged) ...
Ok((active_uid, out))
```

After the fix lands, the App-side fallbacks in `PrivChatApp.kt:543`
and `PrivChatSDKManager.kt:346` can be reverted (R8.7 close-out
step).

---

## §10. Migration / compatibility risk

For **recommended Option B.1**:

| Risk | Mitigation |
|------|------------|
| Existing K_ACTIVE_UID readers expect it to be authoritative | Only one caller exists (`list_local_accounts`); changing it is the fix |
| `current_user_file` could be missing for legitimately-logged-in users on old installs | Only possible if there's an account but no `save_login` ever fired for it; not reproducible from existing code paths |
| Multi-account future: file is a single string | Same constraint K_ACTIVE_UID has today — one active uid at a time. Out-of-scope for R8.7 |
| Concurrent `save_login` + `list_local_accounts` race | File reads/writes are atomic; existing race surface unchanged |

For **NOT-recommended Option B.2** (sled as single source):

| Risk | Mitigation |
|------|------------|
| Existing installs have file=X but K_ACTIVE_UID=None | Boot migration: if K_ACTIVE_UID missing, read file, write K_ACTIVE_UID |
| set_current_uid must now flush sled (slower than file) | Acceptable (sled flush ~ms) |
| Multiple consumers need updating (`set_current_uid`, `authenticate`, etc.) | More patch surface than B.1 |

For **Option A** (per-account boolean):

| Risk | Mitigation |
|------|------------|
| Sled migration: add per-account `is_active` key | Lazy migration on first read; or one-shot on schema bump |
| Switching accounts must update N entries atomically | sled `Batch` |
| Multi-active state (corruption) possible | Add invariant validator on read |

---

## §11. References

- Schema constants: `privchat-sdk/crates/privchat-sdk/src/local_store.rs:46-64`
- `save_login`: `local_store.rs:470-564`
- `clear_session`: `local_store.rs:595-626`
- `list_local_accounts` (storage): `local_store.rs:914-964`
- `save_current_uid` / `load_current_uid` / `clear_current_uid`:
  `local_store.rs:3778-3806`
- `wipe_user_full`: `local_store.rs:3809-3847`
- `flush_user`: `local_store.rs:966-974`
- Actor `ListLocalAccounts` handler: `privchat-sdk/src/lib.rs:11194-11211`
- Actor `SetCurrentUid` handler: `privchat-sdk/src/lib.rs:11213-11239`
- Actor `ClearLocalState` handler: `privchat-sdk/src/lib.rs:9994-10020`
- `authenticate` (external-auth): `privchat-sdk/src/lib.rs:5960-5990`
- BUILTIN `login` (save_login + flush_user): `privchat-sdk/src/lib.rs:5807-5808`
- FFI struct + mapper: `privchat-sdk-ffi/src/lib.rs:1728-1731, 2997-3004, 7468-7475`
- `fire_forced_logout` (does NOT touch K_ACTIVE_UID):
  `privchat-sdk/src/lib.rs:790-839`
- App-side fallbacks: `privchat-app/.../PrivChatApp.kt:543`,
  `PrivChatSDKManager.kt:346`

---

## Required completion-report answers

(per audit prompt's 7-question checklist)

| # | Question | Answer |
|---|----------|--------|
| 1 | list 拿到的是空账号，还是账号存在但 isActive=false? | **账号存在但 isActive=false.** `list_local_accounts` iterates `acct/<uid>/last_login_at` keys to build entries — these persist through `save_login`. The `is_active` boolean is then computed against `K_ACTIVE_UID`, which can be stale or missing. |
| 2 | Rust 存储里 is_active 是否存在? | **No per-account `is_active` field exists.** Storage has a singleton `K_ACTIVE_UID` pointer in the sled `accounts` tree. `is_active` is *computed* by the actor wrapper as `K_ACTIVE_UID == entry.uid`. |
| 3 | 登录成功后是否写盘 is_active=true? | **Effectively yes.** `save_login` writes `K_ACTIVE_UID = uid` (sled) AND `current_user_file = uid` (plain file). Caller follows up with `flush_user(uid)` → sled `global_db.flush()` → fsync. After login returns success, both stores are durably set. |
| 4 | 重启后 DB 中 is_active 实际值是什么? | Whatever was last persisted. Single-account flow: `K_ACTIVE_UID = uid` stays. The symptom of `is_active=false` on restart implies `K_ACTIVE_UID` is missing or mismatched — which can happen if `set_current_uid` was called with a uid different from the most recently `save_login`'d uid (since `set_current_uid` writes only the file, not `K_ACTIVE_UID`). |
| 5 | FFI/Kotlin 映射是否正确? | **Yes.** Rust struct `LocalAccountSummary { is_active: bool }` → UniFFI procmacro generates Kotlin `LocalAccountSummary(isActive: Boolean)` — 1:1, no bug. |
| 6 | 是否存在主动清 active 的代码? | **No code path actively clears `K_ACTIVE_UID` under normal operation.** Only `clear_session` (via `ClearLocalState`) and `wipe_user_full` (via `WipeCurrentUserFull`) clear it. Neither is invoked by App code (confirmed by grep). `fire_forced_logout` does NOT touch storage. |
| 7 | 推荐修复是布尔字段、active pointer，还是 App fallback? | **Active pointer (consolidate to one store).** Specifically **Option B.1**: make `current_user_file` the single source of truth for active uid; `list_local_accounts` reads from it instead of `K_ACTIVE_UID`. Zero migration, smallest patch surface (one function), `set_current_uid` already correct under this scheme. After the fix, App-side fallbacks can be reverted. |
