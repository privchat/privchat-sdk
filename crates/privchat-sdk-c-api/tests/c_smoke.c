// C smoke test for privchat-sdk-c-api.
//
// 1. `cargo test -p privchat-sdk-c-api` syntax-checks it against the header
//    (c_smoke_compiles_against_header) and links + executes it against the
//    built cdylib (c_smoke_links_and_runs) — real ABI signature proof.
// 2. Manual runtime run:
//      cc tests/c_smoke.c -Iinclude \
//         -L../../target/release -lprivchat_sdk_c_api \
//         -Wl,-rpath,../../target/release -o c_smoke
//      ./c_smoke
//    (no server required; the connect attempt is expected to fail fast.)
#include <stdio.h>
#include <string.h>
#include "privchat_sdk_c_api.h"

int main(void) {
    // 1. invalid config must fail with a message
    PrivchatCapiClient* bad = privchat_capi_client_create("{not json");
    if (bad != NULL) { printf("FAIL: invalid config accepted\n"); return 1; }
    const char* err = privchat_capi_last_error();
    printf("invalid-config error: %s\n", err ? err : "(null)");

    // 2. valid config (tcp, local)
    const char* cfg = "{\"endpoints\":[{\"protocol\":\"Tcp\",\"host\":\"127.0.0.1\","
                      "\"port\":9001,\"path\":null,\"use_tls\":false}],"
                      "\"connection_timeout_secs\":5,"
                      "\"data_dir\":\"/tmp/privchat-capi-smoke\"}";
    PrivchatCapiClient* c = privchat_capi_client_create(cfg);
    if (c == NULL) {
        printf("FAIL: create returned NULL: %s\n", privchat_capi_last_error());
        return 1;
    }
    printf("client created\n");

    // 3. sync event path without server
    char* events = privchat_capi_recent_events(c, 10);
    printf("recent_events: %s\n", events ? events : "(null)");
    privchat_capi_free_string(events);

    // 4. connect should fail fast (no server) but must not crash
    int rc = privchat_capi_connect(c, 6000);
    printf("connect rc=%d err=%s\n", rc, privchat_capi_last_error() ? privchat_capi_last_error() : "(none)");

    privchat_capi_client_destroy(c);
    printf("destroyed ok\n");
    return 0;
}
