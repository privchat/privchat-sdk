use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub struct DeduplicationManager {
    seen_messages: Arc<Mutex<HashSet<String>>>,
}

impl DeduplicationManager {
    pub fn new() -> Self {
        Self {
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn is_duplicate(&self, client_msg_no: &str) -> bool {
        let mut seen_messages = self.seen_messages.lock().unwrap();
        if seen_messages.contains(client_msg_no) {
            true
        } else {
            seen_messages.insert(client_msg_no.to_string());
            false
        }
    }
} 