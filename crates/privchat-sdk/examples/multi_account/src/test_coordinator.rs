//! 测试协调器 - 统一管理测试执行和报告

use crate::account_manager::MultiAccountManager;
use crate::realistic_test_phases::RealisticTestPhases;
use crate::types::{TestResults, PhaseResult, TestConfig};
use privchat_sdk::error::Result;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{info, warn, error};

/// 测试协调器
pub struct TestCoordinator {
    test_phases: RealisticTestPhases,
    test_results: TestResults,
    config: TestConfig,
}

impl TestCoordinator {
    /// 创建新的测试协调器
    pub fn new() -> Self {
        let config = TestConfig::default();
        let test_phases = RealisticTestPhases::new(config.clone());
        
        Self {
            test_phases,
            test_results: TestResults::default(),
            config,
        }
    }
    
    /// 使用自定义配置创建测试协调器
    pub fn with_config(config: TestConfig) -> Self {
        let test_phases = RealisticTestPhases::new(config.clone());
        
        Self {
            test_phases,
            test_results: TestResults::default(),
            config,
        }
    }
    
    /// 运行所有测试
    pub async fn run_all_tests(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🚀 开始执行完整的多账号测试流程");
        
        let overall_start = Instant::now();
        
        // Phase 1: 并发认证测试
        self.run_phase_1(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 2: 交叉私聊测试
        self.run_phase_2(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 3: RPC 功能测试
        self.run_phase_3(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 4: 群组协作测试
        self.run_phase_4(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 5: 消息接收验证测试
        self.run_phase_5(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 6: 表情包功能测试
        self.run_phase_6(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 7: 会话列表和置顶测试
        self.run_phase_7(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 8: 已读回执测试
        self.run_phase_8(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 9: 文件上传测试
        self.run_phase_9(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 10: 其他消息类型测试（位置、名片）
        self.run_phase_10(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 11: 消息历史查询测试
        self.run_phase_11(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 12: 消息撤回测试
        self.run_phase_12(account_manager).await?;
        self.inter_phase_delay().await;
        
        // Phase 13: 离线消息推送测试
        self.run_phase_13(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 14: pts 同步和离线消息队列限制测试
        self.run_phase_14(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 15: 高级群组功能测试
        self.run_phase_15(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 18: 黑名单测试
        self.run_phase_18(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 16: 消息引用/回复测试
        self.run_phase_16(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 17: Reaction 测试
        self.run_phase_17(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 19: @提及测试
        self.run_phase_19(account_manager).await?;
        self.inter_phase_delay().await;
        
        // ✨ Phase 20: 非好友消息测试
        self.run_phase_20(account_manager).await?;
        self.inter_phase_delay().await;
        
        // 测试完成，共20个阶段
        
        let total_duration = overall_start.elapsed();
        
        // 生成最终报告
        self.generate_final_report(account_manager, total_duration).await;
        
        info!("🎉 所有测试阶段完成！总用时: {}ms", total_duration.as_millis());
        
        Ok(())
    }
    

    
    /// 阶段间延迟
    async fn inter_phase_delay(&self) {
        if self.config.phase_delay > Duration::from_millis(0) {
            info!("⏳ 阶段间延迟 {}ms", self.config.phase_delay.as_millis());
            sleep(self.config.phase_delay).await;
        }
    }
    
    /// 生成最终测试报告
    async fn generate_final_report(
        &mut self,
        account_manager: &mut MultiAccountManager,
        total_duration: Duration,
    ) {
        println!("\n{}", "=".repeat(60));
        println!("📊 多账号协作测试完整报告");
        println!("{}", "=".repeat(60));
        
        // 总体统计
        println!("\n📈 总体统计:");
        println!("   🕒 总执行时间: {} ms", total_duration.as_millis());
        println!("   📋 测试阶段数: {}", self.test_results.total_tests);
        println!("   ✅ 成功阶段: {}", self.test_results.passed_tests);
        println!("   ❌ 失败阶段: {}", self.test_results.failed_tests);
        
        let success_rate = if self.test_results.total_tests > 0 {
            (self.test_results.passed_tests as f64 / self.test_results.total_tests as f64) * 100.0
        } else {
            0.0
        };
        println!("   📊 成功率: {:.1}%", success_rate);
        
        // 消息和RPC统计
        let mut total_messages_sent = 0;
        let mut total_rpc_calls = 0;
        let mut total_rpc_successes = 0;
        let mut total_errors = 0;
        
        for phase in &self.test_results.phase_results {
            total_messages_sent += phase.metrics.messages_sent;
            total_rpc_calls += phase.metrics.rpc_calls;
            total_rpc_successes += phase.metrics.rpc_successes;
            total_errors += phase.metrics.errors.len() as u32;
        }
        
        println!("\n📨 通信统计:");
        println!("   📤 消息发送: {} 条", total_messages_sent);
        println!("   🔧 RPC 调用: {} 次", total_rpc_calls);
        println!("   ✅ RPC 成功: {} 次", total_rpc_successes);
        println!("   ❌ 错误总数: {} 个", total_errors);
        
        if total_rpc_calls > 0 {
            let rpc_success_rate = (total_rpc_successes as f64 / total_rpc_calls as f64) * 100.0;
            println!("   📊 RPC成功率: {:.1}%", rpc_success_rate);
        }
        
        // 阶段详情
        println!("\n📋 阶段执行详情:");
        for (i, phase) in self.test_results.phase_results.iter().enumerate() {
            let status = if phase.success { "✅" } else { "❌" };
            println!("   {}. {} {}: {}ms", 
                     i + 1, status, phase.phase_name, phase.duration.as_millis());
            println!("      详情: {}", phase.details);
            
            if phase.metrics.messages_sent > 0 {
                println!("      消息: {} 条", phase.metrics.messages_sent);
            }
            if phase.metrics.rpc_calls > 0 {
                println!("      RPC: {}/{} 成功", phase.metrics.rpc_successes, phase.metrics.rpc_calls);
            }
            if !phase.metrics.errors.is_empty() {
                println!("      错误: {} 个", phase.metrics.errors.len());
                for error in &phase.metrics.errors {
                    println!("        • {}", error);
                }
            }
        }
        
        // 账号状态报告
        println!("\n👥 账号状态:");
        let account_status = account_manager.generate_status_report().await;
        for line in account_status.lines() {
            if !line.trim().is_empty() && !line.contains("账号状态报告") && !line.contains("===") {
                println!("   {}", line);
            }
        }
        
        // 事件统计
        println!("\n📊 事件统计:");
        let event_report = account_manager.get_event_bus().generate_event_report();
        for line in event_report.lines() {
            if !line.trim().is_empty() && !line.contains("事件统计报告") && !line.contains("===") {
                println!("   {}", line);
            }
        }
        
        // 功能验证总结
        println!("\n🎯 功能验证总结:");
        
        let mut verified_features = Vec::new();
        let mut failed_features = Vec::new();
        
        for phase in &self.test_results.phase_results {
            if phase.success {
                match phase.phase_name.as_str() {
                    "并发认证" | "用户认证" => verified_features.push("✅ 多账号并发认证"),
                    "好友系统流程" => verified_features.push("✅ 好友系统完整流程"),
                    "群组系统流程" => verified_features.push("✅ 群组系统完整流程"),
                    "混合场景测试" => verified_features.push("✅ 混合场景测试"),
                    "消息接收验证" => verified_features.push("✅ 消息接收验证"),
                    "表情包功能" => verified_features.push("✅ 表情包管理功能"),
                    "会话列表和置顶" => verified_features.push("✅ 会话列表和置顶功能"),
                    "已读回执" => verified_features.push("✅ 已读回执功能"),
                    "文件上传" => verified_features.push("✅ 文件上传流程"),
                    "消息撤回" => verified_features.push("✅ 消息撤回功能"),
                    "离线消息推送" => verified_features.push("✅ 离线消息推送和历史消息获取"),
                    _ => {}
                }
            } else {
                match phase.phase_name.as_str() {
                    "用户认证" | "并发认证" => failed_features.push("❌ 多账号并发认证"),
                    "好友系统流程" => failed_features.push("❌ 好友系统完整流程"),
                    "群组系统流程" => failed_features.push("❌ 群组系统完整流程"),
                    "混合场景测试" => failed_features.push("❌ 混合场景测试"),
                    "消息接收验证" => failed_features.push("❌ 消息接收验证"),
                    "表情包功能" => failed_features.push("❌ 表情包管理功能"),
                    "会话列表和置顶" => failed_features.push("❌ 会话列表和置顶功能"),
                    "已读回执" => failed_features.push("❌ 已读回执功能"),
                    "文件上传" => failed_features.push("❌ 文件上传流程"),
                    "消息撤回" => failed_features.push("❌ 消息撤回功能"),
                    "离线消息推送" => failed_features.push("❌ 离线消息推送和历史消息获取"),
                    _ => {}
                }
            }
        }
        
        for feature in verified_features {
            println!("   {}", feature);
        }
        
        if !failed_features.is_empty() {
            println!("\n⚠️  未通过的功能:");
            for feature in failed_features {
                println!("   {}", feature);
            }
        }
        
        // 测试结论
        println!("\n🏆 测试结论:");
        if success_rate >= 80.0 {
            println!("   🎉 多账号协作功能测试 PASSED!");
            println!("   📝 系统具备完整的多账号协作能力");
        } else if success_rate >= 60.0 {
            println!("   ⚠️  多账号协作功能测试 PARTIAL PASS");
            println!("   📝 系统基本功能正常，存在部分问题");
        } else {
            println!("   ❌ 多账号协作功能测试 FAILED");
            println!("   📝 系统存在重大问题，需要修复");
        }
        
        println!("\n{}", "=".repeat(60));
    }
    
    /// 获取测试结果
    pub fn get_test_results(&self) -> &TestResults {
        &self.test_results
    }
    
    /// 获取测试配置
    pub fn get_config(&self) -> &TestConfig {
        &self.config
    }
    
    // 各个阶段的独立执行方法
    async fn run_phase_1(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        let mut attempts = 0;
        let max_attempts = self.config.max_retries;
        
        loop {
            attempts += 1;
            info!("🔄 执行 Phase 1 (尝试 {}/{})", attempts, max_attempts);
            
            match self.test_phases.phase1_user_authentication(account_manager).await {
                Ok(result) => {
                    if result.success {
                        info!("✅ Phase 1 成功完成");
                        self.test_results.phase_results.push(result);
                        self.test_results.passed_tests += 1;
                        break;
                    } else {
                        warn!("⚠️ Phase 1 执行失败: {}", result.details);
                        if attempts >= max_attempts {
                            error!("❌ Phase 1 重试次数已用完");
                            self.test_results.phase_results.push(result);
                            self.test_results.failed_tests += 1;
                            break;
                        } else {
                            sleep(self.config.phase_delay).await;
                        }
                    }
                }
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    } else {
                        sleep(self.config.phase_delay).await;
                    }
                }
            }
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_2(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        let mut attempts = 0;
        let max_attempts = self.config.max_retries;
        
        loop {
            attempts += 1;
            info!("🔄 执行 Phase 2 (尝试 {}/{})", attempts, max_attempts);
            
            match self.test_phases.phase2_friend_system_workflow(account_manager).await {
                Ok(result) => {
                    if result.success {
                        info!("✅ Phase 2 成功完成");
                        self.test_results.phase_results.push(result);
                        self.test_results.passed_tests += 1;
                        break;
                    } else {
                        warn!("⚠️ Phase 2 执行失败: {}", result.details);
                        if attempts >= max_attempts {
                            error!("❌ Phase 2 重试次数已用完");
                            self.test_results.phase_results.push(result);
                            self.test_results.failed_tests += 1;
                            break;
                        } else {
                            sleep(self.config.phase_delay).await;
                        }
                    }
                }
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    } else {
                        sleep(self.config.phase_delay).await;
                    }
                }
            }
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_3(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        let mut attempts = 0;
        let max_attempts = self.config.max_retries;
        
        loop {
            attempts += 1;
            info!("🔄 执行 Phase 3 (尝试 {}/{})", attempts, max_attempts);
            
            match self.test_phases.phase3_group_system_workflow(account_manager).await {
                Ok(result) => {
                    if result.success {
                        info!("✅ Phase 3 成功完成");
                        self.test_results.phase_results.push(result);
                        self.test_results.passed_tests += 1;
                        break;
                    } else {
                        warn!("⚠️ Phase 3 执行失败: {}", result.details);
                        if attempts >= max_attempts {
                            error!("❌ Phase 3 重试次数已用完");
                            self.test_results.phase_results.push(result);
                            self.test_results.failed_tests += 1;
                            break;
                        } else {
                            sleep(self.config.phase_delay).await;
                        }
                    }
                }
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    } else {
                        sleep(self.config.phase_delay).await;
                    }
                }
            }
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_4(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        let mut attempts = 0;
        let max_attempts = self.config.max_retries;
        
        loop {
            attempts += 1;
            info!("🔄 执行 Phase 4 (尝试 {}/{})", attempts, max_attempts);
            
            match self.test_phases.phase4_mixed_scenarios(account_manager).await {
                Ok(result) => {
                    if result.success {
                        info!("✅ Phase 4 成功完成");
                        self.test_results.phase_results.push(result);
                        self.test_results.passed_tests += 1;
                        break;
                    } else {
                        warn!("⚠️ Phase 4 执行失败: {}", result.details);
                        if attempts >= max_attempts {
                            error!("❌ Phase 4 重试次数已用完");
                            self.test_results.phase_results.push(result);
                            self.test_results.failed_tests += 1;
                            break;
                        } else {
                            sleep(self.config.phase_delay).await;
                        }
                    }
                }
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    } else {
                        sleep(self.config.phase_delay).await;
                    }
                }
            }
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_5(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        let mut attempts = 0;
        let max_attempts = self.config.max_retries;
        
        loop {
            attempts += 1;
            info!("🔄 执行 Phase 5 (尝试 {}/{})", attempts, max_attempts);
            
            match self.test_phases.phase5_message_receiving(account_manager).await {
                Ok(result) => {
                    if result.success {
                        info!("✅ Phase 5 成功完成");
                        self.test_results.phase_results.push(result);
                        self.test_results.passed_tests += 1;
                        break;
                    } else {
                        warn!("⚠️ Phase 5 执行失败: {}", result.details);
                        if attempts >= max_attempts {
                            error!("❌ Phase 5 重试次数已用完");
                            self.test_results.phase_results.push(result);
                            self.test_results.failed_tests += 1;
                            break;
                        } else {
                            sleep(self.config.phase_delay).await;
                        }
                    }
                }
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    } else {
                        sleep(self.config.phase_delay).await;
                    }
                }
            }
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_6(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 6: 表情包功能测试");
        match self.test_phases.phase6_sticker_features(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 6 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 6 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 6 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_7(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 7: 会话列表和置顶");
        match self.test_phases.phase7_channel_features(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 7 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 7 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 7 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_8(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 8: 已读回执测试");
        match self.test_phases.phase8_read_receipts(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 8 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 8 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 8 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_9(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 9: 文件上传测试");
        match self.test_phases.phase9_file_upload(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 9 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 9 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 9 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_10(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 10: 其他消息类型测试");
        match self.test_phases.phase10_other_message_types(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 10 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 10 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 10 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_11(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 11: 消息历史查询测试");
        match self.test_phases.phase11_message_history(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 11 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 11 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 11 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_12(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 12: 消息撤回测试");
        match self.test_phases.phase12_message_revoke(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 12 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 12 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 12 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_13(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 13: 离线消息推送测试");
        match self.test_phases.phase13_offline_message_push(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 13 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 13 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 13 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_14(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 14: pts 同步和离线消息队列限制测试");
        match self.test_phases.phase14_pts_sync_and_queue_limit(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 14 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 14 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 14 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_15(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 15: 高级群组功能测试");
        match self.test_phases.phase15_advanced_group_features(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 15 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 15 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 15 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_18(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 18: 黑名单测试");
        match self.test_phases.phase18_blacklist_test(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 18 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 18 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 18 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_16(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 16: 消息引用/回复测试");
        match self.test_phases.phase16_message_reply(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 16 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 16 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 16 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_17(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 17: Reaction 测试");
        match self.test_phases.phase17_message_reaction(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 17 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 17 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 17 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_19(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 19: @提及测试");
        match self.test_phases.phase19_mention_test(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 19 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 19 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 19 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }
    
    async fn run_phase_20(&mut self, account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🔄 执行 Phase 20: 非好友消息测试");
        match self.test_phases.phase20_non_friend_message(account_manager).await {
            Ok(result) => {
                if result.success {
                    info!("✅ Phase 20 成功完成");
                    self.test_results.passed_tests += 1;
                } else {
                    warn!("⚠️ Phase 20 执行失败: {}", result.details);
                    self.test_results.failed_tests += 1;
                }
                self.test_results.phase_results.push(result);
            }
            Err(e) => error!("❌ Phase 20 执行错误: {}", e),
        }
        self.test_results.total_tests += 1;
        Ok(())
    }

}