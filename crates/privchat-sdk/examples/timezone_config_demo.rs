//! 时区配置示例
//! 
//! 展示如何在 SDK 中配置时区，以及时间戳如何自动转换

use privchat_sdk::{PrivchatConfig, TimeFormatter, TimezoneConfig};
use chrono::Utc;

#[tokio::main]
async fn main() {
    println!("=== PrivChat SDK 时区配置示例 ===\n");
    
    // 示例 1: 配置 SDK 使用特定时区（东八区 UTC+8）
    println!("📋 示例 1: 配置 SDK 使用 UTC+8 时区");
    let config = PrivchatConfig::builder()
        .data_dir("./test_data")
        .timezone_hours(8)  // 设置为东八区
        .build();
    
    println!("   配置的时区偏移: {:?} 秒\n", config.timezone_offset_seconds);
    
    // 模拟 SDK 初始化时应用时区配置
    if let Some(offset_seconds) = config.timezone_offset_seconds {
        let tz_config = TimezoneConfig { offset_seconds };
        TimeFormatter::set_timezone(tz_config);
        println!("   ✅ 已设置时区: UTC{:+}", offset_seconds / 3600);
    }
    
    // 示例 2: 存储和读取消息时间戳
    println!("\n📋 示例 2: 存储和读取消息时间戳");
    
    // 业务层：生成 UTC 时间戳（存储到数据库）
    let utc_timestamp = Utc::now().timestamp_millis();
    println!("   存储的 UTC 时间戳: {}", utc_timestamp);
    
    // 显示层：自动转换为配置的时区显示
    let display_time = TimeFormatter::format_standard(utc_timestamp);
    println!("   显示时间（UTC+8）: {}", display_time);
    
    let display_time_short = TimeFormatter::format_short(utc_timestamp);
    println!("   显示时间（简短）: {}", display_time_short);
    println!();
    
    // 示例 3: 日志输出自动转换
    println!("📋 示例 3: 日志输出自动转换");
    println!("   使用 fmt_timestamp! 宏:");
    println!("   标准格式: {}", fmt_timestamp!(utc_timestamp));
    println!("   简短格式: {}", fmt_timestamp!(utc_timestamp, short));
    println!("   仅日期: {}", fmt_timestamp!(utc_timestamp, date));
    println!("   仅时间: {}", fmt_timestamp!(utc_timestamp, time));
    println!();
    
    // 示例 4: 切换时区
    println!("📋 示例 4: 动态切换时区");
    
    // 切换到纽约时区 (UTC-5)
    TimeFormatter::set_timezone(TimezoneConfig::from_hours(-5));
    println!("   切换到 UTC-5 (纽约时区)");
    println!("   显示时间: {}", TimeFormatter::format_standard(utc_timestamp));
    
    // 切换到东京时区 (UTC+9)
    TimeFormatter::set_timezone(TimezoneConfig::from_hours(9));
    println!("   切换到 UTC+9 (东京时区)");
    println!("   显示时间: {}", TimeFormatter::format_standard(utc_timestamp));
    
    // 切换回 UTC+8
    TimeFormatter::set_timezone(TimezoneConfig::from_hours(8));
    println!("   切换回 UTC+8");
    println!("   显示时间: {}", TimeFormatter::format_standard(utc_timestamp));
    println!();
    
    // 示例 5: 时间计算（相对时间）
    println!("📋 示例 5: 时间计算");
    
    let five_min_ago = utc_timestamp - (5 * 60 * 1000);
    println!("   5分钟前的时间戳: {}", five_min_ago);
    println!("   显示: {}", TimeFormatter::format_standard(five_min_ago));
    println!("   距现在: {} 分钟", TimeFormatter::minutes_since(five_min_ago));
    println!();
    
    let two_hours_ago = utc_timestamp - (2 * 60 * 60 * 1000);
    println!("   2小时前的时间戳: {}", two_hours_ago);
    println!("   显示: {}", TimeFormatter::format_standard(two_hours_ago));
    println!("   距现在: {} 小时", TimeFormatter::hours_since(two_hours_ago));
    println!();
    
    // 示例 6: 判断时间关系
    println!("📋 示例 6: 判断时间关系");
    
    let is_today = TimeFormatter::is_today(utc_timestamp);
    println!("   当前时间是否是今天: {}", is_today);
    
    let yesterday_ts = utc_timestamp - (24 * 60 * 60 * 1000);
    let is_yesterday = TimeFormatter::is_yesterday(yesterday_ts);
    println!("   昨天的时间是否是昨天: {}", is_yesterday);
    
    let same_day = TimeFormatter::is_same_day(utc_timestamp, five_min_ago);
    println!("   5分钟前和现在是否同一天: {}", same_day);
    println!();
    
    // 示例 7: 用户输入时间转换
    println!("📋 示例 7: 用户输入时间转换（本地 -> UTC）");
    
    // 用户在 UI 输入的时间（假设是 UTC+8 时区的 2024-01-17 14:00:00）
    let user_input = "2024-01-17 14:00:00";
    println!("   用户输入（UTC+8）: {}", user_input);
    
    if let Some(utc_ts) = TimeFormatter::parse_to_utc_timestamp(user_input) {
        println!("   转换为 UTC 时间戳: {}", utc_ts);
        println!("   验证（转回显示）: {}", TimeFormatter::format_standard(utc_ts));
    }
    println!();
    
    // 示例 8: 完整的 SDK 配置示例
    println!("📋 示例 8: 完整的 SDK 配置");
    
    let configs = vec![
        ("UTC+8（北京、上海）", 8),
        ("UTC+9（东京、首尔）", 9),
        ("UTC-5（纽约）", -5),
        ("UTC-8（洛杉矶）", -8),
        ("UTC+0（伦敦）", 0),
    ];
    
    for (name, hours) in configs {
        let config = PrivchatConfig::builder()
            .data_dir("./test_data")
            .timezone_hours(hours)
            .build();
        
        if let Some(offset) = config.timezone_offset_seconds {
            println!("   {} -> offset_seconds: {} ({:+} 小时)", 
                name, offset, offset / 3600);
        }
    }
    
    println!("\n✅ 使用说明:");
    println!("   1. 在 SDK 配置中设置 timezone_hours/timezone_minutes/timezone_seconds");
    println!("   2. SDK 初始化时会自动应用时区配置");
    println!("   3. 所有时间戳在存储和传输时仍然是 UTC");
    println!("   4. TimeFormatter 自动根据配置的时区转换显示");
    println!("   5. 应用层使用 TimeFormatter 的方法进行国际化显示");
}
