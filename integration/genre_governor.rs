// =============================================================================
// integration/genre_governor.rs  |  Genre-aware supervisor configuration
// =============================================================================
// Configures WattageProfile, SpectralIOGovernor thresholds, and render budget
// parameters per game genre.
//
// WHY SEPARATE FROM WattageProfile:
//   WattageProfile owns hardware parameters (TDP, dt, λ, α).
//   GenreMode owns game-loop parameters (ghost threshold, divergence sensitivity,
//   prefetch TTL, resolution floor). They are orthogonal concerns.
//
// GENRE TRADEOFFS:
//
//   FpsCompetitive:  dt=1/360, ema_beta=0.88, ghost_threshold=0.03
//     Rationale: every frame is a potential shot registration. Input latency
//     matters more than smoothness. Fast EMA means S_k tracks Z_k closely →
//     GhostGuard fires sooner on prediction errors. Higher divergence_threshold
//     avoids false prefetch triggers on flick shots (intentional fast motion).
//
//   FpsTactical:  dt=1/240, ema_beta=0.91, ghost_threshold=0.04
//     Rationale: slower paced than competitive, allows occasional frame skips
//     during static aim (scope hold). Moderate thresholds.
//
//   MmoOpenWorld:  dt=1/120, ema_beta=0.97, ghost_threshold=0.06
//     Rationale: players tolerate higher input latency. High EMA smooths
//     Z trajectory across large-world panning. Aggressive prefetch (low
//     salience_threshold, long TTL) because asset loads are slow and painful.
//     Resolution floor at 0.50 — AFMF2 covers quality gap during pans.
//
//   MmoInstance:  dt=1/120, ema_beta=0.94, ghost_threshold=0.05
//     Rationale: confined spaces, many entities. Frequent entropy spikes from
//     combat VFX. Slightly higher divergence_threshold vs open world to reduce
//     false triggers during ability spam.
//
// DOUBLE PERFORMANCE PATH:
//   The ~2× effective FPS comes from combining three effects:
//   1. Resolution scale 0.67–0.75 on pre-detected transitions → ~30-40% cheaper render
//   2. AFMF2 2× interpolation (driver level) → doubles perceived FPS
//   3. Markov prefetch eliminates hitch frames that break the 2× illusion
//   These are independently valid; together they compound.
// =============================================================================

use dvsm_v3::{WattageProfile, FrameGenMode};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GenreMode {
    FpsCompetitive = 0,
    FpsTactical    = 1,
    MmoOpenWorld   = 2,
    MmoInstance    = 3,
}

impl GenreMode {
    pub fn is_fps(self) -> bool {
        matches!(self, GenreMode::FpsCompetitive | GenreMode::FpsTactical)
    }

    /// ‖z_synth − z_actual‖ threshold that triggers ghost guard.
    /// FPS needs it tighter — prediction errors manifest as input-response lag.
    pub fn ghost_threshold(self) -> f32 {
        match self {
            GenreMode::FpsCompetitive => 0.03,
            GenreMode::FpsTactical    => 0.04,
            GenreMode::MmoOpenWorld   => 0.06,
            GenreMode::MmoInstance    => 0.05,
        }
    }

    /// dH/dt threshold that triggers spectral prefetch.
    /// FPS has intentional fast motion (flick shots, sprints) that isn't a
    /// scene transition. Higher threshold avoids wasting prefetch bandwidth.
    pub fn divergence_threshold(self) -> f32 {
        match self {
            GenreMode::FpsCompetitive => 0.22,
            GenreMode::FpsTactical    => 0.18,
            GenreMode::MmoOpenWorld   => 0.12,
            GenreMode::MmoInstance    => 0.15,
        }
    }

