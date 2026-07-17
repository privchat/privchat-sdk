use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncPhase {
    Idle,
    Syncing,
    Synced,
    Retrying,
    FailedTerminal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncRunKind {
    Bootstrap,
    Resume,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncStateSnapshot {
    pub phase: SyncPhase,
    pub run_kind: Option<SyncRunKind>,
    pub attempt: u32,
    pub error_code: Option<u32>,
    pub message: Option<String>,
    pub updated_at_ms: i64,
}

impl Default for SyncStateSnapshot {
    fn default() -> Self {
        Self {
            phase: SyncPhase::Idle,
            run_kind: None,
            attempt: 0,
            error_code: None,
            message: None,
            updated_at_ms: 0,
        }
    }
}

/// Owns sync lifecycle state. The SDK actor remains the sole executor, so this
/// coordinator deliberately does not spawn another task or hold async locks.
pub(crate) struct SyncCoordinator {
    snapshot: SyncStateSnapshot,
}

impl SyncCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: SyncStateSnapshot::default(),
        }
    }

    pub(crate) fn snapshot(&self) -> SyncStateSnapshot {
        self.snapshot.clone()
    }

    pub(crate) fn begin(&mut self, kind: SyncRunKind, now_ms: i64) -> bool {
        if self.snapshot.phase == SyncPhase::Syncing {
            return false;
        }
        self.snapshot = SyncStateSnapshot {
            phase: SyncPhase::Syncing,
            run_kind: Some(kind),
            attempt: self.snapshot.attempt,
            error_code: None,
            message: None,
            updated_at_ms: now_ms,
        };
        true
    }

    pub(crate) fn complete(&mut self, kind: SyncRunKind, now_ms: i64) {
        self.snapshot = SyncStateSnapshot {
            phase: SyncPhase::Synced,
            run_kind: Some(kind),
            attempt: 0,
            error_code: None,
            message: None,
            updated_at_ms: now_ms,
        };
    }

    pub(crate) fn fail(
        &mut self,
        kind: SyncRunKind,
        terminal: bool,
        error_code: Option<u32>,
        message: String,
        now_ms: i64,
    ) {
        let attempt = if terminal {
            self.snapshot.attempt
        } else {
            self.snapshot.attempt.saturating_add(1)
        };
        self.snapshot = SyncStateSnapshot {
            phase: if terminal {
                SyncPhase::FailedTerminal
            } else {
                SyncPhase::Retrying
            },
            run_kind: Some(kind),
            attempt,
            error_code,
            message: Some(message),
            updated_at_ms: now_ms,
        };
    }

    pub(crate) fn reset(&mut self, now_ms: i64) {
        self.snapshot = SyncStateSnapshot {
            updated_at_ms: now_ms,
            ..SyncStateSnapshot::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_begin_is_coalesced() {
        let mut coordinator = SyncCoordinator::new();
        assert!(coordinator.begin(SyncRunKind::Bootstrap, 1));
        assert!(!coordinator.begin(SyncRunKind::Resume, 2));
        assert_eq!(
            coordinator.snapshot().run_kind,
            Some(SyncRunKind::Bootstrap)
        );
    }

    #[test]
    fn recoverable_failures_increment_attempt_until_success() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Resume, 1);
        coordinator.fail(
            SyncRunKind::Resume,
            false,
            Some(9),
            "transport".to_string(),
            2,
        );
        assert_eq!(coordinator.snapshot().phase, SyncPhase::Retrying);
        assert_eq!(coordinator.snapshot().attempt, 1);

        coordinator.begin(SyncRunKind::Resume, 3);
        coordinator.fail(
            SyncRunKind::Resume,
            false,
            Some(9),
            "transport".to_string(),
            4,
        );
        assert_eq!(coordinator.snapshot().attempt, 2);

        coordinator.begin(SyncRunKind::Resume, 5);
        coordinator.complete(SyncRunKind::Resume, 6);
        assert_eq!(coordinator.snapshot().phase, SyncPhase::Synced);
        assert_eq!(coordinator.snapshot().attempt, 0);
    }

    #[test]
    fn terminal_failure_stops_retry_semantics() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Bootstrap, 1);
        coordinator.fail(
            SyncRunKind::Bootstrap,
            true,
            Some(10_002),
            "token expired".to_string(),
            2,
        );
        assert_eq!(coordinator.snapshot().phase, SyncPhase::FailedTerminal);
        assert_eq!(coordinator.snapshot().attempt, 0);
    }
}
