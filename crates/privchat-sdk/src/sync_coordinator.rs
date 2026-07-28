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

/// 退避基数与上限。1s 起步、翻倍、封顶 60s。
pub(crate) const SYNC_RETRY_BASE_MS: i64 = 1_000;
pub(crate) const SYNC_RETRY_MAX_MS: i64 = 60_000;

/// Owns sync lifecycle state **and the single retry deadline**. The SDK actor
/// remains the sole executor, so this coordinator deliberately does not spawn
/// another task or hold async locks.
///
/// 为什么 deadline 必须归它：sync 有多个触发源（重连成功、connect 成功、token
/// 刷新后重认证、显式命令）。2026-07-28 真机实测，账号切换后这些触发源与失败互相
/// 喂：sync 失败 → 重连 → 连接事件 → 再 sync，5 次/秒、CPU 50%、消息发不出去。
/// 在任何一个调用点 sleep 都堵不住，因为下一个触发源立刻又进来了；唯一能收敛的
/// 位置是所有触发源共同经过的这道闸门。
pub(crate) struct SyncCoordinator {
    snapshot: SyncStateSnapshot,
    /// 下一次允许开跑的时刻。`None` = 不受限。
    next_retry_at_ms: Option<i64>,
    /// account/session 世代。切账号、重置会 bump；跨代的旧结果不得回写。
    generation: u64,
}

/// `begin` 被拒的原因。调用方据此决定是「静默跳过」还是「已经在跑」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncBeginRejection {
    /// 已有一轮在跑（原有的合并语义）。
    AlreadyRunning,
    /// 处于退避窗口内，还差 `remaining_ms`。
    Backoff { remaining_ms: i64 },
    /// 终态失败（如 token 失效），不再自动重试，必须由显式动作复位。
    Terminal,
}

