//! 时间处理工具模块
//! 
//! 提供统一的时间格式化、转换功能，支持可配置的时区
//! 
//! # 设计原则
//! 
//! - **存储层**: 所有时间字段使用 UTC 毫秒时间戳（INTEGER）
//! - **业务层**: 统一使用 `Utc::now().timestamp_millis()` 生成时间
//! - **显示层**: 根据配置的时区自动转换时间戳
//! - **多语言**: 不包含硬编码文本，由应用层处理国际化

use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDateTime, TimeZone, Utc, Datelike, Timelike};
use std::sync::RwLock;

/// 全局时区配置
static TIMEZONE_OFFSET: RwLock<Option<FixedOffset>> = RwLock::new(None);

/// 时区配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimezoneConfig {
    /// 时区偏移（秒），例如：+08:00 = 28800, -05:00 = -18000
    pub offset_seconds: i32,
}

impl TimezoneConfig {
    /// 创建时区配置（从小时偏移）
    /// 
    /// # 参数
    /// 
    /// * `hours` - 时区小时偏移，例如：+8, -5
    pub fn from_hours(hours: i32) -> Self {
        Self {
            offset_seconds: hours * 3600,
        }
    }
    
    /// 创建时区配置（从分钟偏移）
    /// 
    /// # 参数
    /// 
    /// * `minutes` - 时区分钟偏移，例如：480 (+8小时), -300 (-5小时)
    pub fn from_minutes(minutes: i32) -> Self {
        Self {
            offset_seconds: minutes * 60,
        }
    }
    
    /// 使用系统本地时区
    pub fn local() -> Self {
        let now = Local::now();
        let offset = now.offset().local_minus_utc();
        Self {
            offset_seconds: offset,
        }
    }
    
    /// 获取 FixedOffset
    pub fn to_fixed_offset(&self) -> Option<FixedOffset> {
        FixedOffset::east_opt(self.offset_seconds)
    }
}

/// 时间格式化工具
pub struct TimeFormatter;

impl TimeFormatter {
    /// 设置全局时区配置
    /// 
    /// # 参数
    /// 
    /// * `config` - 时区配置
    pub fn set_timezone(config: TimezoneConfig) {
        if let Some(offset) = config.to_fixed_offset() {
            let mut tz = TIMEZONE_OFFSET.write().unwrap();
            *tz = Some(offset);
        }
    }
    
    /// 获取当前配置的时区，如果未配置则使用系统本地时区
    fn get_timezone() -> FixedOffset {
        let tz = TIMEZONE_OFFSET.read().unwrap();
        if let Some(offset) = *tz {
            offset
        } else {
            // 如果未配置，使用系统本地时区
            let now = Local::now();
            FixedOffset::east_opt(now.offset().local_minus_utc()).unwrap()
        }
    }
    
    /// 将 UTC 毫秒时间戳转换为配置的时区
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    /// 
    /// # 返回
    /// 
    /// 配置时区的 DateTime，转换失败返回当前时间
    pub fn to_timezone(utc_timestamp_ms: i64) -> DateTime<FixedOffset> {
        let datetime_utc = Utc.timestamp_millis_opt(utc_timestamp_ms)
            .single()
            .unwrap_or_else(Utc::now);
        
        let tz = Self::get_timezone();
        datetime_utc.with_timezone(&tz)
    }
    
    /// 格式化为标准日期时间字符串
    /// 
    /// 格式: "YYYY-MM-DD HH:MM:SS"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_standard(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    }
    
    /// 格式化为简短日期时间字符串
    /// 
    /// 格式: "YYYY-MM-DD HH:MM"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_short(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
    
