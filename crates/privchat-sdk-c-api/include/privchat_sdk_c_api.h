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

// Last error on this thread; NULL when the last call succeeded. Valid until
// the next failing c-api call on the same thread. Do not free.
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

// --- transfer / rpc --------------------------------------------------------

// body is passed through as raw bytes (callers send a JSON string).
// Returns JSON {"request_id","channel_id","code","message","data"}.
char* privchat_capi_transfer(PrivchatCapiClient* client, uint64_t channel_id,
                             const char* route, const char* body, uint64_t timeout_ms);
// Returns the server JSON body as a string.
char* privchat_capi_rpc_call(PrivchatCapiClient* client, const char* route,
                             const char* body_json, uint64_t timeout_ms);

#ifdef __cplusplus
}
#endif
