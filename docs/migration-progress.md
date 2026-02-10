# Migration Progress Matrix (Legacy SDK -> privchat-rust)

## Legend
- `DONE`: migrated and exported in `privchat-sdk-ffi`
- `PARTIAL`: core path done, advanced variants pending
- `TODO`: not migrated yet

## 1) Core Session Flow
- connect/login/register/authenticate/bootstrap/shutdown: `DONE`
- connection state + session snapshot: `DONE`
- supervised sync lifecycle: `DONE`

Optimization during migration:
- removed sync bridge paths from FFI, unified async object exports
- actor-serialized command execution to avoid lock contention across await

## 2) Local-first Storage
- SQL migration baseline + actorized storage path: `DONE`
- per-user isolated storage roots: `DONE`
- message/channel/user/group/reaction/mention/reminder local APIs: `DONE`

Optimization during migration:
- strong local-first gate (`run_bootstrap_sync` required)
- local CRUD keyed only by `message.id`

## 3) Queue System
- normal outbound queue: `DONE`
- multi file queues: `DONE`
- local_message_id generation (Snowflake) + persistence: `DONE`

Optimization during migration:
- local retries continue using `message.id`
- server dedupe uses `local_message_id`

## 4) Remote RPC Capabilities
- generic rpc_call bridge: `DONE`
- friend/group/channel/message status/reaction/profile/privacy/qrcode/device/file/sync wrappers: `DONE` (core set)
- advanced long-tail wrappers from legacy: `PARTIAL`

Optimization during migration:
- wrappers reuse same actor transport path; no extra concurrency model introduced
- JSON request/response wrappers allow immediate parity while typed DTOs are incrementally added

## 5) Remaining High-priority Work
- legacy long-tail APIs (search/channel list variants/device list variants): `PARTIAL`
- event stream contract (sequence/replay/backpressure) strict spec + implementation: `DONE` (`sequence_id` + `recent_events/events_since` + polling observability)
- cancellation semantics and task shutdown conformance tests: `DONE` (shutdown 幂等、shutdown 后拒绝新请求测试已覆盖)
- iOS auto-repro gate integration as mandatory gate: `DONE` (`scripts/gate.sh` 默认执行 iOS auto-repro)

## 6) Acceptance Tracking
- cargo tests (`privchat-sdk`, `privchat-sdk-ffi`): passing
- architectural hard constraints (async-only FFI, no by-handle/snapshot sync helper): applied in new stack
- API gap (exact-name) latest snapshot: `0` missing (`docs/api-gap-report.md`)

## 7) Compatibility Aliases Added (Latest Batch)
- bootstrap/init compatibility:
  - retained: `is_initialized`, `is_shutting_down`
  - removed after native binding regeneration:
    `initialize`, `initialize_blocking`, `run_bootstrap_sync_async`, `run_bootstrap_sync_in_background`
- state/event compatibility:
  - `get_connection_summary`, `subscribe_events`, `subscribe_network_status`
- presence/typing compatibility:
  - `batch_get_presence`, `get_presence`, `clear_presence_cache`
  - `start_typing`, `start_typing_blocking`, `stop_typing`
- message operation compatibility:
  - `mark_as_read_blocking`, `recall_message_blocking`, `add_reaction_blocking`, `edit_message_blocking`

## 8) Coverage Milestone
- legacy API exact-name compatibility matrix reached `100%` (`155/155`) in `privchat-sdk-ffi`
- note: several compatibility methods are semantic wrappers/no-op placeholders that keep API surface stable while actorized internals remain async-only

## 9) Semantic Migration (Current)
- user settings now persisted in local KV (`get_all_user_settings/get_user_setting/set_user_setting`)
- channel preferences now persisted per channel in local KV:
  - notification mode / favourite / low priority / tags
- search semantics upgraded:
  - `search_channel` now filters local channel fields
  - `search_messages` now filters local message content
