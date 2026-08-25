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

/// 分片方案。服务端只冻结 `base_unit`（寻址网格）；其余是客户端的传输决策
/// （RESUMABLE_UPLOAD_SPEC §5：首片 64KiB 探测、单次上限 2MiB、并发 1）。
#[derive(Debug, Clone, Copy)]
pub struct UploadPlan {
    pub base_unit: u32,
    pub initial_request_size: u32,
    pub max_request_size: u32,
    pub session_threshold: u64,
    pub max_parallel_parts: u8,
}

impl UploadPlan {
    /// 从服务端给的网格构造客户端方案。
    pub fn for_base_unit(base_unit: u32) -> Self {
        let base_unit = base_unit.max(1);
        Self {
            base_unit,
            initial_request_size: base_unit,
            max_request_size: (2 * 1024 * 1024).max(base_unit),
            session_threshold: base_unit as u64,
            max_parallel_parts: 1,
        }
    }
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
    /// 完成后校验失败（20618，仅 S3 直传）：废弃 token 与会话、从零重新申请，
    /// 禁止沿原会话重传片（RESUMABLE_UPLOAD_SPEC §8）。
    RestartUpload,
    /// 终局失败，重试没有意义。
    Fatal,
}

/// 一次分片 PUT 响应 → 客户端动作。**Rust 与 TS 必须逐字一致**（RESUMABLE_UPLOAD_SPEC）。
///
/// - `code`：响应信封里的业务码；响应体无法解析成 JSON 时为 `None`。
/// - `is_server_error`：HTTP 状态是否 5xx。
///
/// 已知业务码给定论；其余（未知码 / 20616 未对齐 / 20617 模式冲突 / 无法解析）按
/// HTTP 状态兜底：**5xx 视为瞬时错误可重试，其余终局失败**。
///
/// 🔴 少了 5xx 这一路，一次数据库抖动返回的「带未知码的 500」会被直接判死、放弃
/// 整份上传——这正是本函数替代旧 `verdict_for_code(只看码)` 的原因。
pub fn chunk_verdict(code: Option<u32>, is_server_error: bool) -> ChunkVerdict {
    match code {
        Some(0) | Some(20614) => ChunkVerdict::Ok, // 20614=已完成，拿 file_id 即可
        Some(20611) | Some(20612) => ChunkVerdict::RetryChunk, // 摘要不符 / 会话忙
        Some(20610) | Some(20615) => ChunkVerdict::Resync,     // 区间对不上 / 缺区间
        Some(20613) => ChunkVerdict::StartOver,                // 会话没了
        Some(20618) => ChunkVerdict::RestartUpload,            // 完成后校验失败，从零重来
        _ => {
            if is_server_error {
                ChunkVerdict::RetryChunk
            } else {
                ChunkVerdict::Fatal
            }
        }
    }
}

/// 只按业务码判（不看 HTTP 状态）——保留给纯码单测；生产路径用 [`chunk_verdict`]。
pub fn verdict_for_code(code: u32) -> ChunkVerdict {
    chunk_verdict(Some(code), false)
}

/// complete 响应的业务码是否意味着「会话作废、废弃它从零重新申请」。
///
/// 🔴 20618 **只在 complete 路径产生**（完成后校验失败，仅 S3 直传，RESUMABLE §8），
/// chunk 循环里永远碰不到；它的客户端动作与 20613 完全相同——废弃 token 与会话、
/// 重新 prepare。抽成纯函数是因为编排层的重新申请分支必须被测试驱动：漏掉这条
/// 映射，20618 会被包成普通 Transport 错误，真实 S3 complete 失败后永远无法自愈。
pub fn complete_code_means_restart(code: u32) -> bool {
    matches!(code, 20613 | 20618)
}

/// 单片最多重试次数（首发之外）。
pub const CHUNK_RETRIES: u32 = 2;

/// 重试之间等多久：**退避**，别在网络刚断的时候连打三枪。
pub fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(300 * 2u64.pow(attempt.min(4)))
}

