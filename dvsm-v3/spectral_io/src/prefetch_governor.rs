// =============================================================================
// spectral_io/src/prefetch_governor.rs  |  Spectral I/O Governor
// =============================================================================
// Orchestrates entropy monitoring → divergence trigger → asset queue.
// Sits between DVSM kernel output and the engine's async streaming system.
//
// PIPELINE (per frame tick):
//
//   [1] SpectralEntropyState::update(Z, norm_sq, dt)
//         → bool trigger
//         → divergence_rate
//
//   [2] ModeClassifier::update(Z)
//         → per-mode GhostClass
//         → mip_bias_array[16]
//
//   [3] MarkovSalience::observe(h_normalized)
//         → prefetch_mask
//         → prioritized_groups(...)
//
//   [4] PrefetchQueue::enqueue(groups, lead_frames)
//         → async I/O jobs submitted to engine streaming thread
//         → BC7/ASTC decompression scheduled ahead of render thread
//
//   [5] MipHintBuffer::update(mip_bias_array)
//         → written to engine texture streaming hint buffer
//         → streaming layer drops unnecessary mips (bandwidth relief)
//
// BANDWIDTH MODEL (Ally X LPDDR5X-7500):
//   Peak bandwidth: ~60 GB/s (shared CPU+GPU, iGPU gets ~30–40 GB/s)
//   Standard streaming (no DVSM): texture fetches fill ~70% of iGPU bandwidth
//   With mip bias (Diffuse modes): estimated 25–35% bandwidth reduction
//   With predictive prefetch: stutter elimination (0→1 frame hitches removed)
//
//   These are estimates based on mip level size ratios (4:1 per level).
//   Actual gains are title-dependent. Measure with FrameVarianceRing.p99().
//   Do not publish bandwidth savings claims without ring buffer proof.
//
// QUEUE DESIGN:
//   Fixed-capacity ring (no heap). Engine polls each frame.
//   Entries expire after `lead_frames` frames — stale prefetches discarded.
//   This prevents bandwidth waste from false triggers.
// =============================================================================

use crate::spectral_io::src::entropy::{
    SpectralEntropyState, ModeClassifier, DIM, DIVERGENCE_THRESHOLD,
};
use crate::spectral_io::src::markov_salience::{MarkovSalience, BUCKET_COUNT};

// ---------------------------------------------------------------------------
// PREFETCH JOB  (engine-consumable work item)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PrefetchJob {
    /// Asset group index (engine-defined; maps to mesh/texture/shader batch)
    pub group_id:      u32,
    /// Priority score [0,1]: 1.0 = highest. From salience probability.
    pub priority:      f32,
    /// Frames remaining before this job expires (stale → discard)
    pub ttl_frames:    u16,
    /// Compression format hint (engine uses this to pick decompressor)
    pub format_hint:   CompressionFormat,
    /// Padding to 12 bytes
    pub _pad:          u8,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CompressionFormat {
    #[default]
    Unknown = 0,
    BC7     = 1,   // desktop/Xbox BC7 mode
    ASTC    = 2,   // mobile / handheld ASTC 4×4
    BC1     = 3,   // low-quality fallback
}

// ---------------------------------------------------------------------------
// PREFETCH QUEUE (fixed ring, no alloc)
// ---------------------------------------------------------------------------

pub const QUEUE_CAPACITY: usize = 32;

pub struct PrefetchQueue {
    jobs:  [PrefetchJob; QUEUE_CAPACITY],
    head:  usize,
    tail:  usize,
    count: usize,
}

impl PrefetchQueue {
    pub fn new() -> Self {
        Self {
            jobs: [PrefetchJob::default(); QUEUE_CAPACITY],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Enqueue a job. Drops oldest if full (back-pressure: oldest is stalest).
    pub fn enqueue(&mut self, job: PrefetchJob) {
        if self.count == QUEUE_CAPACITY {
            // Drop oldest (head)
            self.head = (self.head + 1) % QUEUE_CAPACITY;
            self.count -= 1;
        }
        self.jobs[self.tail] = job;
        self.tail = (self.tail + 1) % QUEUE_CAPACITY;
        self.count += 1;
    }

    /// Dequeue next job. Returns None if empty.
    pub fn dequeue(&mut self) -> Option<PrefetchJob> {
        if self.count == 0 { return None; }
        let job = self.jobs[self.head];
        self.head = (self.head + 1) % QUEUE_CAPACITY;
        self.count -= 1;
        Some(job)
    }

    /// Age all jobs by 1 frame. Remove expired (ttl = 0).
    pub fn tick_ttl(&mut self) {
        let mut i = self.head;
        let mut new_count = 0;
        for _ in 0..self.count {
            if self.jobs[i].ttl_frames > 0 {
                self.jobs[i].ttl_frames -= 1;
                new_count += 1;
            }
            i = (i + 1) % QUEUE_CAPACITY;
        }
        // Compact: rebuild from survivors. Simple O(N) for N=32.
        let mut tmp = [PrefetchJob::default(); QUEUE_CAPACITY];
        let mut w = 0usize;
        let mut r = self.head;
        for _ in 0..self.count {
            if self.jobs[r].ttl_frames > 0 {
                tmp[w] = self.jobs[r];
                w += 1;
            }
            r = (r + 1) % QUEUE_CAPACITY;
        }
        self.jobs = tmp;
        self.head = 0;
        self.tail = w;
        self.count = new_count;
    }

    pub fn len(&self) -> usize { self.count }
    pub fn is_empty(&self) -> bool { self.count == 0 }
}

// ---------------------------------------------------------------------------
// MIP HINT BUFFER  (written each frame to engine texture streaming layer)
// ---------------------------------------------------------------------------

/// Per-mode mip bias array + aggregate hint.
/// Engine maps these to its internal mip streaming priority system.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MipHintBuffer {
    /// Per-mode bias: 0 = full res, 1 = -1 mip, 2 = -2 mips
    pub per_mode: [u8; DIM],
    /// Global aggregate (max of per_mode). Single hint for coarse streaming.
    pub global:   u8,
    /// Entropy normalized [0,255] for engine debug overlay
    pub h_u8:     u8,
    pub _pad:     u16,
}

impl MipHintBuffer {
    pub fn from_classifier(classifier: &ModeClassifier, h_normalized: f32) -> Self {
        let per_mode = classifier.mip_bias_array();
        let global   = classifier.global_mip_bias();
        let h_u8     = (h_normalized * 255.0) as u8;
        Self { per_mode, global, h_u8, _pad: 0 }
    }
}

// ---------------------------------------------------------------------------
// SPECTRAL I/O GOVERNOR  (top-level orchestrator)
// ---------------------------------------------------------------------------

pub struct SpectralIOGovernor {
    pub entropy:    SpectralEntropyState,
    pub classifier: ModeClassifier,
    pub markov:     MarkovSalience,
    pub queue:      PrefetchQueue,
    pub mip_hints:  MipHintBuffer,