impl SyncCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: SyncStateSnapshot::default(),
            next_retry_at_ms: None,
            generation: 0,
        }
    }

    pub(crate) fn snapshot(&self) -> SyncStateSnapshot {
        self.snapshot.clone()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// 下一次允许开跑的绝对时刻（epoch ms）。actor 据此建定时器唤醒。
    pub(crate) fn next_retry_at_ms(&self) -> Option<i64> {
        self.next_retry_at_ms
    }

    /// 第 `attempt` 次失败之后要等多久。指数退避 + 抖动，封顶 `SYNC_RETRY_MAX_MS`。
    ///
    /// 抖动取自 `now_ms`（±25%）而不是 rng：这里不需要密码学随机，只需要让同一
    /// 时刻掉线的一批设备不要在同一毫秒重试；用时间派生还能让测试保持确定。
    pub(crate) fn retry_delay_ms(attempt: u32, now_ms: i64) -> i64 {
        let shift = attempt.saturating_sub(1).min(6);
        let base = SYNC_RETRY_BASE_MS
            .saturating_mul(1_i64 << shift)
            .min(SYNC_RETRY_MAX_MS);
        let span = (base / 4).max(1);
        let jitter = now_ms.rem_euclid(span * 2) - span;
        (base + jitter).clamp(SYNC_RETRY_BASE_MS, SYNC_RETRY_MAX_MS)
    }

    pub(crate) fn begin(
        &mut self,
        kind: SyncRunKind,
        now_ms: i64,
    ) -> std::result::Result<(), SyncBeginRejection> {
        if self.snapshot.phase == SyncPhase::Syncing {
            return Err(SyncBeginRejection::AlreadyRunning);
        }
        if self.snapshot.phase == SyncPhase::FailedTerminal {
            return Err(SyncBeginRejection::Terminal);
        }
        if let Some(deadline) = self.next_retry_at_ms {
            if now_ms < deadline {
                return Err(SyncBeginRejection::Backoff {
                    remaining_ms: deadline - now_ms,
                });
            }
        }
        self.next_retry_at_ms = None;
        self.snapshot = SyncStateSnapshot {
            phase: SyncPhase::Syncing,
            run_kind: Some(kind),
            attempt: self.snapshot.attempt,
            error_code: None,
            message: None,
            updated_at_ms: now_ms,
        };
        Ok(())
    }

    pub(crate) fn complete(&mut self, kind: SyncRunKind, now_ms: i64) {
        self.next_retry_at_ms = None;
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
        // 终态不排期：它等的不是时间，是一个显式动作（重新登录 / 换账号）。
        self.next_retry_at_ms = if terminal {
            None
        } else {
            Some(now_ms.saturating_add(Self::retry_delay_ms(attempt, now_ms)))
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

    /// 网络恢复：立刻解除退避窗口。
    ///
    /// 退避是为了不在**没有希望**的时候空转；网络刚回来正是最有希望的时刻，
    /// 这时还让用户干等剩余的退避时间是把节流用错了地方。
    pub(crate) fn note_network_available(&mut self) {
        if self.snapshot.phase == SyncPhase::Retrying {
            self.next_retry_at_ms = None;
        }
    }

    /// 这一轮被主动放弃（不是失败）：把 Running 撤下来，不计 attempt、不排退避。
    ///
    /// 用在「同步跑到一半被账号切换打断」：那一轮的结果已经不属于任何人了，既不该
    /// 记成失败（会让新账号背上上一个账号的 attempt 和退避），也不能留着 Running
    /// 不动——那样闸门永远关着，后面所有触发源都会被 begin() 挡回去，同步彻底卡死。
    pub(crate) fn abandon(&mut self, now_ms: i64) {
        self.next_retry_at_ms = None;
        self.snapshot = SyncStateSnapshot {
            updated_at_ms: now_ms,
            ..SyncStateSnapshot::default()
        };
    }

    /// 换账号 / 换会话：清空一切并 bump 世代。
    pub(crate) fn reset(&mut self, now_ms: i64) {
        self.generation = self.generation.wrapping_add(1);
        self.next_retry_at_ms = None;
        self.snapshot = SyncStateSnapshot {
            updated_at_ms: now_ms,
            ..SyncStateSnapshot::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 越过退避窗口再开跑，模拟「时间到了、某个触发源又来了」。
    fn begin_after_backoff(coordinator: &mut SyncCoordinator, kind: SyncRunKind, now_ms: i64) {
        let at = coordinator.next_retry_at_ms().unwrap_or(now_ms).max(now_ms);
        coordinator.begin(kind, at).expect("backoff should be over");
    }

    #[test]
    fn duplicate_begin_is_coalesced() {
        let mut coordinator = SyncCoordinator::new();
        assert!(coordinator.begin(SyncRunKind::Bootstrap, 1).is_ok());
        assert_eq!(
            coordinator.begin(SyncRunKind::Resume, 2),
            Err(SyncBeginRejection::AlreadyRunning)
        );
        assert_eq!(
            coordinator.snapshot().run_kind,
            Some(SyncRunKind::Bootstrap)
        );
    }

    #[test]
    fn recoverable_failures_increment_attempt_until_success() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Resume, 1).unwrap();
        coordinator.fail(
            SyncRunKind::Resume,
            false,
            Some(9),
            "transport".to_string(),
            2,
        );
        assert_eq!(coordinator.snapshot().phase, SyncPhase::Retrying);
        assert_eq!(coordinator.snapshot().attempt, 1);

        begin_after_backoff(&mut coordinator, SyncRunKind::Resume, 3);
        coordinator.fail(
            SyncRunKind::Resume,
            false,
            Some(9),
            "transport".to_string(),
            4_000,
        );
        assert_eq!(coordinator.snapshot().attempt, 2);

        begin_after_backoff(&mut coordinator, SyncRunKind::Resume, 5_000);
        coordinator.complete(SyncRunKind::Resume, 6_000);
        assert_eq!(coordinator.snapshot().phase, SyncPhase::Synced);
        assert_eq!(coordinator.snapshot().attempt, 0);
        assert_eq!(coordinator.next_retry_at_ms(), None);
    }

    #[test]
    fn terminal_failure_stops_retry_semantics() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Bootstrap, 1).unwrap();
        coordinator.fail(
            SyncRunKind::Bootstrap,
            true,
            Some(10_002),
            "token expired".to_string(),
            2,
        );
        assert_eq!(coordinator.snapshot().phase, SyncPhase::FailedTerminal);
        assert_eq!(coordinator.snapshot().attempt, 0);
        // 终态不排期重试，也不接受新一轮：它等的是一个显式动作。
        assert_eq!(coordinator.next_retry_at_ms(), None);
        assert_eq!(
            coordinator.begin(SyncRunKind::Resume, 3),
            Err(SyncBeginRejection::Terminal)
        );
    }

    /// 这条就是 2026-07-28 真机那个热循环的回归门禁。
    ///
    /// 当时的形状：触发源不停调 begin，每次都被放行、立刻失败、再被调用，
    /// 30 秒 120 次。现在同样「不停调」，但计数必须是个位数。
    #[test]
    fn a_hot_trigger_source_cannot_spin_the_coordinator() {
        let mut coordinator = SyncCoordinator::new();
        let mut now_ms = 0_i64;
        let mut runs = 0_u32;

        // 30 秒，每 50ms 来一个触发源（真机实测约 5 次/秒，这里更凶）。
        while now_ms < 30_000 {
            if coordinator.begin(SyncRunKind::Resume, now_ms).is_ok() {
                runs += 1;
                coordinator.fail(
                    SyncRunKind::Resume,
                    false,
                    Some(9),
                    "transport error: disconnected".to_string(),
                    now_ms,
                );
            }
            now_ms += 50;
        }

        // 1+2+4+8+16(s) → 30 秒内最多 6 轮。真机那次是 120。
        assert!(
            runs <= 8,
            "30s 内跑了 {runs} 轮，退避没有生效（热循环回归）"
        );
        assert!(runs >= 3, "退避过度，30s 只跑了 {runs} 轮");
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let mut coordinator = SyncCoordinator::new();
        let mut now_ms = 0_i64;
        let mut delays = Vec::new();
        for _ in 0..10 {
            coordinator.begin(SyncRunKind::Resume, now_ms).unwrap();
            coordinator.fail(SyncRunKind::Resume, false, None, "x".into(), now_ms);
            let deadline = coordinator.next_retry_at_ms().unwrap();
            delays.push(deadline - now_ms);
            now_ms = deadline;
        }
        assert!(delays[0] < delays[3], "退避没有增长: {delays:?}");
        assert!(
            delays.iter().all(|d| *d <= SYNC_RETRY_MAX_MS),
            "退避超过上限: {delays:?}"
        );
        assert!(
            delays.iter().all(|d| *d >= SYNC_RETRY_BASE_MS),
            "退避低于下限: {delays:?}"
        );
    }

    #[test]
    fn network_recovery_cancels_the_backoff_window() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Resume, 0).unwrap();
        coordinator.fail(SyncRunKind::Resume, false, None, "offline".into(), 0);
        // 退避中：拒绝。
        assert!(matches!(
            coordinator.begin(SyncRunKind::Resume, 10),
            Err(SyncBeginRejection::Backoff { .. })
        ));
        // 网络回来了，等待就没有意义了。
        coordinator.note_network_available();
        assert!(coordinator.begin(SyncRunKind::Resume, 11).is_ok());
    }

    #[test]
    fn reset_bumps_generation_and_clears_backoff() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Resume, 0).unwrap();
        coordinator.fail(SyncRunKind::Resume, false, None, "x".into(), 0);
        let gen_before = coordinator.generation();

        coordinator.reset(100);

        assert_eq!(coordinator.generation(), gen_before + 1);
        assert_eq!(coordinator.next_retry_at_ms(), None);
        assert_eq!(coordinator.snapshot().phase, SyncPhase::Idle);
        assert_eq!(coordinator.snapshot().attempt, 0);
        // 换账号之后必须能立刻开跑，不继承上一个账号的退避。
        assert!(coordinator.begin(SyncRunKind::Bootstrap, 101).is_ok());
    }

    /// 终态也要能被 reset 解开——换账号就是那个「显式动作」。
    #[test]
    fn reset_clears_terminal_failure() {
        let mut coordinator = SyncCoordinator::new();
        coordinator.begin(SyncRunKind::Resume, 0).unwrap();
        coordinator.fail(SyncRunKind::Resume, true, Some(10_002), "expired".into(), 0);
        assert_eq!(
            coordinator.begin(SyncRunKind::Resume, 1),
            Err(SyncBeginRejection::Terminal)
        );
        coordinator.reset(2);
        assert!(coordinator.begin(SyncRunKind::Bootstrap, 3).is_ok());
    }

    #[test]
    fn an_abandoned_run_does_not_wedge_the_gate() {
        let mut c = SyncCoordinator::new();
        assert!(c.begin(SyncRunKind::Bootstrap, 0).is_ok());
        // 跑到一半被打断
        c.abandon(10);
        // 闸门必须重新打开，否则后面每一个触发源都会被挡回去 = 同步永久卡死
        assert!(
            c.begin(SyncRunKind::Bootstrap, 20).is_ok(),
            "放弃一轮之后闸门没打开，同步会永久卡在 Running"
        );
    }

    #[test]
    fn an_abandoned_run_is_not_counted_as_a_failure() {
        let mut c = SyncCoordinator::new();
        assert!(c.begin(SyncRunKind::Resume, 0).is_ok());
        c.fail(SyncRunKind::Resume, false, None, "boom".into(), 0);
        let after_failure = c.snapshot().attempt;
        assert!(after_failure > 0);

        c.abandon(100);
        assert_eq!(
            c.snapshot().attempt,
            0,
            "被打断的一轮把 attempt/退避留给了下一个账号"
        );
        assert_eq!(c.snapshot().phase, SyncPhase::Idle);
    }
}