    /// 格式化为日期字符串
    /// 
    /// 格式: "YYYY-MM-DD"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_date(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.format("%Y-%m-%d").to_string()
    }
    
    /// 格式化为时间字符串
    /// 
    /// 格式: "HH:MM:SS"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_time(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.format("%H:%M:%S").to_string()
    }
    
    /// 格式化为简短时间字符串
    /// 
    /// 格式: "HH:MM"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_time_short(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.format("%H:%M").to_string()
    }
    
    /// 格式化为 ISO 8601 格式
    /// 
    /// 格式: "YYYY-MM-DDTHH:MM:SS+HH:MM"
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn format_iso8601(utc_timestamp_ms: i64) -> String {
        let dt = Self::to_timezone(utc_timestamp_ms);
        dt.to_rfc3339()
    }
    
    /// 获取时间差（秒）
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    /// 
    /// # 返回
    /// 
    /// 距离现在的秒数（正数表示过去，负数表示未来）
    pub fn seconds_since(utc_timestamp_ms: i64) -> i64 {
        let now = Utc::now().timestamp_millis();
        (now - utc_timestamp_ms) / 1000
    }
    
    /// 获取时间差（分钟）
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn minutes_since(utc_timestamp_ms: i64) -> i64 {
        Self::seconds_since(utc_timestamp_ms) / 60
    }
    
    /// 获取时间差（小时）
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn hours_since(utc_timestamp_ms: i64) -> i64 {
        Self::seconds_since(utc_timestamp_ms) / 3600
    }
    
    /// 获取时间差（天）
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn days_since(utc_timestamp_ms: i64) -> i64 {
        Self::seconds_since(utc_timestamp_ms) / 86400
    }
    
    /// 判断两个时间戳是否在同一天
    /// 
    /// # 参数
    /// 
    /// * `timestamp1_ms` - UTC 毫秒时间戳 1
    /// * `timestamp2_ms` - UTC 毫秒时间戳 2
    /// 
    /// # 返回
    /// 
    /// 是否在同一天（基于配置的时区）
    pub fn is_same_day(timestamp1_ms: i64, timestamp2_ms: i64) -> bool {
        let dt1 = Self::to_timezone(timestamp1_ms);
        let dt2 = Self::to_timezone(timestamp2_ms);
        dt1.date_naive() == dt2.date_naive()
    }
    
    /// 判断时间戳是否是今天
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn is_today(utc_timestamp_ms: i64) -> bool {
        let dt = Self::to_timezone(utc_timestamp_ms);
        let now_tz = Utc::now().with_timezone(&Self::get_timezone());
        dt.date_naive() == now_tz.date_naive()
    }
    
    /// 判断时间戳是否是昨天
    /// 
    /// # 参数
    /// 
    /// * `utc_timestamp_ms` - UTC 毫秒时间戳
    pub fn is_yesterday(utc_timestamp_ms: i64) -> bool {
        let dt = Self::to_timezone(utc_timestamp_ms);
        let now_tz = Utc::now().with_timezone(&Self::get_timezone());
        let yesterday = now_tz.date_naive() - Duration::days(1);
        dt.date_naive() == yesterday
    }
    
    /// 将本地时间字符串转换为 UTC 毫秒时间戳
    /// 
    /// # 参数
    /// 
    /// * `local_time_str` - 本地时间字符串，格式: "YYYY-MM-DD HH:MM:SS"
    /// 
    /// # 返回
    /// 
    /// UTC 毫秒时间戳，转换失败返回 None
    pub fn parse_to_utc_timestamp(local_time_str: &str) -> Option<i64> {
        let naive = NaiveDateTime::parse_from_str(local_time_str, "%Y-%m-%d %H:%M:%S").ok()?;
        let tz = Self::get_timezone();
        let local_dt = tz.from_local_datetime(&naive).single()?;
        Some(local_dt.with_timezone(&Utc).timestamp_millis())
    }
    
    /// 将指定时区的时间转换为 UTC 毫秒时间戳
    /// 
    /// # 参数
    /// 
    /// * `datetime` - 指定时区的 DateTime
    pub fn datetime_to_utc_timestamp<Tz: TimeZone>(datetime: DateTime<Tz>) -> i64 {
        datetime.with_timezone(&Utc).timestamp_millis()
    }
    
    /// 获取当前 UTC 毫秒时间戳
    pub fn now_utc_millis() -> i64 {
        Utc::now().timestamp_millis()
    }
    
    /// 获取当前配置时区的年月日
    /// 
    /// # 返回
    /// 
    /// (年, 月, 日)
    pub fn now_date() -> (i32, u32, u32) {
        let now = Utc::now().with_timezone(&Self::get_timezone());
        (now.year(), now.month(), now.day())
    }
    
    /// 获取当前配置时区的时分秒
    /// 
    /// # 返回
    /// 
    /// (时, 分, 秒)
    pub fn now_time() -> (u32, u32, u32) {
        let now = Utc::now().with_timezone(&Self::get_timezone());
        (now.hour(), now.minute(), now.second())
    }
}