    /// Total asset groups the engine has registered
    pub total_groups: u32,
    /// Max prefetch jobs per trigger event
    pub max_jobs_per_trigger: u32,
    /// Default TTL in frames for prefetch jobs
    pub default_ttl: u16,
    /// Default compression format hint
    pub format_hint: CompressionFormat,
}

impl SpectralIOGovernor {
    pub fn new(total_groups: u32) -> Self {
        Self {
            entropy:              SpectralEntropyState::default(),
            classifier:           ModeClassifier::default(),
            markov:               MarkovSalience::new(),
            queue:                PrefetchQueue::new(),
            mip_hints:            MipHintBuffer::default(),
            total_groups,
            max_jobs_per_trigger: 4,
            default_ttl:          16,   // ~67ms at 240Hz — enough to decompress BC7
            format_hint:          CompressionFormat::BC7,
        }
    }

    /// Full per-frame tick. Call after DVSMSupervisor::tick().
    ///
    /// z:       current Z vector from DVSMState
    /// norm_sq: current ‖Z‖² from DVSMState
    /// dt:      WattageProfile.dt
    ///
    /// Returns GovFrameResult: mip hints + whether prefetch jobs were enqueued.
    pub fn tick(
        &mut self,
        z: &[f32; DIM],
        norm_sq: f32,
        dt: f32,
    ) -> GovFrameResult {
        // 1. Update entropy
        let trigger = self.entropy.update(z, norm_sq, dt);

        // 2. Update mode classifier
        self.classifier.update(z);

        // 3. Update Markov (observe new entropy bucket)
        self.markov.observe(self.entropy.h_normalized());

        // 4. Update mip hints (every frame — drives texture streaming)
        self.mip_hints = MipHintBuffer::from_classifier(
            &self.classifier,
            self.entropy.h_normalized(),
        );

        // 5. Age existing jobs
        self.queue.tick_ttl();

        // 6. If triggered: enqueue prefetch jobs
        let jobs_enqueued = if trigger {
            let lead = self.entropy.estimated_lead_frames();
            let ttl  = (self.default_ttl as u32 + lead).min(60) as u16;
            let groups = self.markov.prioritized_groups(
                self.total_groups,
                self.max_jobs_per_trigger,
            );
            let sal = self.markov.salience();
            let mut count = 0u32;
            for &g in groups.as_slice() {
                let b = g as usize % BUCKET_COUNT;
                self.queue.enqueue(PrefetchJob {
                    group_id:    g,
                    priority:    sal[b],
                    ttl_frames:  ttl,
                    format_hint: self.format_hint,
                    _pad:        0,
                });
                count += 1;
            }
            count
        } else { 0 };

        GovFrameResult {
            triggered:      trigger,
            jobs_enqueued,
            mip_hints:      self.mip_hints,
            divergence_rate: self.entropy.divergence_rate,
            h_normalized:   self.entropy.h_normalized(),
        }
    }

    /// Engine calls this to drain the prefetch queue each frame.
    pub fn drain_jobs(&mut self, out: &mut [PrefetchJob], max: usize) -> usize {
        let mut n = 0;
        while n < max {
            match self.queue.dequeue() {
                Some(j) => { out[n] = j; n += 1; }
                None    => break,
            }
        }
        n
    }
}

// ---------------------------------------------------------------------------
// FRAME RESULT (returned to engine each tick)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GovFrameResult {
    pub triggered:       bool,
    pub jobs_enqueued:   u32,
    pub mip_hints:       MipHintBuffer,
    pub divergence_rate: f32,
    pub h_normalized:    f32,
}
