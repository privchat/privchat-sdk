// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! 分片上传客户端（RESUMABLE_UPLOAD_SPEC §4）。
//!
//! 用户能感觉到的那件事就一句：**弱网上传一张 4MB 的照片不再一失败就从头再来。**
//!
//! 三个决定：
//!
//! 1. **分片大小由客户端定，网格由服务端定。** `base_unit` 是寻址网格，冻结；一次请求
//!    发几个 unit 是传输决策，每次重新决定。把请求大小也冻死的话，一个弱网客户端在
//!    大请求连续超时之后就没有别的招了——只能放弃整次上传。
//!
//! 2. **先探一次，再按实测吞吐调整。** 首个请求固定 `initial_request_size`（64KiB），
//!    用它端到端的耗时估吞吐，之后按「一秒钟能传多少」定下一片的大小。
//!
//! 3. **失败立刻减半，成功缓慢增长。** 网络变差是突然的，变好是渐进的；两个方向用
//!    同样的步长，要么恢复太慢，要么在临界点上反复抖。

use std::time::{Duration, Instant};

use crate::{Error, Result};

/// 服务端下发的分片方案。
#[derive(Debug, Clone, Copy)]
pub struct UploadPlan {
    pub base_unit: u32,
    pub initial_request_size: u32,
    pub max_request_size: u32,
    pub session_threshold: u64,
    pub max_parallel_parts: u8,
}

/// 上传进度：`uploaded / total`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProgress {
    pub uploaded: u64,
    pub total: u64,
}

impl UploadProgress {
    pub fn percent(&self) -> u8 {
        if self.total == 0 {
            return 100;
        }
        ((self.uploaded.min(self.total) as u128 * 100) / self.total as u128) as u8
    }
}

/// 单次请求大小的自适应控制器。
///
/// 🔴 **吞吐用端到端耗时算，不减 RTT。** 减掉一个同样有噪声的估计值，两者接近时
/// 商会炸上天：64KiB / (85ms - 80ms) 会算出 12.8MiB/s，于是下一片直接顶到上限，
/// 在真实弱网里必然超时——然后减半、再炸、再减半。宁可保守一点。
#[derive(Debug, Clone)]
pub struct ChunkSizer {
    plan: UploadPlan,
    /// 平滑后的吞吐（字节/秒）。`None` = 还没测过。
    rate: Option<f64>,
    next: u32,
}

impl ChunkSizer {
    pub fn new(plan: UploadPlan) -> Self {
        Self {
            plan,
            rate: None,
            next: plan.initial_request_size.max(plan.base_unit),
        }
    }

    /// 下一片发多大。
    pub fn next_size(&self) -> u32 {
        self.next
    }

    /// 一片成功了：按实测吞吐调整。
    pub fn on_success(&mut self, bytes: u32, elapsed: Duration) {
        let secs = elapsed.as_secs_f64().max(0.001);
        let sample = bytes as f64 / secs;
        // EWMA：单次抖动不该让下一片直接跳一个数量级。
        self.rate = Some(match self.rate {
            Some(prev) => prev * 0.7 + sample * 0.3,
            None => sample,
        });

        // 目标：一片大约传 1 秒。太小则请求开销占比过高，太大则一次失败要重传很多。
        let target = self.rate.unwrap_or(sample);
        // 🔴 每步最多涨 4 倍。一次异常快的采样（比如命中了某层缓存）不该把下一片
        // 直接顶到上限——那一片一旦超时，代价是整片重来。
        let capped = (target as u64).min(bytes as u64 * 4);
        self.next = self.clamp(capped);
    }

    /// 一片失败了：**立刻减半**。
    pub fn on_failure(&mut self) {
        let halved = (self.next / 2).max(self.plan.base_unit);
        self.next = self.clamp(halved as u64);
        // 之前的吞吐估计已经不作数了：网络刚证明了它比估计的差。
        self.rate = None;
    }

    /// 收进 [base_unit, max_request_size]，并对齐到网格。
    fn clamp(&self, want: u64) -> u32 {
        let base = self.plan.base_unit.max(1) as u64;
        let max = self.plan.max_request_size.max(self.plan.base_unit) as u64;
        let want = want.clamp(base, max);
        // 向下取整到网格：非末段必须整格，多出来的零头服务端会拒。
        let aligned = (want / base) * base;
        aligned.max(base) as u32
    }
}

/// 还没确认的区间（`[offset, offset+len)`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub offset: u64,
    pub len: u64,
}