- read/presence/device compatibility improved:
  - `is_event_read_by` and `seen_by_for_event` now call read-list RPC route
  - read-list parser now supports multiple payload shapes (`[]`, `{data:[]}`, `{items:[]}`, `{result:{items:[]}}`)
  - `seen_by_for_event` now returns normalized array JSON payload
  - `get_presence_stats` now aggregates from friend list + presence fetch
  - `list_my_devices` now returns current session device snapshot
  - `dm_peer_user_id` now resolves from local channel members
  - `get_typing_stats` now returns real-time typing state counters and active channels (tracked in ffi runtime state)
  - `own_last_read` now prefers local `channel_extra.browse_to`, then falls back to max `server_message_id`
  - `mark_fully_read_at` now updates both remote read receipt and local unread state (best-effort)
  - `retry_message` now rebuilds outbound payload from local message record instead of empty payload
- friend/blacklist local-first consistency improved:
  - `delete_friend` now updates remote then deletes local friend relation atomically in the same API flow
  - local `blacklist` table migrated (`V20260210193000__blacklist.sql`) with per-user isolation
  - `add_to_blacklist/remove_from_blacklist/get_blacklist/check_blacklist` now reconcile local blacklist cache
  - new local APIs exported: `upsert_blacklist_entry/delete_blacklist_entry/list_blacklist_entries`
- bootstrap sync semantics upgraded:
  - `sync_entities` now applies server payload to local store (friend/user/group/group_member/channel/channel_member/user_block/blacklist/user_settings)
  - `run_bootstrap_sync` no longer only counts remote items; it now performs real local-first data hydration
- lifecycle compatibility improved:
  - `enter_background/on_app_background` now stop supervised sync + mark lifecycle state
  - `enter_foreground/on_app_foreground` now resume supervised sync
  - lifecycle callback bridge methods now emit structured logs
  - `get_connection_summary` now includes `app_in_background` and `supervised_sync_running`
- background compatibility wrappers hardened:
  - `run_bootstrap_sync_in_background` / `sync_entities_in_background` / `sync_messages_in_background`
    no longer depend on ambient Tokio reactor (`tokio::spawn` removed from FFI main path)
  - wrappers now run on dedicated background thread with explicit runtime bootstrap,
    preventing "there is no reactor running" regressions on iOS/Kotlin caller threads
- new ffi unit tests:
  - read-list parsing compatibility tests added in `privchat-sdk-ffi/src/lib.rs`
- queue control semantics improved:
  - `send_queue_set_enabled` and `channel_send_queue_set_enabled` now take effect
  - outbound message/file enqueue path now enforces global + per-channel queue enable flags
  - `file_api_base_url` now derives from current server endpoint config
- callback bridge semantics improved:
  - callback registration methods now persist internal registration state flags
  - `next_event` now tracks polling call count
  - `get_connection_summary` now includes callback/hook/listener registration flags
  - `event_config` now includes event poll count for observability
