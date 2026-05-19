// =============================================================================
// spectral_io/src/markov_salience.rs  |  Markov Salience (Route 9)
// =============================================================================
// Tracks phase-space transition history and predicts which asset groups
// are most likely needed next. This is "Route 9" in the DVSM design.
//
// MATH:
//   State: quantize H_normalized into B buckets (default B=8).
//   Transition matrix T[i][j] = count of transitions from bucket i → j.
//   Row-normalize → stochastic matrix P where P[i][j] = P(next=j | now=i).
//
//   Salience vector: S = P[current_bucket]  (probability distribution over
//   next buckets). Maps directly to asset group priority.
//
//   Markov assumption: next state depends only on current state.
//   This is an approximation — real scene transitions have longer memory.
//   For a DIM=16 state space, the Markov model is a practical compression
//   of higher-order dynamics. Increasing B improves fidelity at O(B²) cost.
//
// ASSET GROUP MAPPING:
//   Divide asset groups into B priority classes (0=highest).
//   Group g is assigned to bucket b = g % B.
//   Salience S[b] > SALIENCE_THRESHOLD → enqueue group g for prefetch.
//
// DEV NOTE — arbitrary bucket boundaries:
//   Entropy buckets are uniform quantization of [0, H_MAX].
//   They do NOT correspond to physical scene features.
//   "Attractor basin" is a useful metaphor but the bucket that captures
//   a given scene depends on title-specific Z dynamics.
//   Tune BUCKET_COUNT and SALIENCE_THRESHOLD per title.
// =============================================================================

pub const BUCKET_COUNT:        usize = 8;
pub const SALIENCE_THRESHOLD:  f32   = 0.25;  // P(next bucket) > this → prefetch

#[derive(Clone, Debug)]
pub struct MarkovSalience {
    /// Transition count matrix T[from][to]
    pub transitions: [[u32; BUCKET_COUNT]; BUCKET_COUNT],
    /// Current entropy bucket index
    pub current_bucket: usize,
    /// Total transitions observed (for normalization)
    pub total: [u32; BUCKET_COUNT],
}

impl MarkovSalience {
    pub fn new() -> Self {
        Self {
            transitions: [[0; BUCKET_COUNT]; BUCKET_COUNT],
            current_bucket: 0,
            total: [0; BUCKET_COUNT],
        }
    }

    /// Quantize H_normalized ∈ [0,1] → bucket index.
    pub fn bucket(h_normalized: f32) -> usize {
        let b = (h_normalized * BUCKET_COUNT as f32) as usize;
        b.min(BUCKET_COUNT - 1)
    }

    /// Record transition: current → new_h. Update matrix and advance state.
    pub fn observe(&mut self, new_h_normalized: f32) {
        let next = Self::bucket(new_h_normalized);
        self.transitions[self.current_bucket][next] += 1;
        self.total[self.current_bucket] += 1;
        self.current_bucket = next;
    }

    /// Salience vector for current bucket: P[current][j] for all j.
    /// Returns raw probabilities. Caller checks > SALIENCE_THRESHOLD.
    pub fn salience(&self) -> [f32; BUCKET_COUNT] {
        let mut s = [0.0_f32; BUCKET_COUNT];
        let t = self.total[self.current_bucket];
        if t == 0 {
            // No history: uniform prior
            let u = 1.0 / BUCKET_COUNT as f32;
            for j in 0..BUCKET_COUNT { s[j] = u; }
        } else {
            for j in 0..BUCKET_COUNT {
                s[j] = self.transitions[self.current_bucket][j] as f32 / t as f32;
            }
        }
        s
    }

    /// Returns bitmask of buckets that exceed SALIENCE_THRESHOLD.
    /// Bit j set → bucket j is a high-probability next state → prefetch.
    pub fn prefetch_mask(&self) -> u8 {
        let s = self.salience();
        let mut mask = 0u8;
        for j in 0..BUCKET_COUNT {
            if s[j] > SALIENCE_THRESHOLD { mask |= 1 << j; }
        }
        mask
    }

    /// Map a prefetch_mask to a list of asset group indices.
    /// Asset group g belongs to bucket (g % BUCKET_COUNT).
    /// Returns up to max_groups indices, sorted by salience (highest first).
    pub fn prioritized_groups(
        &self,
        total_groups: u32,
        max_groups: u32,
    ) -> heapless_vec::HeaplessVec<u32, 64> {
        let s = self.salience();
        let mut out = heapless_vec::HeaplessVec::new();

        // Build (salience, group_index) pairs, highest salience first
        // Simple insertion sort (total_groups typically < 32 for a title)
        let mut pairs: [(f32, u32); 64] = [(0.0, 0); 64];
        let n = (total_groups as usize).min(64);
        for g in 0..n {
            let b = g % BUCKET_COUNT;
            pairs[g] = (s[b], g as u32);
        }
        pairs[..n].sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

        for i in 0..(max_groups as usize).min(n) {
            if pairs[i].0 > SALIENCE_THRESHOLD {
                let _ = out.push(pairs[i].1);
            }
        }
        out
    }
}

// Minimal no_std compatible fixed-capacity vec (avoids alloc dependency)
mod heapless_vec {
    pub struct HeaplessVec<T: Copy + Default, const N: usize> {
        data: [T; N],
        len:  usize,
    }
    impl<T: Copy + Default, const N: usize> HeaplessVec<T, N> {
        pub fn new() -> Self { Self { data: [T::default(); N], len: 0 } }
        pub fn push(&mut self, val: T) -> Result<(), ()> {
            if self.len >= N { return Err(()); }
            self.data[self.len] = val;
            self.len += 1;
            Ok(())
        }
        pub fn as_slice(&self) -> &[T] { &self.data[..self.len] }
        pub fn len(&self) -> usize { self.len }
    }
}
pub use heapless_vec::HeaplessVec;