/// 已确认区间 → 缺口。
///
/// 🔴 缺口是**续传的全部依据**：服务端只告诉你「哪些段确认了」，要传什么得自己算。
/// 算错会有两种后果——漏掉一段就永远 complete 不了，多算一段就白传。
pub fn gaps_from_confirmed(confirmed: &[(u64, u64)], total: u64) -> Vec<Gap> {
    let mut ranges: Vec<(u64, u64)> = confirmed.to_vec();
    ranges.sort_by_key(|(off, _)| *off);

    let mut gaps = Vec::new();
    let mut cursor = 0u64;
    for (off, len) in ranges {
        if off > cursor {
            gaps.push(Gap {
                offset: cursor,
                len: off - cursor,
            });
        }
        cursor = cursor.max(off + len);
    }
    if cursor < total {
        gaps.push(Gap {
            offset: cursor,
            len: total - cursor,
        });
    }
    gaps
}

/// 把一个缺口切成若干请求：除最后一片外都按 `size` 且对齐网格。
pub fn split_gap(gap: Gap, size: u32, total: u64) -> Vec<Gap> {
    let mut out = Vec::new();
    let mut offset = gap.offset;
    let end = gap.offset + gap.len;
    while offset < end {
        let mut len = (size as u64).min(end - offset);
        // 非末段必须整格；末段（正好顶到文件结尾）可以是任意长度。
        if offset + len < total {
            let base = size.max(1) as u64;
            let aligned = (len / base) * base;
            if aligned > 0 {
                len = aligned;
            }
        }
        out.push(Gap { offset, len });
        offset += len;
    }
    out
}

/// 一次分片请求的结果分类。
///
/// 🔴 分类决定客户端下一步做什么，所以它必须来自**服务端的错误码**，不能靠猜。
/// 猜错的两个方向分别是「无限重试一个终局拒绝」和「放弃一个本可自愈的上传」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkVerdict {
    /// 写进去了，或者同样内容已经确认过（都算成功）。
    Ok,
    /// 这一片值得再试（网络抖动、校验和不符、服务端忙）。
    RetryChunk,
    /// 区间视图对不上：重新拉一次 status 再继续。
    Resync,
    /// 会话没了：只能重新申请 token 从头传。
    StartOver,
    /// 终局失败，重试没有意义。
    Fatal,
}

/// 服务端错误码 → 客户端动作。码值见 protocol `ErrorCode` 20610-20617。
pub fn verdict_for_code(code: u32) -> ChunkVerdict {
    match code {
        0 => ChunkVerdict::Ok,
        // 这一片本身有问题，重传这一片就好。
        20611 => ChunkVerdict::RetryChunk,
        // 会话被别的请求占着，等一下再来。
        20612 => ChunkVerdict::RetryChunk,
        // 区间对不上，拉 status 对齐。
        20610 => ChunkVerdict::Resync,
        // 会话没了 / 模式冲突 / 没有分片方案：这条路走不通了。
        20613 => ChunkVerdict::StartOver,
        20614 => ChunkVerdict::Ok, // 已完成：拿 file_id 就是了
        20615 => ChunkVerdict::Resync,
        20616 | 20617 => ChunkVerdict::Fatal,
        _ => ChunkVerdict::Fatal,
    }
}

/// 单片最多重试次数（首发之外）。
pub const CHUNK_RETRIES: u32 = 2;

/// 重试之间等多久：**退避**，别在网络刚断的时候连打三枪。
pub fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(300 * 2u64.pow(attempt.min(4)))
}

/// 一次分片上传的状态机——**不做 IO**，因此可以完整单测。
///
/// 🔴 把决策与 IO 分开的理由：弱网行为几乎全是决策（发多大、重试几次、什么时候
/// 去对齐、什么时候放弃）。混在 HTTP 调用里就只能靠真起一个服务端来测，
/// 于是那些最该被覆盖的分支反而最难覆盖。
pub struct ResumableUpload {
    pub total: u64,
    sizer: ChunkSizer,
    gaps: Vec<Gap>,
    confirmed: u64,
}

impl ResumableUpload {
    pub fn new(total: u64, plan: UploadPlan, confirmed: &[(u64, u64)]) -> Self {
        Self {
            total,
            sizer: ChunkSizer::new(plan),
            gaps: gaps_from_confirmed(confirmed, total),
            confirmed: confirmed.iter().map(|(_, len)| len).sum(),
        }
    }

    pub fn progress(&self) -> UploadProgress {
        UploadProgress {
            uploaded: self.confirmed,
            total: self.total,
        }
    }

    pub fn is_done(&self) -> bool {
        self.gaps.is_empty()
    }

    /// 下一片该发哪一段。
    pub fn next_chunk(&self) -> Option<Gap> {
        let gap = self.gaps.first()?;
        let pieces = split_gap(*gap, self.sizer.next_size(), self.total);
        pieces.into_iter().next()
    }

    /// 这一片成功了。
    pub fn on_chunk_ok(&mut self, chunk: Gap, elapsed: Duration) {
        self.sizer.on_success(chunk.len as u32, elapsed);
        self.consume(chunk);
        self.confirmed = (self.confirmed + chunk.len).min(self.total);
    }

