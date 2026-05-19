// =============================================================================
// spectral_io/src/entropy.rs  |  Spectral Entropy + Ghost Classifier
// =============================================================================
// Computes H(Z): spectral entropy of the DVSM state vector.
// Primary trigger signal for the prefetch governor.
//
// MATH:
//   p_k = Z_k² / ‖Z‖²                       (normalized energy per mode)
//   H(Z) = −Σ_k p_k · log₂(p_k)             (bits; max = log₂(16) = 4.0)
//   dH/dt ≈ (H_t − H_{t−1}) / dt            (divergence rate)
//
//   Prefetch trigger: dH/dt > DIVERGENCE_THRESHOLD
//
// WHY PREDICTIVE:
//   Scene transitions redistribute energy across modes before the camera
//   crosses a boundary. Entropy rises 3–8 frames early at 240Hz.
//   Camera-distance heuristics fire after. This closes that gap.
//
// GHOST CLASSES (drive mip selection):
//   Echo      — |Z_k| variance low  → stable mode → high mip (full res)
//   Diffuse   — |Z_k| variance high → transient   → low mip  (bandwidth relief)
//   Collapsed — |Z_k| < threshold   → dead mode   → min mip
// =============================================================================

pub const DIM: usize = 16;
pub const H_MAX: f32 = 4.0;             // log₂(16)
pub const DIVERGENCE_THRESHOLD: f32 = 0.15;  // bits/frame at 240Hz — tune per title
pub const DIFFUSE_VAR_THRESHOLD: f32 = 0.004;
pub const COLLAPSE_THRESHOLD:    f32 = 0.01;
pub const PEAK_RESET_FRAMES:  u32 = 120;

// ---------------------------------------------------------------------------
// ENTROPY TRACKER
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct SpectralEntropyState {
    pub h_current:       f32,
    pub h_prev:          f32,
    pub divergence_rate: f32,  // dH/dt (bits/frame)
    pub peak_divergence: f32,
    pub peak_age:        u32,
}

impl SpectralEntropyState {
    /// Update from current Z. Returns true if prefetch should trigger.
    pub fn update(&mut self, z: &[f32; DIM], norm_sq: f32, dt: f32) -> bool {
        if norm_sq < 1e-9 {
            self.h_current = 0.0;
            self.divergence_rate = 0.0;
            return false;
        }
        let mut h = 0.0_f32;
        for k in 0..DIM {
            let p = (z[k] * z[k]) / norm_sq;
            if p > 1e-9 { h -= p * p.log2(); }
        }
        self.h_prev = self.h_current;
        self.h_current = h;
        self.divergence_rate = if dt > 0.0 { (h - self.h_prev) / dt } else { 0.0 };

        let abs_div = self.divergence_rate.abs();
        if abs_div > self.peak_divergence {
            self.peak_divergence = abs_div;
            self.peak_age = 0;
        } else {
            self.peak_age += 1;
            if self.peak_age > PEAK_RESET_FRAMES {
                self.peak_divergence = abs_div;
                self.peak_age = 0;
            }
        }
        abs_div > DIVERGENCE_THRESHOLD
    }

    pub fn h_normalized(&self) -> f32 { self.h_current / H_MAX }

    /// Estimated frames before scene transition peak.
    /// Conservative upper bound: min(gap/rate, 60).
    pub fn estimated_lead_frames(&self) -> u32 {
        let gap = self.peak_divergence - self.divergence_rate.abs();
        if gap <= 0.0 || self.divergence_rate.abs() < 1e-6 { return 0; }
        ((gap / self.divergence_rate.abs()) as u32).min(60)
    }
}

// ---------------------------------------------------------------------------
// GHOST CLASSIFIER (per-mode Welford variance → mip bias)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GhostClass {
    Echo      = 0,  // stable    → mip bias 0 (full res)
    Diffuse   = 1,  // transient → mip bias 1 (−1 mip)
    Collapsed = 2,  // dead      → mip bias 2 (−2 mip)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModeClassifier {
    pub n:    [u32; DIM],
    pub mean: [f32; DIM],
    pub m2:   [f32; DIM],
}

impl ModeClassifier {
    pub fn update(&mut self, z: &[f32; DIM]) {
        for k in 0..DIM {
            let x = z[k].abs();
            self.n[k] += 1;
            let d = x - self.mean[k];
            self.mean[k] += d / self.n[k] as f32;
            self.m2[k]   += d * (x - self.mean[k]);
        }
    }

    pub fn classify(&self, k: usize) -> GhostClass {
        if self.mean[k] < COLLAPSE_THRESHOLD { return GhostClass::Collapsed; }
        let var = if self.n[k] < 2 { 0.0 } else { self.m2[k] / self.n[k] as f32 };
        if var > DIFFUSE_VAR_THRESHOLD { GhostClass::Diffuse } else { GhostClass::Echo }
    }

    pub fn mip_bias(&self, k: usize) -> u8 { self.classify(k) as u8 }

    /// Aggregate: max bias across all modes → single streaming hint.
    pub fn global_mip_bias(&self) -> u8 {
        (0..DIM).map(|k| self.mip_bias(k)).max().unwrap_or(0)
    }

    /// Per-mode mip bias array — for fine-grained texture streaming.
    pub fn mip_bias_array(&self) -> [u8; DIM] {
        let mut out = [0u8; DIM];
        for k in 0..DIM { out[k] = self.mip_bias(k); }
        out
    }
}