/// S3 面：把服务端下发的**字节区间**换算成要传的 **part 号**（RESUMABLE §8.1/§8.3）。
///
/// 🔴 两面共用同一个 status 协议：服务端把 `ListParts` 换算成区间下发，客户端再换回
/// 片号。所以这里的输入就是 proxy 面那套 `missing`，不需要第二种进度模型。
///
/// part 号从 1 起，第 n 片覆盖 `[(n-1)*part_size, n*part_size)`。区间只要与某片有交集，
/// 该片就要整片重传——S3 不接受半片，最小可写单位就是一片。
pub fn parts_from_missing(missing: &[(u64, u64)], part_size: u64, total_parts: u32) -> Vec<u32> {
    if part_size == 0 {
        return Vec::new();
    }
    let mut want: Vec<u32> = Vec::new();
    for &(offset, len) in missing {
        if len == 0 {
            continue;
        }
        let first = offset / part_size + 1;
        let last = (offset + len - 1) / part_size + 1;
        for n in first..=last {
            if n >= 1 && n <= total_parts as u64 {
                want.push(n as u32);
            }
        }
    }
    want.sort_unstable();
    want.dedup();
    want
}

/// 第 n 片在源文件中的字节区间（末片按余数截断）。
pub fn part_range(part_number: u32, part_size: u64, total: u64) -> (u64, u64) {
    let offset = (part_number as u64 - 1) * part_size;
    let len = part_size.min(total.saturating_sub(offset));
    (offset, len)
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

    /// 直接用服务端给的缺失区间构造（spec §3.2：客户端不自己求补集）。
    pub fn from_missing(total: u64, plan: UploadPlan, missing: &[(u64, u64)]) -> Self {
        let mut gaps: Vec<Gap> = missing
            .iter()
            .map(|(offset, len)| Gap {
                offset: *offset,
                len: *len,
            })
            .collect();
        gaps.sort_by_key(|g| g.offset);
        let missing_total: u64 = gaps.iter().map(|g| g.len).sum();
        Self {
            total,
            sizer: ChunkSizer::new(plan),
            gaps,
            confirmed: total.saturating_sub(missing_total),
        }
    }

    /// 以服务端的 `missing` 为准重算缺口。
    pub fn resync_missing(&mut self, missing: &[(u64, u64)]) {
        let fresh = Self::from_missing(self.total, self.sizer.plan, missing);
        self.gaps = fresh.gaps;
        self.confirmed = fresh.confirmed;
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
        assert_eq!(
            verdict_for_code(20618),
            ChunkVerdict::RestartUpload,
            "完成后校验失败：废弃会话从零重来，禁止沿原会话重传片"
        );
    }

    #[test]
    fn chunk_verdict_falls_back_on_http_status_for_unknown_bodies() {
        use ChunkVerdict::*;
        // 已知码：HTTP 状态无关，永远给定论。
        assert_eq!(chunk_verdict(Some(0), true), Ok);
        assert_eq!(chunk_verdict(Some(20611), true), RetryChunk);
        assert_eq!(chunk_verdict(Some(20613), true), StartOver);
        // 🔴 未知码 + 5xx = 瞬时错误，重试；未知码 + 非 5xx = 终局。
        assert_eq!(chunk_verdict(Some(99999), true), RetryChunk);
        assert_eq!(chunk_verdict(Some(99999), false), Fatal);
        // 20616/20617 是 400 类校验错，非 5xx → 终局；万一带 5xx 也按瞬时重试（与 TS 一致）。
        assert_eq!(chunk_verdict(Some(20617), false), Fatal);
        assert_eq!(chunk_verdict(Some(20617), true), RetryChunk);
        // 无法解析响应体：同样按 HTTP 状态兜底。
        assert_eq!(chunk_verdict(None, true), RetryChunk);
        assert_eq!(chunk_verdict(None, false), Fatal);
    }

    /// 🔴 complete 路径的重启映射（RESUMABLE §8）：20613 与 20618 都是「废弃会话、
    /// 从零重新申请」；其余码都不是。
    #[test]
    fn complete_restart_codes_are_exactly_20613_and_20618() {
        assert!(complete_code_means_restart(20613), "会话没了要重来");
        assert!(complete_code_means_restart(20618), "完成后校验失败要从零重来");
        assert!(!complete_code_means_restart(0));
        assert!(!complete_code_means_restart(20611));
        assert!(!complete_code_means_restart(20615));
        assert!(!complete_code_means_restart(20616));
        assert!(!complete_code_means_restart(99999));
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

// ---------------------------------------------------------------- 会话 sidecar

/// 一次上传会话的落盘记录。
///
/// 🔴 **单独一个文件，不塞进 `body.sealed.json`。** 两者的生命周期不一样：密文缓存
/// 为了转发秒传可以留一周，而上传会话只有 token 的 24 小时；而且同一份收到的密文
/// 可能被多条重发任务复用，它们不该不受控地共享同一个上传会话。
///
/// 🔴 存的是**整张 token**，不是 `upload_id`。status/chunk/complete 三个端点都要
/// `X-Upload-Token`，只留 id 等于留了个打不开的门牌号。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadSessionRecord {
    pub token: String,
    /// Unix 秒。过期就别再试了，直接重新 prepare。
    pub expires_at: i64,
    pub upload_url: String,
    pub plan: UploadPlanRecord,
    /// 这张 token 是为**哪一份密文**签的。
    pub sealed_sha256: String,
    pub sealed_size: u64,
    /// 谁的会话。换账号之后不能接着用。
    pub user_id: u64,
    pub local_message_id: String,
    /// 服务器身份（切环境后旧会话作废）。
    pub server_identity: String,
    /// 会话的数据面（§8.2）。🔴 必须落盘：重启后要靠它决定字节发给谁；
    /// 缺省（旧记录）= 内置面，与该记录写下时的行为一致。
    #[serde(default = "default_transport")]
    pub transport: String,
    /// 仅 S3 面：固定分片大小。缺了就算不出片号，恢复时判为不可复用。
    #[serde(default)]
    pub part_size: Option<u64>,
    /// 仅 S3 面：总片数。
    #[serde(default)]
    pub total_parts: Option<u32>,
}

fn default_transport() -> String {
    "proxy_offset_v1".to_string()
}

/// `UploadPlan` 的可序列化副本。
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct UploadPlanRecord {
    pub base_unit: u32,
    pub initial_request_size: u32,
    pub max_request_size: u32,
    pub session_threshold: u64,
    pub max_parallel_parts: u8,
}

impl From<UploadPlan> for UploadPlanRecord {
    fn from(p: UploadPlan) -> Self {
        Self {
            base_unit: p.base_unit,
            initial_request_size: p.initial_request_size,
            max_request_size: p.max_request_size,
            session_threshold: p.session_threshold,
            max_parallel_parts: p.max_parallel_parts,
        }
    }
}

impl From<UploadPlanRecord> for UploadPlan {
    fn from(p: UploadPlanRecord) -> Self {
        Self {
            base_unit: p.base_unit,
            initial_request_size: p.initial_request_size,
            max_request_size: p.max_request_size,
            session_threshold: p.session_threshold,
            max_parallel_parts: p.max_parallel_parts,
        }
    }
}

/// 记录为什么不能复用。返回原因而不是 `bool`，是为了让日志能说清「为什么又从头传了」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseVerdict {
    Reusable,
    Expired,
    DifferentUser,
    DifferentServer,
    /// 密文换了（重新封装过）——那张 token 签的是另一份字节。
    DifferentPayload,
    /// 这条记录属于另一条消息。
    DifferentMessage,
}