    /// 这一片失败了：下一片减半，缺口不动（原样重来）。
    pub fn on_chunk_failed(&mut self) {
        self.sizer.on_failure();
    }

    /// 服务端告诉我们真实的已确认区间——**以它为准**重算缺口。
    ///
    /// 本地记账只是乐观估计；一旦与服务端不一致（重试、并发、跨设备），必须听服务端的。
    pub fn resync(&mut self, confirmed: &[(u64, u64)]) {
        self.gaps = gaps_from_confirmed(confirmed, self.total);
        self.confirmed = confirmed.iter().map(|(_, len)| len).sum();
    }

    fn consume(&mut self, chunk: Gap) {
        let Some(first) = self.gaps.first_mut() else {
            return;
        };
        if chunk.offset != first.offset {
            return;
        }
        if chunk.len >= first.len {
            self.gaps.remove(0);
        } else {
            first.offset += chunk.len;
            first.len -= chunk.len;
        }
    }

    /// 当前分片大小（测试与日志用）。
    pub fn current_chunk_size(&self) -> u32 {
        self.sizer.next_size()
    }
}

/// 分片上传对外的错误。
pub fn chunk_error(msg: impl Into<String>) -> Error {
    Error::Transport(msg.into())
}

/// 计时器：把「一片花了多久」这件事收在一处，避免每个调用点各写一遍。
pub struct ChunkTimer(Instant);

impl ChunkTimer {
    pub fn start() -> Self {
        Self(Instant::now())
    }
    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// 校验一份 blob 的分片摘要（服务端要求 `X-Chunk-SHA256`）。
pub fn chunk_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

pub type UploadResult<T> = Result<T>;

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> UploadPlan {
        UploadPlan {
            base_unit: 64 * 1024,
            initial_request_size: 64 * 1024,
            max_request_size: 2 * 1024 * 1024,
            session_threshold: 64 * 1024,
            max_parallel_parts: 1,
        }
    }

    #[test]
    fn the_first_request_is_one_base_unit() {
        let sizer = ChunkSizer::new(plan());
        assert_eq!(sizer.next_size(), 64 * 1024, "首片必须是探测大小");
    }

    /// 好链路：涨，但**每步最多 4 倍**。
    #[test]
    fn a_fast_link_grows_but_not_in_one_leap() {
        let mut sizer = ChunkSizer::new(plan());
        // 64KiB / 10ms ≈ 6.4MiB/s，一秒的量早就超过上限了。
        sizer.on_success(64 * 1024, Duration::from_millis(10));
        assert_eq!(
            sizer.next_size(),
            4 * 64 * 1024,
            "一次超快采样不该把下一片直接顶到上限"
        );
        sizer.on_success(4 * 64 * 1024, Duration::from_millis(40));
        assert!(sizer.next_size() > 4 * 64 * 1024);
        assert!(sizer.next_size() <= 2 * 1024 * 1024, "不得超过服务端上限");
    }

    /// 🔴 弱网：失败立刻减半，一路降到网格下限就不再降。
    #[test]
    fn a_failing_link_halves_immediately_and_stops_at_the_grid() {
        let mut sizer = ChunkSizer::new(plan());
        sizer.on_success(64 * 1024, Duration::from_millis(10));
        sizer.on_success(256 * 1024, Duration::from_millis(40));
        let before = sizer.next_size();
        sizer.on_failure();
        assert!(sizer.next_size() < before, "失败必须立刻变小");
        for _ in 0..10 {
            sizer.on_failure();
        }
        assert_eq!(
            sizer.next_size(),
            64 * 1024,
            "降到网格就是底，再降下去服务端也会拒"
        );
    }

    /// 大小永远落在网格上——不然服务端直接判不对齐。
    #[test]
    fn sizes_always_land_on_the_grid() {
        let mut sizer = ChunkSizer::new(plan());
        for ms in [7, 13, 200, 3, 900, 45] {
            sizer.on_success(sizer.next_size(), Duration::from_millis(ms));
            assert_eq!(
                sizer.next_size() % (64 * 1024),
                0,
                "{} 不在网格上",
                sizer.next_size()
            );
        }
    }

    #[test]
    fn gaps_of_an_untouched_file_is_the_whole_file() {
        assert_eq!(
            gaps_from_confirmed(&[], 1000),
            vec![Gap {
                offset: 0,
                len: 1000
            }]
        );
    }

    /// 🔴 缺口算错就等于续传坏掉：漏一段永远传不完，多一段白传。
    #[test]
    fn gaps_are_exactly_what_is_missing() {
        // 已确认 [0,100) 和 [300,400)，总长 500。
        let gaps = gaps_from_confirmed(&[(0, 100), (300, 100)], 500);
        assert_eq!(
            gaps,
            vec![
                Gap {
                    offset: 100,
                    len: 200
                },
                Gap {
                    offset: 400,
                    len: 100
                }
            ]
        );
    }

