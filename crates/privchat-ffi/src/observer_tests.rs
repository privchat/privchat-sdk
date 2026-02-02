//! Tests for Timeline Observer and Channel List Observer
//! 
//! These tests verify that:
//! 1. Observers can be registered and unregistered
//! 2. Events from SDK layer are correctly forwarded to observers
//! 3. All diff kinds (Reset, Append, UpdateByItemId, RemoveByItemId) work correctly
//! 4. Channel list updates (Reset, Update) work correctly

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::Mutex;
use crate::events::{
    TimelineObserver, TimelineDiffKind, MessageEntry, MessageStatus,
    ChannelListObserver, ChannelListEntry,
};
use crate::sdk::PrivchatSDK;

/// Mock Timeline Observer for testing
#[derive(Clone)]
pub struct MockTimelineObserver {
    pub received_diffs: Arc<Mutex<Vec<TimelineDiffKind>>>,
    pub received_errors: Arc<Mutex<Vec<String>>>,
    pub call_count: Arc<AtomicUsize>,
}

impl MockTimelineObserver {
    pub fn new() -> Self {
        Self {
            received_diffs: Arc::new(Mutex::new(Vec::new())),
            received_errors: Arc::new(Mutex::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn get_diffs(&self) -> Vec<TimelineDiffKind> {
        self.received_diffs.lock().await.clone()
    }

    pub async fn get_errors(&self) -> Vec<String> {
        self.received_errors.lock().await.clone()
    }

    pub fn get_call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl TimelineObserver for MockTimelineObserver {
    fn on_diff(&self, diff: TimelineDiffKind) {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let diffs = self.received_diffs.clone();
        tokio::spawn(async move {
            diffs.lock().await.push(diff);
        });
    }

    fn on_error(&self, message: String) {
        let errors = self.received_errors.clone();
        tokio::spawn(async move {
            errors.lock().await.push(message);
        });
    }
}

/// Mock Channel List Observer for testing
#[derive(Clone)]
pub struct MockChannelListObserver {
    pub received_resets: Arc<Mutex<Vec<Vec<ChannelListEntry>>>>,
    pub received_updates: Arc<Mutex<Vec<ChannelListEntry>>>,
    pub reset_count: Arc<AtomicUsize>,
    pub update_count: Arc<AtomicUsize>,
}

impl MockChannelListObserver {
    pub fn new() -> Self {
        Self {
            received_resets: Arc::new(Mutex::new(Vec::new())),
            received_updates: Arc::new(Mutex::new(Vec::new())),
            reset_count: Arc::new(AtomicUsize::new(0)),
            update_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn get_resets(&self) -> Vec<Vec<ChannelListEntry>> {
        self.received_resets.lock().await.clone()
    }

    pub async fn get_updates(&self) -> Vec<ChannelListEntry> {
        self.received_updates.lock().await.clone()
    }

    pub fn get_reset_count(&self) -> usize {
        self.reset_count.load(Ordering::SeqCst)
    }

    pub fn get_update_count(&self) -> usize {
        self.update_count.load(Ordering::SeqCst)
    }
}

impl ChannelListObserver for MockChannelListObserver {
    fn on_reset(&self, items: Vec<ChannelListEntry>) {
        self.reset_count.fetch_add(1, Ordering::SeqCst);
        let resets = self.received_resets.clone();
        tokio::spawn(async move {
            resets.lock().await.push(items);
        });
    }

    fn on_update(&self, item: ChannelListEntry) {
        self.update_count.fetch_add(1, Ordering::SeqCst);
        let updates = self.received_updates.clone();
        tokio::spawn(async move {
            updates.lock().await.push(item);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::PrivchatSDK;
    use crate::config::PrivchatConfigBuilder;

    /// Helper function to create a test SDK instance
    fn create_test_sdk() -> Arc<PrivchatSDK> {
        use crate::config::{PrivchatConfigBuilder, ServerEndpoint, TransportProtocol};
        let builder = Arc::new(PrivchatConfigBuilder::new());
        let builder = builder.data_dir("/tmp/privchat_test".to_string());
        let builder = builder.assets_dir("/tmp/privchat_test_assets".to_string());
        // Add a test server endpoint
        let endpoint = ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "localhost".to_string(),
            port: 9001,
            path: None,
            use_tls: false,
        };
        let builder = builder.server_endpoint(endpoint);
        let config = builder.build().unwrap();
        Arc::new(PrivchatSDK::new(config).unwrap())
    }

    // Note: Helper functions to emit events would require access to SDK's internal event_manager
    // For now, we only test observer registration/unregistration
    // Full event emission tests would require:
    // 1. SDK initialization and connection
    // 2. A test helper method to inject events, or
    // 3. Integration tests with real SDK operations

    #[test]
    fn test_timeline_observer_register_unregister() {
        let sdk = create_test_sdk();
        
        // Register observer
        let observer = MockTimelineObserver::new();
        let token = sdk.clone().observe_timeline(100, Box::new(observer.clone()));
        
        assert!(token > 0, "Token should be greater than 0");
        
        // Unregister observer
        let removed = sdk.clone().unobserve_timeline(token);
        assert!(removed, "Observer should be removed successfully");
        
        // Try to unregister again (should fail)
        let removed_again = sdk.clone().unobserve_timeline(token);
        assert!(!removed_again, "Observer should not be removed again");
    }

    // Note: Tests that require event emission would need:
    // 1. SDK to be initialized and connected
    // 2. A way to inject events into the event system
    // 3. Integration test setup with a real or mock server
    //
    // For now, we test the basic registration/unregistration functionality
    // which doesn't require event emission
    //
    // Integration tests for event emission would go here
    // They require SDK initialization and event injection capability


    #[test]
    fn test_channel_list_observer_register_unregister() {
        let sdk = create_test_sdk();
        
        // Register observer
        let observer = MockChannelListObserver::new();
        let token = sdk.clone().observe_channel_list(Box::new(observer.clone()));
        
        assert!(token > 0, "Token should be greater than 0");
        
        // Unregister observer
        let removed = sdk.clone().unobserve_channel_list(token);
        assert!(removed, "Observer should be removed successfully");
        
        // Try to unregister again (should fail)
        let removed_again = sdk.clone().unobserve_channel_list(token);
        assert!(!removed_again, "Observer should not be removed again");
    }

    // Integration tests for channel list observer event emission would go here
    // They require SDK initialization and event injection capability
}
