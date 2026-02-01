//! Privchat FFI - Foreign Function Interface for Privchat SDK
//! 
//! This crate provides cross-language bindings for the Privchat SDK using UniFFI.
//! It generates bindings for:
//! - Kotlin (Android)
//! - Swift (iOS)
//! - Python
//! - Ruby
//! 
//! # Architecture
//! 
//! The FFI layer follows UniFFI 0.31 best practices:
//! - Direct async/await support (no TaskHandle needed)
//! - Type-safe callback interfaces (no JSON serialization)
//! - Detailed error information in Error enums
//! - Memory management via UniFFI's Arc-based system

#![allow(clippy::new_without_default)]
#![allow(clippy::arc_with_non_send_sync)]

mod error;
mod config;
mod events;
mod sdk;
mod task_handle;
mod helpers;

#[cfg(test)]
mod observer_tests;

// Re-export public types
pub use error::PrivchatError;
pub use config::{parse_server_url, PrivchatConfig, PrivchatConfigBuilder, ServerConfig, ServerEndpoint, TransportProtocol, HttpClientConfig};
pub use events::{ConnectionState, NetworkStatus, PrivchatDelegate, MessageEntry, MessageStatus, EventType, SDKEvent, Channel, PresenceEntry, SendObserver, SendUpdate, SendState, TimelineObserver, TimelineDiffKind, ChannelListObserver, ChannelListEntry, LatestChannelEvent, FriendEntry, UserEntry, FriendPendingEntry, GroupEntry, GroupMemberEntry, BlacklistEntry, SyncStateEntry, AuthResult, GroupCreateResult, TypingObserver, TypingIndicatorEvent, ReceiptsObserver, ReadReceiptEvent, UnreadStats, LastReadPosition, GroupQRCodeJoinResult, SyncObserver, SyncStatus, SyncPhase, SearchPage, SearchHit, NotificationMode, ChannelTags, DeviceSummary, ReactionChip, SeenByEntry, MediaProcessOp, VideoProcessHook, SendMessageOptions, AttachmentInfo, AttachmentSendResult, ProgressObserver};
pub use sdk::PrivchatSDK;
pub use task_handle::TaskHandle;

// Setup UniFFI scaffolding for proc-macro mode
// This allows us to use #[uniffi::export] directly on async functions
uniffi::setup_scaffolding!();

/// Get SDK version string（来自 privchat-sdk version.rs，单一来源）
#[uniffi::export]
pub fn sdk_version() -> String {
    privchat_sdk::version::SDK_VERSION.to_string()
}

/// Get git commit SHA（用于日志、debug、上报）
#[uniffi::export]
pub fn git_sha() -> String {
    privchat_sdk::version::GIT_SHA.to_string()
}

/// Get build timestamp（用于日志、debug、上报）
#[uniffi::export]
pub fn build_time() -> String {
    privchat_sdk::version::BUILD_TIME.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sdk_version() {
        let version = sdk_version();
        assert!(!version.is_empty());
        assert!(version.chars().next().unwrap().is_ascii_digit()); // semver starts with digit
    }
}
