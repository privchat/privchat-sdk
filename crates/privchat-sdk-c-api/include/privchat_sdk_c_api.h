// Stable C ABI for the PrivChat SDK.
// Hand-maintained counterpart of crates/privchat-sdk-c-api/src/lib.rs.
// Rules: opaque handle + scalars + UTF-8 JSON strings only. Strings returned
// here are owned by the library; free them with privchat_capi_free_string.

#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct PrivchatCapiClient PrivchatCapiClient;

#define PRIVCHAT_CAPI_OK 0
#define PRIVCHAT_CAPI_ERR_SDK 1
#define PRIVCHAT_CAPI_ERR_INVALID_ARG 2
#define PRIVCHAT_CAPI_ERR_TIMEOUT 3

// --- lifecycle -------------------------------------------------------------

// config_json: JSON-serialized PrivchatConfig, e.g.
// {"endpoints":[{"protocol":"Quic","host":"127.0.0.1","port":8443,
//   "path":null,"use_tls":false}],
//  "connection_timeout_secs":30,"data_dir":"/tmp/privchat-godot"}
PrivchatCapiClient* privchat_capi_client_create(const char* config_json);
void privchat_capi_client_destroy(PrivchatCapiClient* client);

// Last error on this thread; NULL when the last call succeeded. The pointer
// is invalidated by the NEXT c-api call on the same thread (successful calls
// clear the stored message too) — copy it before calling anything else.
// Do not free.
const char* privchat_capi_last_error(void);

void privchat_capi_free_string(char* s);

// --- connect / auth ----------------------------------------------------------

int32_t privchat_capi_authenticate(PrivchatCapiClient* client, uint64_t user_id,
                                   const char* token, const char* device_id,
                                   uint64_t timeout_ms);
int32_t privchat_capi_connect(PrivchatCapiClient* client, uint64_t timeout_ms);
int32_t privchat_capi_disconnect(PrivchatCapiClient* client, uint64_t timeout_ms);
// Bootstrap sync gate: must complete after authenticate+connect before any
// local-first operation (e.g. send_text_message), otherwise those calls are
// rejected with "run_bootstrap_sync required".
int32_t privchat_capi_run_bootstrap_sync(PrivchatCapiClient* client, uint64_t timeout_ms);
int32_t privchat_capi_shutdown(PrivchatCapiClient* client, uint64_t timeout_ms);

// JSON ConnectionState (serde variant name, e.g. "Authenticated").
char* privchat_capi_connection_state(PrivchatCapiClient* client, uint64_t timeout_ms);
// JSON SessionSnapshot, or JSON "null" when no session.
char* privchat_capi_session_snapshot(PrivchatCapiClient* client, uint64_t timeout_ms);

// --- channels ----------------------------------------------------------------

// channel_type: 0=Private, 1=Group, 2=Room. token may be NULL.
int32_t privchat_capi_subscribe_channel(PrivchatCapiClient* client, uint64_t channel_id,
                                        uint8_t channel_type, const char* token,
                                        uint64_t timeout_ms);
int32_t privchat_capi_unsubscribe_channel(PrivchatCapiClient* client, uint64_t channel_id,
                                          uint8_t channel_type, uint64_t timeout_ms);
int32_t privchat_capi_sync_channel(PrivchatCapiClient* client, uint64_t channel_id,
                                   int32_t channel_type, uint64_t timeout_ms,
                                   uint64_t* out_applied);

// --- messages ------------------------------------------------------------------

// Queue-first text send; out_message_id receives the local message id.
int32_t privchat_capi_send_text_message(PrivchatCapiClient* client, uint64_t channel_id,
                                        int32_t channel_type, uint64_t from_uid,
                                        const char* content, uint64_t timeout_ms,
                                        uint64_t* out_message_id);

// JSON array of SequencedSdkEvent.
char* privchat_capi_recent_events(PrivchatCapiClient* client, uint64_t limit);
char* privchat_capi_timeline_events_since(PrivchatCapiClient* client,
                                          uint64_t from_sequence_id, uint64_t limit);
// Unfiltered: ALL sequenced events (incl. SubscriptionMessageReceived).
char* privchat_capi_events_since(PrivchatCapiClient* client,
                                 uint64_t from_sequence_id, uint64_t limit);

