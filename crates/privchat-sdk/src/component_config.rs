//! Public server-owned component configuration. Credentials remain in call/join only.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentConfig {
    pub schema_version: u32,
    pub revision: String,
    pub max_age_seconds: u32,
    pub components: Components,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Components {
    pub rtc: RtcComponent,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RtcComponent {
    pub schema_version: u32,
    pub enabled: bool,
    pub livekit_url: String,
    pub peer_enabled: bool,
    pub allow_turn: bool,
}

impl crate::PrivchatSdk {
    pub async fn get_component_config(&self) -> crate::Result<ComponentConfig> {
        self.rpc_call_typed("config/get", &serde_json::json!({}))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_wire_shape_is_camel_case_and_contains_no_credential_fields() {
        let raw = serde_json::json!({"schemaVersion":1,"revision":"opaque","maxAgeSeconds":300,
            "components":{"rtc":{"schemaVersion":1,"enabled":true,"livekitUrl":"wss://rtc.example.invalid","peerEnabled":true,"allowTurn":false}}});
        let view: ComponentConfig = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(view).unwrap(), raw);
    }
}