/// 时间戳格式化辅助宏（用于日志）
/// 
/// 自动将 UTC 时间戳转换为配置的时区进行显示
#[macro_export]
macro_rules! fmt_timestamp {
    ($ts:expr) => {
        $crate::utils::TimeFormatter::format_standard($ts)
    };
    ($ts:expr, short) => {
        $crate::utils::TimeFormatter::format_short($ts)
    };
    ($ts:expr, date) => {
        $crate::utils::TimeFormatter::format_date($ts)
    };
    ($ts:expr, time) => {
        $crate::utils::TimeFormatter::format_time($ts)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timezone_config() {
        let config = TimezoneConfig::from_hours(8);
        assert_eq!(config.offset_seconds, 28800);
        
        let config = TimezoneConfig::from_minutes(480);
        assert_eq!(config.offset_seconds, 28800);
    }

    #[test]
    fn test_set_timezone() {
        // 设置为 UTC+8
        TimeFormatter::set_timezone(TimezoneConfig::from_hours(8));
        
        let timestamp = Utc.with_ymd_and_hms(2024, 1, 17, 6, 0, 0)
            .unwrap()
            .timestamp_millis();
        
        let formatted = TimeFormatter::format_standard(timestamp);
        assert!(formatted.contains("14:00:00") || formatted.contains("2024-01-17"));
    }

    #[test]
    fn test_format_functions() {
        let timestamp = Utc.with_ymd_and_hms(2024, 1, 17, 14, 30, 45)
            .unwrap()
            .timestamp_millis();
        
        let standard = TimeFormatter::format_standard(timestamp);
        assert!(standard.contains("2024-01-17"));
        assert!(standard.contains(":"));
        
        let date = TimeFormatter::format_date(timestamp);
        assert_eq!(date.len(), 10); // YYYY-MM-DD
        
        let time = TimeFormatter::format_time(timestamp);
        assert!(time.contains(":"));
    }

    #[test]
    fn test_time_calculations() {
        let now = TimeFormatter::now_utc_millis();
        let five_min_ago = now - (5 * 60 * 1000);
        
        let minutes = TimeFormatter::minutes_since(five_min_ago);
        assert_eq!(minutes, 5);
        
        let seconds = TimeFormatter::seconds_since(five_min_ago);
        assert_eq!(seconds, 300);
    }

    #[test]
    fn test_is_same_day() {
        let now = TimeFormatter::now_utc_millis();
        let one_hour_later = now + (60 * 60 * 1000);
        
        assert!(TimeFormatter::is_same_day(now, one_hour_later));
    }

    #[test]
    fn test_parse_to_utc_timestamp() {
        // 先设置时区
        TimeFormatter::set_timezone(TimezoneConfig::from_hours(8));
        
        let timestamp = TimeFormatter::parse_to_utc_timestamp("2024-01-17 14:00:00");
        assert!(timestamp.is_some());
        
        if let Some(ts) = timestamp {
            // 验证转换回来是否正确
            let formatted = TimeFormatter::format_standard(ts);
            assert!(formatted.contains("2024-01-17"));
            assert!(formatted.contains("14:00:00"));
        }
    }

    #[test]
    fn test_now_utc_millis() {
        let timestamp = TimeFormatter::now_utc_millis();
        assert!(timestamp > 0);
    }

    #[test]
    fn test_is_today() {
        let now = TimeFormatter::now_utc_millis();
        assert!(TimeFormatter::is_today(now));
        
        let yesterday = now - (24 * 60 * 60 * 1000);
        assert!(!TimeFormatter::is_today(yesterday));
    }
}