impl UploadSessionRecord {
    /// 这条记录还能不能拿来续传。
    ///
    /// 🔴 四个条件缺一不可。少查一个的后果都是**把字节传到错误的地方**：换了账号还用
    /// 旧 token、换了环境指向旧服务器、密文重新封装过而 token 签的是旧摘要——
    /// 最后要么被服务端拒，要么更糟，续到一个不属于这次发送的会话上。
    pub fn reusable_for(
        &self,
        now_secs: i64,
        user_id: u64,
        server_identity: &str,
        sealed_sha256: &str,
        sealed_size: u64,
        local_message_id: &str,
    ) -> ReuseVerdict {
        // 路径已经按消息隔离了；这里再查一次，是因为「路径对了内容却是别人的」
        // 属于最难查的一类错误，多一道判断比事后追查便宜。
        if self.local_message_id != local_message_id {
            return ReuseVerdict::DifferentMessage;
        }
        if self.expires_at <= now_secs {
            return ReuseVerdict::Expired;
        }
        if self.user_id != user_id {
            return ReuseVerdict::DifferentUser;
        }
        if self.server_identity != server_identity {
            return ReuseVerdict::DifferentServer;
        }
        if !self.sealed_sha256.eq_ignore_ascii_case(sealed_sha256) || self.sealed_size != sealed_size
        {
            return ReuseVerdict::DifferentPayload;
        }
        ReuseVerdict::Reusable
    }

