# FFI API Inventory (privchat-rust)

本文档列出 `crates/privchat-sdk-ffi/src/lib.rs` 当前对外 UniFFI async API，作为旧 `privchat-sdk/privchat-ffi` 对齐与回归基线。

## 1. Session / Connection

- `connect()`
- `disconnect()`
- `is_connected()`
- `connection_state()`
- `next_event(timeout_ms)` (returns `ConnectionStateChanged/BootstrapCompleted/Shutdown*`)
- `ping()`
- `login(username, password, device_id)`
- `register(username, password, device_id)`
- `authenticate(user_id, token, device_id)`
- `run_bootstrap_sync()`
- `is_bootstrap_completed()`
- `session_snapshot()`
- `clear_local_state()`
- `shutdown()`

## 2. Sync

- `sync_entities(entity_type, scope)`
- `sync_channel(channel_id, channel_type)`
- `sync_all_channels()`

## 3. Presence / Typing

- `subscribe_presence(user_ids)`
- `unsubscribe_presence(user_ids)`
- `fetch_presence(user_ids)`
- `send_typing(channel_id, channel_type, is_typing, action_type)`

## 3.5 RPC Bridge (Migration Accelerator)

- `rpc_call(route, body_json)` (统一异步 RPC，返回 JSON 字符串)
- `search_users(query)`
- `send_friend_request(target_user_id, message, source, source_id)`
- `get_friend_pending_requests()`
- `accept_friend_request(from_user_id, message)`
- `reject_friend_request(from_user_id, message)`
- `get_or_create_direct_channel(peer_user_id)`
- `create_group(name, description, member_ids)`
- `get_group_info(group_id)`
- `fetch_group_members_remote(group_id, page, page_size)`
- `delete_friend(friend_id)`
- `add_to_blacklist(blocked_user_id)`
- `remove_from_blacklist(blocked_user_id)`
- `get_blacklist()`
- `check_blacklist(target_user_id)`
- `mark_as_read(channel_id, server_message_id)`
- `recall_message(server_message_id, channel_id)`
- `add_reaction(server_message_id, channel_id, emoji)`
- `remove_reaction(server_message_id, emoji)`
- `list_reactions(server_message_id)`
- `reaction_stats(server_message_id)`
- `pin_channel(channel_id, pinned)`
- `hide_channel(channel_id)`
- `mute_channel(channel_id, muted)`
- `update_device_push_state(device_id, apns_armed, push_token)`
- `get_device_push_status(device_id)`
- `get_messages_remote(channel_id, before_server_message_id, limit)`
- `message_read_list(server_message_id, channel_id)`
- `message_read_stats(server_message_id, channel_id)`
- `check_friend(friend_id)`
- `get_profile()`
- `update_profile(profile_json)`
- `get_privacy_settings()`
- `update_privacy_settings(payload: AccountPrivacyUpdateInput)`
- `qrcode_generate(qr_type, payload, expire_seconds)`
- `qrcode_resolve(qr_key, token)`
- `qrcode_list(qr_type)`
- `user_qrcode_get()`
- `search_user_by_qrcode(qr_key, token)`
- `account_user_detail_remote(user_id)`
- `account_user_share_card_remote(user_id)`
- `account_user_update_remote(payload: AccountUserUpdateInput)`
- `auth_logout_remote()`
- `auth_refresh_remote(payload: AuthRefreshInput)`
- `qrcode_refresh(qr_key, expire_seconds)`
- `qrcode_revoke(qr_key)`
- `user_qrcode_generate(expire_seconds)`
- `user_qrcode_refresh(expire_seconds)`
- `message_unread_count_remote(channel_id)`
- `group_add_members_remote(group_id, user_ids)`
- `group_remove_member_remote(group_id, user_id)`
- `group_leave_remote(group_id)`
- `group_mute_member_remote(group_id, user_id, duration_seconds)`
- `group_unmute_member_remote(group_id, user_id)`
- `group_transfer_owner_remote(group_id, target_user_id)`
- `group_set_role_remote(group_id, user_id, role)`
- `group_get_settings_remote(group_id)`
- `group_update_settings_remote(payload: GroupSettingsUpdateInput)`
- `group_mute_all_remote(group_id, enabled)`
- `group_approval_list_remote(group_id, page, page_size)`
- `group_approval_handle_remote(approval_id, approved, reason)`
- `group_qrcode_generate_remote(group_id, expire_seconds)`
- `group_qrcode_join_remote(qr_key, token)`
- `channel_broadcast_create_remote(payload: ChannelBroadcastCreateInput)`
- `channel_broadcast_subscribe_remote(payload: ChannelBroadcastSubscribeInput)`
- `channel_broadcast_list_remote(payload: ChannelBroadcastListInput)`
- `channel_content_publish_remote(payload: ChannelContentPublishInput)`
- `channel_content_list_remote(payload: ChannelContentListInput)`
- `sticker_package_list_remote(payload: StickerPackageListInput)`
- `sticker_package_detail_remote(payload: StickerPackageDetailInput)`
- `sync_submit_remote(payload: SyncSubmitInput)`
- `entity_sync_remote(payload: SyncEntitiesInput)`
- `sync_get_difference_remote(payload: GetDifferenceInput)`
- `sync_get_channel_pts_remote(payload: GetChannelPtsInput)`
- `sync_batch_get_channel_pts_remote(payload: BatchGetChannelPtsInput)`
- `file_request_upload_token_remote(payload: FileRequestUploadTokenInput)`
- `file_upload_callback_remote(payload: FileUploadCallbackInput)`