- protocol typed RPC migration (in-progress, strict mode):
  - migrated from manual JSON to `privchat-protocol` typed req/resp:
    - `search_users`
    - `send_friend_request`
    - `get_friend_pending_requests`
    - `accept_friend_request`
    - `reject_friend_request`
    - `get_or_create_direct_channel`
    - `create_group`
    - `get_group_info`
    - `fetch_group_members_remote`
    - `delete_friend`
    - `add_to_blacklist`
    - `remove_from_blacklist`
    - `get_blacklist`
    - `check_blacklist`
    - `check_friend`
    - `mark_as_read`
    - `recall_message`
    - `add_reaction`
    - `remove_reaction`
    - `list_reactions`
    - `reaction_stats`
    - `reactions_batch`
    - `pin_channel`
    - `hide_channel`
    - `mute_channel`
    - `update_device_push_state`
    - `get_device_push_status`
    - `get_messages_remote`
    - `message_read_list`
    - `message_read_stats`
    - `get_privacy_settings`
    - `update_privacy_settings`
    - `qrcode_generate`
    - `qrcode_resolve`
    - `qrcode_list`
    - `user_qrcode_get`
    - `search_user_by_qrcode`
    - `account_user_detail_remote`
    - `account_user_share_card_remote`
    - `account_user_update_remote`
    - `qrcode_refresh`
    - `qrcode_revoke`
    - `user_qrcode_generate`
    - `user_qrcode_refresh`
    - `message_unread_count_remote`
    - `group_add_members_remote`
    - `group_remove_member_remote`
    - `group_leave_remote`
    - `group_mute_member_remote`
    - `group_unmute_member_remote`
    - `group_transfer_owner_remote`
    - `group_set_role_remote`
    - `group_get_settings_remote`
    - `group_update_settings_remote`
    - `group_mute_all_remote`
    - `group_approval_list_remote`
    - `group_approval_handle_remote`
    - `group_qrcode_generate_remote`
    - `group_qrcode_join_remote`
  - local-first cache update logic preserved during typed migration (friend/blacklist/group/group_member upsert/delete)
  - protocol alignment completed for message-status payload naming:
    - `MessageStatusReadRequest/MessageReadListRequest/MessageReadStatsRequest` now use `message_id` (with `server_message_id` alias compatibility)
    - added `MessageStatusCountRequest/MessageStatusCountResponse`
  - additional typed migration completed:
    - `sync_submit_remote` -> `ClientSubmitRequest/ClientSubmitResponse`
    - `entity_sync_remote` -> `SyncEntitiesRequest/SyncEntitiesResponse`
    - `sync_get_difference_remote` -> `GetDifferenceRequest/GetDifferenceResponse`
    - `sync_get_channel_pts_remote` -> `GetChannelPtsRequest/GetChannelPtsResponse`
    - `sync_batch_get_channel_pts_remote` -> `BatchGetChannelPtsRequest/BatchGetChannelPtsResponse`
    - `file_request_upload_token_remote` -> `FileRequestUploadTokenRequest/FileRequestUploadTokenResponse`
    - `file_upload_callback_remote` -> `FileUploadCallbackRequest/FileUploadCallbackResponse`
    - `channel_broadcast_subscribe_remote` -> `ChannelBroadcastSubscribeRequest/ChannelBroadcastSubscribeResponse`
    - `auth_logout_remote` -> `AuthLogoutRequest/AuthLogoutResponse`
    - `auth_refresh_remote` -> `AuthRefreshRequest/AuthRefreshResponse`
    - `get_profile/update_profile` -> `AccountProfileGetRequest/AccountProfileUpdateRequest` typed paths
    - `channel_broadcast_create_remote` -> `ChannelBroadcastCreateRequest/ChannelBroadcastCreateResponse`
    - `channel_broadcast_list_remote` -> `ChannelBroadcastListRequest/ChannelBroadcastListResponse`
    - `channel_content_publish_remote` -> `ChannelContentPublishRequest/ChannelContentPublishResponse`
    - `channel_content_list_remote` -> `ChannelContentListRequest/ChannelContentListResponse`
    - `sticker_package_list_remote` -> `StickerPackageListRequest/StickerPackageListResponse`
    - `sticker_package_detail_remote` -> `StickerPackageDetailRequest/StickerPackageDetailResponse`
  - remaining manual `serde_json::json!` callsites in `privchat-sdk-ffi`: `0` (down from `55`)
  - remaining direct `rpc_call(route, payload_json)` business callsites in `privchat-sdk-ffi`: `0`
  - direct `rpc_call(...)` now only exists in core helpers (`rpc_call_typed` and compatibility `rpc_call` wrapper)
  - FFI typed-return batch (json wrapper removed on bool-like APIs):
    - `hide_channel` / `mute_channel`
    - `reject_friend_request` / `delete_friend`
    - `invite_to_group` / `remove_group_member` / `leave_group`
    - `group_add_members_remote` / `group_remove_member_remote` / `group_leave_remote`
    - `leave_channel`
  - FFI typed-return batch (json wrapper removed on friend/group/channel flows):
    - `send_friend_request` -> `FriendRequestResult`
    - `accept_friend_request` -> `u64` (channel id)
    - `get_or_create_direct_channel` -> `DirectChannelResult`
    - `create_group` -> `GroupCreateResult`
    - `group_qrcode_join_remote` / `join_group_by_qrcode` -> `GroupQrCodeJoinResult`
    - `get_channel_sync_state` -> `ChannelSyncState`
  - Kotlin native adapter aligned to typed FFI (removed JSON parsing for above APIs)
- gate verification:
  - iOS auto-repro (`scripts/ios-auto-repro.sh`) latest run passes full chain:
    `create -> connect -> login -> authenticate -> runBootstrapSync -> MainTabPage`
  - latest verification logs:
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-195337.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-200710.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-204314.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-205058.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-210956.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-224156.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260210-225154.log`
    - `/Users/zoujiaqing/projects/privchat/privchat-sdk-kotlin/.repro-logs/ios-app-stdout-20260211-002751.log`
