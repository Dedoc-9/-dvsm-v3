// =============================================================================
// integration/render_budget.rs  |  Pre-emptive render quality scaling
// =============================================================================
// Uses SpectralIOGovernor's Markov lead time to drop render resolution BEFORE
// a scene transition peaks, then lets AFMF2 cover the quality gap at display res.
//
// MECHANISM (how ~2× effective FPS is achieved):
//
//   Frame budget without DVSM:
//     Render at native (1.0) → AFMF2 2× → 2× perceived FPS
//     Cost: 1 full render per 2 display frames.
//
//   Frame budget with DVSM pre-drop (non-transition stable window):
//     Render at 0.67–0.75 → AFMF2 2× → 2× perceived FPS
//     Cost: ~0.55–0.65 of a full render per 2 display frames.
//     Saved budget: route to frequency boost or additional AFMF2 headroom.
//
//   During scene transition (entropy peak):
//     Render at native (1.0) — AFMF2 can't be trusted here.
//     DVSM detected this 3–8 frames early; engine had time to preload assets.
//     No stutter hitch because Markov prefetch already loaded next-scene assets.
//
// CONSTRAINTS (non-negotiable):
//   - FPS competitive: floor 0.75, never skip frames (shot registration latency)
//   - FPS competitive: never drop during high norm_variance (fast movement)
//   - No two consecutive skipped frames (AFMF2 can't chain two interpolations)
//   - Scale changes are EMA-smoothed to avoid visible resolution flicker
//
// RESOLUTION SCALE → RENDER COST (approximate, RDNA3.5 shader throughput):
//   1.00 → baseline
//   0.75 → ~0.56× cost  (area = 0.5625)
//   0.67 → ~0.45× cost
//   0.50 → ~0.25× cost
//
// IMPORTANT: This module outputs a recommendation. The engine must implement
// dynamic resolution scaling (DRS) and pass resolution_scale to its DRS system.
// DVSM does not directly control the renderer.
// =============================================================================

use crate::integration::genre_governor::GenreMode;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RenderBudgetOutput {
    /// Target render resolution scale [floor, 1.0].
    /// Feed to engine DRS system (e.g., UE5 r.ScreenPercentage).
    pub resolution_scale: f32,

    /// True if this frame can be skipped (AFMF2 covers it from prior real frame).
    /// Engine should submit no draw calls when true.
    /// Never true for FpsCompetitive, never true on consecutive frames.
    pub skip_frame: bool,

    /// AFMF2 target multiplier (display frames per real frame): 2 or 4.
    pub afmf2_multiplier: u8,

    /// Frames until expected scene transition peak.
    /// Engine can use this to delay expensive LOD switches until after the peak.
    pub transition_lead_frames: u32,
}

pub struct RenderBudgetAllocator {
    /// EMA-smoothed resolution scale. Avoids single-frame resolution jumps.
    current_scale: f32,
    /// EMA coefficient. Higher = slower scale changes (less flicker).
    scale_ema: f32,
    /// Consecutive skipped frames counter. Skip only allowed at 0.
    consecutive_skips: u32,
}

impl RenderBudgetAllocator {
    pub fn new() -> Self {
        Self { current_scale: 1.0, scale_ema: 0.80, consecutive_skips: 0 }
    }

    pub fn tick(
        &mut self,
        divergence_rate:   f32,
        lead_frames:       u32,
        norm_variance:     f32,
        dispatch_budget:   f32,   // [0,1] from PowerTelemetrySample
        h_normalized:      f32,
        genre:             GenreMode,
    ) -> RenderBudgetOutput {
        let target = self.target_scale(
            divergence_rate, lead_frames, norm_variance, dispatch_budget, genre,
        );

        // EMA smooth: prevents visible frame-to-frame resolution flicker
        self.current_scale = self.scale_ema * self.current_scale
            + (1.0 - self.scale_ema) * target;
        // Clamp to genre floor regardless of EMA state
        self.current_scale = self.current_scale.max(genre.resolution_floor());

        let skip = self.should_skip(norm_variance, lead_frames, h_normalized, genre);
        if skip {
            self.consecutive_skips += 1;
        } else {
            self.consecutive_skips = 0;
        }

        RenderBudgetOutput {
            resolution_scale:      self.current_scale,
            skip_frame:            skip,
            afmf2_multiplier:      genre.afmf2_target_multiplier(norm_variance),
            transition_lead_frames: lead_frames,
        }
    }

    fn target_scale(
        &self,
        divergence_rate: f32,
        lead_frames:     u32,
        norm_variance:   f32,
        dispatch_budget: f32,
        genre:           GenreMode,
    ) -> f32 {
        let floor = genre.resolution_floor();

        // FPS: never drop during fast movement — aliasing breaks target ID
        if genre.is_fps() && norm_variance > 0.10 {
            return 1.0;
        }

        // Need ≥2 frames of lead to pre-drop without visible stutter.
        // If transition is at peak or past it, hold native for clean AFMF2 reference.
        if lead_frames < 2 {
            return 1.0;
        }

        // Pressure from two sources:
        //   div_pressure: how far past divergence threshold we are (transition strength)
        //   thermal_pressure: how much headroom we've spent (power budget tightness)
        let div_pressure   = (divergence_rate.abs() / (genre.divergence_threshold() * 3.0))
                                .min(1.0);
        let thermal_pressure = (1.0 - dispatch_budget).clamp(0.0, 1.0);

        // Blend: transition drives 70% of the drop, thermal 30%
        let combined = div_pressure * 0.70 + thermal_pressure * 0.30;
        let drop = combined * (1.0 - floor);

        (1.0 - drop).max(floor)
    }

    fn should_skip(
        &self,
        norm_variance: f32,
        lead_frames:   u32,
        h_normalized:  f32,
        genre:         GenreMode,
    ) -> bool {
        // FPS competitive: never skip — every frame could register a hit
        if matches!(genre, GenreMode::FpsCompetitive) {
            return false;
        }
        // No consecutive skips — AFMF2 can't chain two interpolations reliably
        if self.consecutive_skips > 0 {
            return false;
        }
        // Don't skip during movement
        if norm_variance > 0.05 {
            return false;
        }
        // Don't skip too close to a transition peak (need real frames for AFMF2 ref)
        if lead_frames > 0 && lead_frames <= 3 {
            return false;
        }
        // Don't skip in complex scenes (high entropy = AFMF2 less reliable)
        if h_normalized > 0.70 {
            return false;
        }
        true
    }
}