    /// 会话记录的路径：**每条出站消息独占**。
    ///
    /// 🔴 不能只按密文路径命名。转发收到的附件时，多条新消息复用**同一份源密文**，
    /// 于是它们会读到同一张 upload token：两条消息并发操作同一个会话（服务端回
    /// SessionBusy）、进度画到别人的气泡上、一条清理状态把另一条的也清了。
    /// 密文可以共享，上传会话不行。
    pub fn path_for(sealed_cache: &std::path::Path, local_message_id: &str) -> std::path::PathBuf {
        let safe: String = local_message_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        sealed_cache.with_extension(format!("{safe}.upload-session.json"))
    }

    /// 读回来；不存在或损坏都当作「没有」——从头传总是安全的那一侧。
    pub fn load(sealed_cache: &std::path::Path, local_message_id: &str) -> Option<Self> {
        let raw = std::fs::read_to_string(Self::path_for(sealed_cache, local_message_id)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// 原子写：临时文件 → rename。
    ///
    /// 🔴 必须在**发出第一片之前**落盘。反过来的话，第一片传完就崩溃，那次上传
    /// 就成了服务端上一个没人认领的会话——下次重来又是新的，永远续不上。
    /// 🔴 `rename` 只保证「要么旧的要么新的」，不保证**掉电后还在**。写完不 fsync
    /// 的话，崩溃恢复时这条记录可能根本不存在——而它存在的唯一理由就是给崩溃用的。
    /// 顺序：独占创建临时文件 → 写 → fsync 文件 → rename → fsync 父目录。
    pub fn store(&self, sealed_cache: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let path = Self::path_for(sealed_cache, &self.local_message_id);
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        let body = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // 固定 `.tmp` 会让并发写互相截断；名字带 pid+纳秒并用 create_new 独占。
        let tmp = dir.join(format!(
            ".upload-session-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        // 目录项本身也要落盘，否则 rename 可能在崩溃后消失。
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// 丢掉会话记录，**保留密文**：密文还要用来重新 prepare。
    pub fn discard(sealed_cache: &std::path::Path, local_message_id: &str) {
        let _ = std::fs::remove_file(Self::path_for(sealed_cache, local_message_id));
    }
}


#[cfg(test)]
mod s3_part_mapping_tests {
    use super::*;

    /// §8.1/§8.3：status 的字节区间 → 要重传的片号。
    #[test]
    fn missing_ranges_map_to_whole_parts() {
        let part = 8u64 << 20;
        let total_parts = 3u32;

        // 完整一片。
        assert_eq!(parts_from_missing(&[(0, part)], part, total_parts), vec![1]);
        // 🔴 只缺一个字节也要整片重传：S3 的最小可写单位是一片。
        assert_eq!(parts_from_missing(&[(part, 1)], part, total_parts), vec![2]);
        // 跨片区间 → 覆盖到的片全要。
        assert_eq!(
            parts_from_missing(&[(part - 1, 2)], part, total_parts),
            vec![1, 2]
        );
        // 多段合并去重且有序。
        assert_eq!(
            parts_from_missing(&[(2 * part, 10), (0, 5), (1, 5)], part, total_parts),
            vec![1, 3]
        );
        // 空区间不产片；越界片号被丢弃（服务端不该给，给了也不炸）。
        assert_eq!(parts_from_missing(&[(0, 0)], part, total_parts), Vec::<u32>::new());
        assert_eq!(parts_from_missing(&[(99 * part, part)], part, total_parts), Vec::<u32>::new());
    }

    /// 末片按余数截断，不能按整片长度去读盘（会越界或多传）。
    #[test]
    fn last_part_is_truncated_to_the_remainder() {
        let part = 8u64 << 20;
        let total = part * 2 + 123;
        assert_eq!(part_range(1, part, total), (0, part));
        assert_eq!(part_range(2, part, total), (part, part));
        assert_eq!(part_range(3, part, total), (2 * part, 123));
        // 单片会话：整个文件就是第 1 片（小文件在 S3 面的常态）。
        assert_eq!(part_range(1, part, 900), (0, 900));
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn rec() -> UploadSessionRecord {
        UploadSessionRecord {
            token: "tok".into(),
            expires_at: 1_000,
            upload_url: "http://h/api/app/files/upload".into(),
            plan: UploadPlanRecord {
                base_unit: 65536,
                initial_request_size: 65536,
                max_request_size: 2 << 20,
                session_threshold: 65536,
                max_parallel_parts: 1,
            },
            transport: default_transport(),
            part_size: None,
            total_parts: None,
            sealed_sha256: "aa".repeat(32),
            sealed_size: 4096,
            user_id: 7,
            local_message_id: "m1".into(),
            server_identity: "local".into(),
        }
    }

    #[test]
    fn a_fresh_matching_record_is_reusable() {
        assert_eq!(
            rec().reusable_for(999, 7, "local", &"aa".repeat(32), 4096, "m1"),
            ReuseVerdict::Reusable
        );
    }

    /// 🔴 过期的 token 再试也是白试——重新 prepare 才是出路。
    #[test]
    fn an_expired_record_is_not_reusable() {
        assert_eq!(
            rec().reusable_for(1_000, 7, "local", &"aa".repeat(32), 4096, "m1"),
            ReuseVerdict::Expired
        );
    }

    /// 🔴 换了账号还用旧 token = 把字节传到别人的会话里。
    #[test]
    fn another_user_may_not_reuse_it() {
        assert_eq!(
            rec().reusable_for(999, 8, "local", &"aa".repeat(32), 4096, "m1"),
            ReuseVerdict::DifferentUser
        );
    }

    /// 换了服务器环境，旧会话在新服务器上根本不存在。
    #[test]
    fn another_server_invalidates_it() {
        assert_eq!(
            rec().reusable_for(999, 7, "prod", &"aa".repeat(32), 4096, "m1"),
            ReuseVerdict::DifferentServer
        );
    }

    /// 🔴 密文重新封装过：token 签的是旧摘要，续上去传的就是拼错的文件。
    #[test]
    fn a_resealed_payload_invalidates_it() {
        assert_eq!(
            rec().reusable_for(999, 7, "local", &"bb".repeat(32), 4096, "m1"),
            ReuseVerdict::DifferentPayload
        );
        assert_eq!(
            rec().reusable_for(999, 7, "local", &"aa".repeat(32), 4097, "m1"),
            ReuseVerdict::DifferentPayload
        );
    }

    #[test]
    fn it_survives_a_round_trip_and_lives_beside_the_sealed_blob() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = dir.path().join("body.sealed");
        std::fs::write(&sealed, b"x").unwrap();

        rec().store(&sealed).unwrap();
        let back = UploadSessionRecord::load(&sealed, "m1").expect("load");
        assert_eq!(back.token, "tok");
        assert_eq!(back.sealed_size, 4096);

        // 丢会话不丢密文：密文还要用来重新 prepare。
        UploadSessionRecord::discard(&sealed, "m1");
        assert!(UploadSessionRecord::load(&sealed, "m1").is_none());
        assert!(sealed.exists(), "密文不能跟着会话一起被删");
    }

    #[test]
    fn a_corrupt_record_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = dir.path().join("body.sealed");
        std::fs::write(UploadSessionRecord::path_for(&sealed, "m1"), b"{not json").unwrap();
        assert!(UploadSessionRecord::load(&sealed, "m1").is_none());
    }

    /// 🔴 转发时多条新消息复用**同一份源密文**：会话必须各归各的。
    ///
    /// 共用一张 token 的话，两条消息会并发操作同一个服务端会话（回 SessionBusy）、
    /// 进度画到别人的气泡上、一条发完把另一条的会话也清了。
    #[test]
    fn two_messages_sharing_one_sealed_blob_do_not_share_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = dir.path().join("shared.sealed");
        std::fs::write(&sealed, b"x").unwrap();

        let mut first = rec();
        first.local_message_id = "msg-a".into();
        first.token = "tok-a".into();
        first.store(&sealed).unwrap();

        let mut second = rec();
        second.local_message_id = "msg-b".into();
        second.token = "tok-b".into();
        second.store(&sealed).unwrap();

        assert_ne!(
            UploadSessionRecord::path_for(&sealed, "msg-a"),
            UploadSessionRecord::path_for(&sealed, "msg-b"),
        );
        assert_eq!(
            UploadSessionRecord::load(&sealed, "msg-a").unwrap().token,
            "tok-a",
            "第二条消息把第一条的 token 覆盖了"
        );

        // 一条发完清理，不能带走另一条的会话。
        UploadSessionRecord::discard(&sealed, "msg-a");
        assert!(UploadSessionRecord::load(&sealed, "msg-a").is_none());
        assert!(UploadSessionRecord::load(&sealed, "msg-b").is_some());
    }

    /// 路径隔离之外再加一道：内容属于别人就不认。
    #[test]
    fn a_record_belonging_to_another_message_is_rejected() {
        assert_eq!(
            rec().reusable_for(999, 7, "local", &"aa".repeat(32), 4096, "m2"),
            ReuseVerdict::DifferentMessage
        );
    }
}