## 4. Outbound Queue (Local-first)

- `enqueue_outbound_message(message_id, payload)`
- `peek_outbound_messages(limit)`
- `ack_outbound_messages(message_ids)`
- `enqueue_outbound_file(message_id, route_key, payload)`
- `peek_outbound_files(queue_index, limit)`
- `ack_outbound_files(queue_index, message_ids)`

## 5. Message / Channel

- `create_local_message(input)`
- `get_message_by_id(message_id)`
- `list_messages(channel_id, channel_type, limit, offset)`
- `mark_message_sent(message_id, server_message_id)`
- `update_message_status(message_id, status)`
- `set_message_read(message_id, channel_id, channel_type, is_read)`
- `set_message_revoke(message_id, revoked, revoker)`
- `edit_message(message_id, content, edited_at)`
- `set_message_pinned(message_id, is_pinned)`
- `get_message_extra(message_id)`
- `upsert_channel(input)`
- `get_channel_by_id(channel_id)`
- `list_channels(limit, offset)`
- `upsert_channel_extra(input)`
- `get_channel_extra(channel_id, channel_type)`
- `mark_channel_read(channel_id, channel_type)`
- `get_channel_unread_count(channel_id, channel_type)`
- `get_total_unread_count(exclude_muted)`

## 6. User / Friend / Group

- `upsert_user(input)`
- `get_user_by_id(user_id)`
- `list_users_by_ids(user_ids)`
- `upsert_friend(input)`
- `list_friends(limit, offset)`
- `upsert_group(input)`
- `get_group_by_id(group_id)`
- `list_groups(limit, offset)`
- `upsert_group_member(input)`
- `list_group_members(group_id, limit, offset)`
- `upsert_channel_member(input)`
- `list_channel_members(channel_id, channel_type, limit, offset)`
- `delete_channel_member(channel_id, channel_type, user_id)`

## 7. Reaction / Mention / Reminder

- `upsert_message_reaction(input)`
- `list_message_reactions(message_id, limit, offset)`
- `record_mention(input)`
- `get_unread_mention_count(channel_id, channel_type, user_id)`
- `list_unread_mention_message_ids(channel_id, channel_type, user_id, limit)`
- `mark_mention_read(message_id, user_id)`
- `mark_all_mentions_read(channel_id, channel_type, user_id)`
- `get_all_unread_mention_counts(user_id)`
- `upsert_reminder(input)`
- `list_pending_reminders(uid, limit, offset)`
- `mark_reminder_done(reminder_id, done)`

## 8. KV / Storage Introspection

- `kv_put(key, value)`
- `kv_get(key)`
- `user_storage_paths()`

## 9. Hard Rules

- 仅允许 UniFFI object + async 导出。
- 不允许 by-handle/snapshot/sync helper 导出。
- `run_bootstrap_sync()` 未完成时，本地数据路径统一拒绝（local-first gate）。
- 本地主键统一为 `message.id`（即 message 表 `id`）。
- `local_message_id` 仅用于服务端幂等去重，不作为本地 CRUD 主键。