// JSON StoredMessage, JSON "null" when not found.
char* privchat_capi_get_message_by_id(PrivchatCapiClient* client, uint64_t message_id,
                                      uint64_t timeout_ms);

// --- conversation history / channel list / read state ----------------------
// Local-first mirrors of the UniFFI surface: local SQLite is the render
// source of truth; the SDK decides when to hydrate from the server and
// persists the history-gap watermark.

// Open a conversation: local rows first; when local is empty the SDK hydrates
// one LATEST window. Returns JSON
// {"messages":[StoredMessage,...],"has_more_before":bool,"fetched_from_server":bool}.
char* privchat_capi_open_conversation(PrivchatCapiClient* client, uint64_t channel_id,
                                      int32_t channel_type, uint32_t limit,
                                      uint64_t timeout_ms);

// Scroll-up paging; has_more_before=false means top reached (persisted).
// Returns JSON {"messages":[StoredMessage,...],"has_more_before":bool}.
char* privchat_capi_load_older_history(PrivchatCapiClient* client, uint64_t channel_id,
                                       int32_t channel_type,
                                       uint64_t before_server_message_id,
                                       uint32_t limit, uint64_t timeout_ms);

// Pure local page read (no network). Returns JSON [StoredMessage,...].
char* privchat_capi_list_messages(PrivchatCapiClient* client, uint64_t channel_id,
                                  int32_t channel_type, uint64_t limit,
                                  uint64_t offset, uint64_t timeout_ms);

// Local conversation list; entries carry unread_count/top/mute/
// last_msg_timestamp/last_msg_content for sorted badge lists.
// Returns JSON [StoredChannel,...].
char* privchat_capi_list_channels(PrivchatCapiClient* client, uint64_t limit,
                                  uint64_t offset, uint64_t timeout_ms);

// Advance the read cursor: RPC message/status/read_pts, then project the
// server-confirmed cursor locally. out_last_read_pts receives the accepted pts.
int32_t privchat_capi_mark_read_to_pts(PrivchatCapiClient* client, uint64_t channel_id,
                                       uint64_t read_pts, uint64_t timeout_ms,
                                       uint64_t* out_last_read_pts);

// Unread counters (local). exclude_muted != 0 skips muted channels.
int32_t privchat_capi_get_channel_unread_count(PrivchatCapiClient* client,
                                               uint64_t channel_id, int32_t channel_type,
                                               uint64_t timeout_ms, int32_t* out_count);
int32_t privchat_capi_get_total_unread_count(PrivchatCapiClient* client,
                                             int32_t exclude_muted, uint64_t timeout_ms,
                                             int32_t* out_count);

// --- transfer / rpc --------------------------------------------------------

// body is passed through as raw bytes (callers send a JSON string).
// Returns JSON {"request_id","channel_id","code","message","data"}.
char* privchat_capi_transfer(PrivchatCapiClient* client, uint64_t channel_id,
                             const char* route, const char* body, uint64_t timeout_ms);
// Returns the server JSON body as a string.
/* Owned byte buffer; release with privchat_capi_free_buffer.
 * Binary-safe: embedded NUL bytes are preserved (FlatBuffers/Protobuf). */
typedef struct PrivchatCapiBuffer {
    uint8_t* data;
    size_t len;
} PrivchatCapiBuffer;

/* Free a buffer produced by this library. NULL is a no-op. */
void privchat_capi_free_buffer(PrivchatCapiBuffer* buffer);

/* Binary-safe Channel Transfer. out_code receives the envelope code (0 = ok);
 * out_reply receives the raw reply payload (release with free_buffer).
 * Returns PRIVCHAT_CAPI_OK when the round trip completed. */
int32_t privchat_capi_transfer_bytes(const PrivchatCapiClient* client,
                                     uint64_t channel_id, const char* route,
                                     const uint8_t* body, size_t body_len,
                                     uint64_t timeout_ms, int32_t* out_code,
                                     PrivchatCapiBuffer* out_reply);

char* privchat_capi_rpc_call(PrivchatCapiClient* client, const char* route,
                             const char* body_json, uint64_t timeout_ms);

#ifdef __cplusplus
}
#endif
