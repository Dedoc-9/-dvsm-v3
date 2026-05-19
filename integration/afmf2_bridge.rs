// =============================================================================
// integration/afmf2_bridge.rs  |  DVSM → AFMF2 confidence signal bridge
// =============================================================================
// AFMF2 (AMD Fluid Motion Frames 2) runs in pixel space using optical flow.
// It interpolates display frames but has no visibility into scene-transition
// timing — it only sees the frame it just rendered.
//
// DVSM detects scene transitions 3–8 frames early via H(Z) divergence rate.
// This bridge converts that early-warning signal into AFMF2 confidence weights,
// suppressing optical flow trust before it would otherwise hallucinate.
//
// KEY INSIGHT FROM DEBATE:
//   AFMF2 and DVSM FrameGen are NOT the same thing. AFMF2 operates post-render
//   in display space. DVSM operates in abstract state space. They don't conflict.
//   This bridge makes AFMF2 smarter using DVSM's predictive signal.
//
// HOW TO WIRE:
//   1. After SpectralIOGovernor::tick(), call Afmf2Signal::compute().
//   2. Pass Afmf2Signal to the AMD AFMF2 driver API:
//        agsDriverExtensionsDX12_SetAFMF2FrameGenerationConfig(
//            context, &afmf2_config);
//      Set config.confidenceScale = signal.confidence_scalar.
//      Set config.enableInterpolation = signal.reference_frame_ok().
//   3. When signal.transition_imminent: pre-reduce render resolution
//      (handled by render_budget.rs) so AFMF2's upscaling covers the gap.
//
// CONFIDENCE MATH:
//   confidence = clamp(1.0 − (|dH/dt| − 0.15) / 0.45, 0.0, 1.0)
//     dH/dt < 0.15  → confidence = 1.0   (stable, trust AFMF2 fully)
//     dH/dt = 0.30  → confidence = 0.67  (mild transition, moderate trust)
//     dH/dt = 0.60  → confidence = 0.0   (hard transition, suppress AFMF2)
//
//   Static scene floor: if norm_variance < 0.02 AND h_normalized < 0.3,
//   confidence is floored at 0.85 regardless of divergence (avoids
//   suppressing AFMF2 on false spikes in completely static scenes).
// =============================================================================

use crate::integration::genre_governor::GenreMode;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Afmf2Signal {
    /// Optical flow confidence for AFMF2 [0.0, 1.0].
    /// 1.0 = interpolate freely. 0.0 = suppress (scene change imminent).
    /// Feed to AMD AFMF2 driver config as confidenceScale.
    pub confidence_scalar: f32,

    /// True when |dH/dt| > divergence_threshold AND lead_frames > 0.
    /// AFMF2 should reduce interpolation aggressiveness; render_budget.rs
    /// will pre-drop resolution to compensate.
    pub transition_imminent: bool,

    /// Frames until estimated entropy peak (from SpectralEntropyState).
    /// 0 = at peak or past it. Used by render_budget to time resolution recovery.
    pub lead_frames: u32,

    /// Per-VRS-tile confidence [0=full, 1=reduced, 2=low].
    /// Matches VRS hint tile layout. Pass to AFMF2 per-region weighting.
    /// Tiles marked 2 are quarter-rate — AFMF2 should not trust optical flow there.
    pub tile_confidence: [u8; 16],

    /// The previous rendered frame had ghost_err > threshold.
    /// Do NOT use it as AFMF2 reference frame — it contains a rebirth discontinuity.
    pub prior_frame_ghosted: bool,

    /// H(Z) normalized to [0,1] (H_MAX = log₂(16) = 4.0).
    /// Low = energy concentrated in few modes (simple scene).
    /// High = energy spread across modes (complex scene transition).
    pub h_normalized: f32,
}

impl Afmf2Signal {
    pub fn compute(
        divergence_rate:        f32,
        divergence_threshold:   f32,   // from GenreMode::divergence_threshold()
        lead_frames:            u32,
        h_normalized:           f32,
        norm_variance:          f32,
        prior_frame_ghost_err:  f32,
        ghost_err_threshold:    f32,   // from GenreMode::ghost_threshold()
        vrs_hints:              &[u8; 16],
    ) -> Self {
        let div_abs = divergence_rate.abs();

        // Confidence falls from 1.0 as divergence climbs past threshold.
        // 3× threshold = full suppression.
        let suppression_range = divergence_threshold * 3.0;
        let raw_confidence = if div_abs <= divergence_threshold {
            1.0_f32
        } else {
            1.0 - (div_abs - divergence_threshold) / suppression_range
        };

        // Static-scene floor: don't suppress AFMF2 on stable frames.
        let confidence_scalar = if norm_variance < 0.02 && h_normalized < 0.30 {
            raw_confidence.clamp(0.85, 1.0)
        } else {
            raw_confidence.clamp(0.0, 1.0)
        };

        Self {
            confidence_scalar,
            transition_imminent: div_abs > divergence_threshold && lead_frames > 0,
            lead_frames,
            tile_confidence: *vrs_hints,
            prior_frame_ghosted: prior_frame_ghost_err > ghost_err_threshold,
            h_normalized,
        }
    }

    /// True when this frame is a safe AFMF2 reference (can interpolate FROM it).
    /// False on ghost-rebirth frames (discontinuity in Z breaks optical flow).
    pub fn reference_frame_ok(&self) -> bool {
        !self.prior_frame_ghosted && self.confidence_scalar > 0.50
    }

    /// Suggested AFMF2 frame gen mode string for driver API.
    pub fn suggested_mode(&self) -> &'static str {
        if !self.reference_frame_ok() {
            "disabled"
        } else if self.confidence_scalar > 0.80 {
            "interpolate"
        } else {
            "reduce_confidence"
        }
    }
}

/// Per-frame coordinator: combines DVSM and SpectralIO outputs into one signal.
pub struct Afmf2Bridge {
    /// VRS hint tile snapshot (16 entries matching supervisor shader output).
    vrs_snapshot: [u8; 16],
}

impl Afmf2Bridge {
    pub fn new() -> Self {
        Self { vrs_snapshot: [0u8; 16] }
    }

    /// Update VRS hint snapshot from the GPU supervisor output.
    /// Call after occupancy_gate_pass reads back vrs_hints[].
    pub fn update_vrs(&mut self, hints: &[u8; 16]) {
        self.vrs_snapshot = *hints;
    }

    /// Build the AFMF2 signal for this frame.
    pub fn build(
        &self,
        divergence_rate:       f32,
        lead_frames:           u32,
        h_normalized:          f32,
        norm_variance:         f32,
        prior_ghost_err:       f32,
        genre:                 GenreMode,
    ) -> Afmf2Signal {
        Afmf2Signal::compute(
            divergence_rate,
            genre.divergence_threshold(),
            lead_frames,
            h_normalized,
            norm_variance,
            prior_ghost_err,
            genre.ghost_threshold(),
            &self.vrs_snapshot,
        )
    }
}