    #[test]
    fn a_fully_confirmed_file_has_no_gaps() {
        assert!(gaps_from_confirmed(&[(0, 500)], 500).is_empty());
    }

    /// 乱序、重叠的已确认区间也要算对——服务端合并过，但别依赖那个假设。
    #[test]
    fn out_of_order_and_overlapping_ranges_still_compute() {
        let gaps = gaps_from_confirmed(&[(300, 100), (0, 50), (25, 100)], 500);
        assert_eq!(
            gaps,
            vec![
                Gap {
                    offset: 125,
                    len: 175
                },
                Gap {
                    offset: 400,
                    len: 100
                }
            ]
        );
    }

    /// 末段可以短，中间段必须整格。
    #[test]
    fn only_the_final_piece_may_be_short() {
        let total = 64 * 1024 * 2 + 500;
        let pieces = split_gap(
            Gap {
                offset: 0,
                len: total,
            },
            64 * 1024,
            total,
        );
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].len, 64 * 1024);
        assert_eq!(pieces[1].len, 64 * 1024);
        assert_eq!(pieces[2].len, 500, "只有顶到文件末尾的那片可以不满格");
    }

    /// 🔴 整条续传的核心：断在中间，接着传的**正好**是缺口，一个字节都不多。
    #[test]
    fn resuming_sends_exactly_the_gap() {
        let total = 64 * 1024 * 5;
        let mut up = ResumableUpload::new(total, plan(), &[(0, 64 * 1024 * 2)]);
        assert_eq!(up.progress().uploaded, 64 * 1024 * 2);
        assert_eq!(up.progress().percent(), 40);

        let mut sent = 0u64;
        while let Some(chunk) = up.next_chunk() {
            sent += chunk.len;
            up.on_chunk_ok(chunk, Duration::from_millis(50));
        }
        assert!(up.is_done());
        assert_eq!(sent, 64 * 1024 * 3, "只该补那三片，多一个字节都是白传");
        assert_eq!(up.progress().percent(), 100);
    }

    /// 失败之后重来的是**同一段**，不会跳过。
    #[test]
    fn a_failed_chunk_is_retried_as_the_same_range() {
        let total = 64 * 1024 * 4;
        let mut up = ResumableUpload::new(total, plan(), &[]);
        let first = up.next_chunk().expect("chunk");
        up.on_chunk_failed();
        let again = up.next_chunk().expect("chunk");
        assert_eq!(again.offset, first.offset, "失败后必须从同一个 offset 重来");
        assert_eq!(up.progress().uploaded, 0, "失败不能算进度");
    }

    /// 服务端说了算：resync 之后按服务端的区间重算。
    #[test]
    fn resync_takes_the_servers_word_for_it() {
        let total = 1000;
        let mut up = ResumableUpload::new(total, plan(), &[]);
        up.resync(&[(0, 400)]);
        assert_eq!(up.progress().uploaded, 400);
        assert_eq!(up.next_chunk().unwrap().offset, 400);
    }

    /// 错误码分流：每一类的动作都必须是它该有的那个。
    #[test]
    fn error_codes_map_to_the_right_action() {
        assert_eq!(verdict_for_code(0), ChunkVerdict::Ok);
        assert_eq!(verdict_for_code(20611), ChunkVerdict::RetryChunk);
        assert_eq!(verdict_for_code(20612), ChunkVerdict::RetryChunk);
        assert_eq!(verdict_for_code(20610), ChunkVerdict::Resync);
        assert_eq!(verdict_for_code(20615), ChunkVerdict::Resync);
        assert_eq!(verdict_for_code(20613), ChunkVerdict::StartOver);
        assert_eq!(verdict_for_code(20614), ChunkVerdict::Ok);
        assert_eq!(
            verdict_for_code(20616),
            ChunkVerdict::Fatal,
            "没有分片方案时重试多少次都一样"
        );
        assert_eq!(verdict_for_code(20617), ChunkVerdict::Fatal);
    }

    /// 退避是涨的——网络刚断的时候连打三枪没有意义。
    #[test]
    fn retries_back_off() {
        assert!(retry_delay(1) > retry_delay(0));
        assert!(retry_delay(2) > retry_delay(1));
    }

    #[test]
    fn progress_percent_is_monotonic_and_bounded() {
        let p = UploadProgress {
            uploaded: 0,
            total: 0,
        };
        assert_eq!(p.percent(), 100, "空文件就是传完了");
        let p = UploadProgress {
            uploaded: 999,
            total: 1000,
        };
        assert_eq!(p.percent(), 99);
    }
}