    /// P(next_bucket) threshold for enqueuing a prefetch job.
    /// MMO open world: low threshold (aggressive) — world assets are large and slow.
    pub fn salience_threshold(self) -> f32 {
        match self {
            GenreMode::FpsCompetitive => 0.35,
            GenreMode::FpsTactical    => 0.30,
            GenreMode::MmoOpenWorld   => 0.18,
            GenreMode::MmoInstance    => 0.22,
        }
    }

    /// Prefetch job TTL in frames. MMO assets take longer to stream from storage.
    pub fn prefetch_ttl_frames(self) -> u16 {
        match self {
            GenreMode::FpsCompetitive => 8,
            GenreMode::FpsTactical    => 12,
            GenreMode::MmoOpenWorld   => 32,
            GenreMode::MmoInstance    => 20,
        }
    }

    /// Minimum render resolution scale during pre-emptive drops.
    /// FPS competitive never drops below 0.75 — aliasing hurts target identification.
    pub fn resolution_floor(self) -> f32 {
        match self {
            GenreMode::FpsCompetitive => 0.75,
            GenreMode::FpsTactical    => 0.67,
            GenreMode::MmoOpenWorld   => 0.50,
            GenreMode::MmoInstance    => 0.60,
        }
    }

    /// AFMF2 multiplier target (how many display frames per real rendered frame).
    /// FPS: 2× max (latency budget). MMO: 2× default, 4× on stable camera pans.
    pub fn afmf2_target_multiplier(self, norm_variance: f32) -> u8 {
        match self {
            GenreMode::FpsCompetitive => 2,
            GenreMode::FpsTactical    => 2,
            GenreMode::MmoOpenWorld   => if norm_variance < 0.02 { 4 } else { 2 },
            GenreMode::MmoInstance    => 2,
        }
    }

    /// WattageProfile tuned for this genre at the given TDP ceiling.
    pub fn wattage_profile(self, tdp_watts: f32) -> WattageProfile {
        match self {
            GenreMode::FpsCompetitive => WattageProfile {
                tdp_watts,
                dt:          1.0 / 360.0,
                lambda:      0.14,
                alpha:       0.10,
                e_target:    1.0,
                ema_beta:    0.88,
                frame_gen:   FrameGenMode::Interpolate,
                vrs_enabled: true,
            },
            GenreMode::FpsTactical => WattageProfile {
                tdp_watts,
                dt:          1.0 / 240.0,
                lambda:      0.12,
                alpha:       0.08,
                e_target:    1.0,
                ema_beta:    0.91,
                frame_gen:   FrameGenMode::Interpolate,
                vrs_enabled: true,
            },
            GenreMode::MmoOpenWorld => WattageProfile {
                tdp_watts,
                dt:          1.0 / 120.0,
                lambda:      0.09,
                alpha:       0.06,
                e_target:    1.0,
                ema_beta:    0.97,
                frame_gen:   FrameGenMode::Interpolate,
                vrs_enabled: true,
            },
            GenreMode::MmoInstance => WattageProfile {
                tdp_watts,
                dt:          1.0 / 120.0,
                lambda:      0.11,
                alpha:       0.07,
                e_target:    1.0,
                ema_beta:    0.94,
                frame_gen:   FrameGenMode::Interpolate,
                vrs_enabled: true,
            },
        }
    }
}

/// Top-level genre-aware handle. Owns the active mode and its derived profile.
pub struct GenreGovernor {
    pub mode:    GenreMode,
    pub profile: WattageProfile,
}

impl GenreGovernor {
    pub fn new(mode: GenreMode, tdp_watts: f32) -> Self {
        Self { mode, profile: mode.wattage_profile(tdp_watts) }
    }

    /// Hot-swap genre at runtime (e.g., hub world → instanced dungeon).
    /// Preserves TDP ceiling; replaces all dynamics parameters.
    pub fn switch_mode(&mut self, new_mode: GenreMode) {
        let tdp = self.profile.tdp_watts;
        self.mode    = new_mode;
        self.profile = new_mode.wattage_profile(tdp);
    }
}
