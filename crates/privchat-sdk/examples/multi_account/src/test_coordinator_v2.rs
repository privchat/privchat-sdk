//! 测试协调器 V2 - 使用新的测试阶段

use crate::account_manager::MultiAccountManager;
use crate::test_phases_v2::TestPhasesV2;
use crate::types::{PhaseResult, TestResults};
use privchat_sdk::error::Result;
use std::time::Instant;
use tracing::{info, error};

pub struct TestCoordinatorV2 {
    results: Vec<PhaseResult>,
}

impl TestCoordinatorV2 {
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }
    
    /// 运行所有26个测试阶段
    pub async fn run_all_phases(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("\n");
        info!("═══════════════════════════════════════════════════════════");
        info!("🚀 PrivChat SDK 完整测试套件 V2 (26个阶段)");
        info!("═══════════════════════════════════════════════════════════");
        info!("\n");
        
        let start_time = Instant::now();
        
        // Phase 1: 用户认证
        self.run_phase(TestPhasesV2::phase1_authentication(account_manager).await).await;
        
        // Phase 2: 好友系统
        self.run_phase(TestPhasesV2::phase2_friend_system(account_manager).await).await;
        
        // Phase 3: 群组系统
        self.run_phase(TestPhasesV2::phase3_group_system(account_manager).await).await;
        
        // Phase 4: 混合场景
        self.run_phase(TestPhasesV2::phase4_mixed_scenarios(account_manager).await).await;
        
        // Phase 5: 消息接收
        self.run_phase(TestPhasesV2::phase5_message_reception(account_manager).await).await;
        
        // Phase 6: 表情包
        self.run_phase(TestPhasesV2::phase6_stickers(account_manager).await).await;
        
        // Phase 7: 会话管理
        self.run_phase(TestPhasesV2::phase7_channels(account_manager).await).await;
        
        // Phase 8: 已读回执
        self.run_phase(TestPhasesV2::phase8_read_receipts(account_manager).await).await;
        
        // Phase 9: 文件上传
        self.run_phase(TestPhasesV2::phase9_file_upload(account_manager).await).await;
        
        // Phase 10: 特殊消息
        self.run_phase(TestPhasesV2::phase10_special_messages(account_manager).await).await;
        
        // Phase 11: 消息历史
        self.run_phase(TestPhasesV2::phase11_message_history(account_manager).await).await;
        
        // Phase 12: 消息撤回
        self.run_phase(TestPhasesV2::phase12_message_revoke(account_manager).await).await;
        
        // Phase 13: 离线消息
        self.run_phase(TestPhasesV2::phase13_offline_messages(account_manager).await).await;
        
        // Phase 14: PTS同步
        self.run_phase(TestPhasesV2::phase14_pts_sync(account_manager).await).await;
        
        // Phase 15: 高级群组
        self.run_phase(TestPhasesV2::phase15_advanced_group(account_manager).await).await;
        
        // Phase 16: 消息回复
        self.run_phase(TestPhasesV2::phase16_message_reply(account_manager).await).await;
        
        // Phase 17: 消息反应
        self.run_phase(TestPhasesV2::phase17_reactions(account_manager).await).await;
        
        // Phase 18: 黑名单
        self.run_phase(TestPhasesV2::phase18_blacklist(account_manager).await).await;
        
        // Phase 19: @提及
        self.run_phase(TestPhasesV2::phase19_mentions(account_manager).await).await;
        
        // Phase 20: 非好友消息
        self.run_phase(TestPhasesV2::phase20_stranger_messages(account_manager).await).await;
        
        // Phase 21: 在线状态
        self.run_phase(TestPhasesV2::phase21_online_presence(account_manager).await).await;
        
        // Phase 22: 输入状态
        self.run_phase(TestPhasesV2::phase22_typing_indicator(account_manager).await).await;
        
        // Phase 23: 系统通知
        self.run_phase(TestPhasesV2::phase23_system_notifications(account_manager).await).await;
        
        // Phase 24: 在线状态管理（使用代理方法）
        self.run_phase(TestPhasesV2::phase21_presence_system(account_manager).await).await;
        
        // Phase 25: 统计信息汇总
        self.run_phase(TestPhasesV2::phase22_statistics(account_manager).await).await;
        
        // Phase 26: 登录功能测试
        self.run_phase(TestPhasesV2::phase26_login_test(account_manager).await).await;
        
        let total_duration = start_time.elapsed();
        
        // 生成最终报告
        self.generate_report(total_duration);
        
        Ok(())
    }
    
    /// 运行单个阶段
    async fn run_phase(&mut self, result: Result<PhaseResult>) {
        match result {
            Ok(phase_result) => {
                let status = if phase_result.success { "✅" } else { "❌" };
                info!("{} Phase {}: {} ({}ms)", 
                      status,
                      self.results.len() + 1,
                      phase_result.phase_name,
                      phase_result.duration.as_millis());
                
                if !phase_result.metrics.errors.is_empty() {
                    for error in &phase_result.metrics.errors {
                        error!("   ⚠️  {}", error);
                    }
                }
                
                self.results.push(phase_result);
            }
            Err(e) => {
                error!("❌ Phase {} 执行失败: {}", self.results.len() + 1, e);
                self.results.push(PhaseResult {
                    phase_name: format!("Phase {}", self.results.len() + 1),
                    success: false,
                    duration: std::time::Duration::from_secs(0),
                    details: format!("执行失败: {}", e),
                    metrics: Default::default(),
                });
            }
        }
    }
    
    /// 生成最终测试报告
    fn generate_report(&self, total_duration: std::time::Duration) {
        info!("\n");
        info!("═══════════════════════════════════════════════════════════");
        info!("📊 测试报告总结");
        info!("═══════════════════════════════════════════════════════════");
        
        let total_phases = self.results.len();
        let successful_phases = self.results.iter().filter(|r| r.success).count();
        let failed_phases = total_phases - successful_phases;
        
        let total_messages: u32 = self.results.iter()
            .map(|r| r.metrics.messages_sent)
            .sum();
        
        let total_rpc_calls: u32 = self.results.iter()
            .map(|r| r.metrics.rpc_calls)
            .sum();
        
        let successful_rpc: u32 = self.results.iter()
            .map(|r| r.metrics.rpc_successes)
            .sum();
        
        info!("\n📈 总体统计:");
        info!("   • 总阶段数: {}", total_phases);
        info!("   • 成功: {} ✅", successful_phases);
        info!("   • 失败: {} ❌", failed_phases);
        info!("   • 成功率: {:.1}%", (successful_phases as f64 / total_phases as f64) * 100.0);
        info!("   • 总耗时: {:.2}s", total_duration.as_secs_f64());
        
        info!("\n📊 操作统计:");
        info!("   • 发送消息: {}", total_messages);
        info!("   • RPC调用: {}/{}", successful_rpc, total_rpc_calls);
        
        info!("\n📋 各阶段详情:");
        for (i, result) in self.results.iter().enumerate() {
            let status = if result.success { "✅" } else { "❌" };
            info!("   {} Phase {}: {} ({}ms) - {}",
                  status,
                  i + 1,
                  result.phase_name,
                  result.duration.as_millis(),
                  result.details);
        }
        
        if failed_phases > 0 {
            info!("\n⚠️  失败阶段详情:");
            for (i, result) in self.results.iter().enumerate() {
                if !result.success {
                    info!("   Phase {}: {}", i + 1, result.phase_name);
                    for error in &result.metrics.errors {
                        info!("      • {}", error);
                    }
                }
            }
        }
        
        info!("\n═══════════════════════════════════════════════════════════");
        if failed_phases == 0 {
            info!("🎉 所有测试阶段全部通过！");
        } else {
            info!("⚠️  部分测试阶段失败，请检查日志");
        }
        info!("═══════════════════════════════════════════════════════════\n");
    }
    
    /// 获取测试结果
    pub fn get_results(&self) -> TestResults {
        let total_phases = self.results.len();
        let successful_phases = self.results.iter().filter(|r| r.success).count();
        
        TestResults {
            total_phases,
            successful_phases,
            failed_phases: total_phases - successful_phases,
            total_duration: self.results.iter()
                .map(|r| r.duration)
                .sum(),
            phase_results: self.results.clone(),
        }
    }
}
