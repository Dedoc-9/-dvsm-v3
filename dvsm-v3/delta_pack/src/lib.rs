// =============================================================================
// DVSM-v3  |  src/lib.rs
// Target   : ROG Ally X  (Z1 Extreme, Phoenix APU, RDNA3 iGPU 4 CU, 35 W TDP)
// Also     : Xbox / PC variable wattage tuning via WattageProfile
//
// CORE EQUATION (Lie-bracket dissipative system):
//
//   dZ_k/dt = Σ_j κ_{kj}(Z_k S_j − Z_j S_k) − λ Z_k  +  B_k(Z,W)
//
//   where B_k is the GRAVITATIONAL BACKREACTION term:
//
//        B_k = −α · (‖Z‖² − E_target) · Z_k
//
//   This enforces soft energy conservation: when the system drifts from
//   its target norm E_target, backreaction pushes it back.
//   Without this, dissipation alone causes norm collapse under large λ.
//
//   Energy identity (antisymmetric κ, with backreaction off):
//     d‖Z‖²/dt = −2λ‖Z‖²   (coupling term is energy-neutral by antisymmetry)
//   With backreaction:
//     d‖Z‖²/dt → 0  as  ‖Z‖² → E_target
//
// DESIGN NOTES (ghost awareness):
//   - "Ghost" = a basis vector that collapses to near-zero norm but should
//     survive. We track ghost candidates via GhostGuard and rebirth them
//     from EMA memory rather than zeroing. This prevents false attractors.
//   - Coarse-grained event boundaries (e.g. frame buckets) are model
//     constructs, not physical limits. The kernel operates sub-frame.
//   - Fixed-point Q31 used for shader-portable deterministic replay.
//     Q31 range: [-1, 1). For values outside, use Q16 with explicit scale.
// =============================================================================

#![no_std]
#![allow(clippy::excessive_precision)]

use core::f32::consts::PI;

// ---------------------------------------------------------------------------
// 1.  CONSTANTS  (all tunable via WattageProfile)
// ---------------------------------------------------------------------------

pub const DIM: usize       = 16;       // state dimension
pub const WAVE_SIZE: u32   = 64;       // RDNA3 Phoenix: Wave64 (4 CU × 2 SIMD × 4 waves = 32 waves)
pub const MAX_CU: u32      = 4;        // Ally X iGPU CU count — do NOT claim more
pub const MAX_WAVES: u32   = MAX_CU * 2 * 4;  // = 32 concurrent waves

// Fixed-point scale: Q31 → multiply by 2^31 to get integer
pub const Q31_SCALE: f32   = 2_147_483_648.0;  // 2^31

// ---------------------------------------------------------------------------
// 2.  WATTAGE PROFILE  (hardware-adaptive; drives λ, dt, frame budget)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WattageProfile {
    /// TDP ceiling in watts. Ally X: 15–35 W. Xbox: varies.
    pub tdp_watts:    f32,
    /// dt per kernel step (seconds). At 240 Hz: 1/240 ≈ 0.00417
    pub dt:           f32,
    /// Dissipation coefficient λ. Higher = faster norm decay.
    pub lambda:       f32,
    /// Backreaction strength α. Keeps ‖Z‖² near E_target.
    pub alpha:        f32,
    /// Target energy ‖Z‖² = E_target
    pub e_target:     f32,
    /// EMA memory coefficient β. S_k = β·S_k + (1−β)·Z_k
    pub ema_beta:     f32,
    /// Frame generation mode
    pub frame_gen:    FrameGenMode,
    /// VRS (Variable Rate Shading) enabled
    pub vrs_enabled:  bool,
}

impl WattageProfile {
    /// ROG Ally X performance mode (35 W)
    pub const ALLY_X_PERF: Self = Self {
        tdp_watts:  35.0,
        dt:         1.0 / 240.0,
        lambda:     0.12,
        alpha:      0.08,
        e_target:   1.0,
        ema_beta:   0.95,
        frame_gen:  FrameGenMode::Interpolate,
        vrs_enabled: true,
    };

    /// ROG Ally X balanced mode (25 W)
    pub const ALLY_X_BALANCED: Self = Self {
        tdp_watts:  25.0,
        dt:         1.0 / 120.0,
        lambda:     0.10,
        alpha:      0.06,
        e_target:   1.0,
        ema_beta:   0.93,
        frame_gen:  FrameGenMode::Interpolate,
        vrs_enabled: true,
    };

    /// Low power (15 W) — Ally X silent / Xbox eco
    pub const LOW_POWER: Self = Self {
        tdp_watts:  15.0,
        dt:         1.0 / 60.0,
        lambda:     0.08,
        alpha:      0.04,
        e_target:   1.0,
        ema_beta:   0.90,
        frame_gen:  FrameGenMode::Off,
        vrs_enabled: true,
    };
}

// ---------------------------------------------------------------------------
// 3.  FRAME GEN + DLSS ANALOG
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameGenMode {
    Off         = 0,
    Interpolate = 1,   // generate 1 synthetic frame between real frames
    Extrapolate = 2,   // predict next frame (higher latency risk)
}

/// Frame generation state — holds two prior frames for interpolation.
/// Ghosting risk: if motion vector is wrong, Z_synth ≠ true future state.
/// Anti-ghosting: GhostGuard checks ‖Z_synth − Z_actual‖ after real frame arrives.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameGenState {
    pub z_prev:   [f32; DIM],  // frame N-1
    pub z_curr:   [f32; DIM],  // frame N
    pub z_synth:  [f32; DIM],  // synthetic frame (interpolated/extrapolated)
    pub ghost_err: f32,        // ‖z_synth − z_next_actual‖ after correction
}

impl FrameGenState {
    pub fn new() -> Self {
        Self {
            z_prev:   [0.0; DIM],
            z_curr:   [0.0; DIM],
            z_synth:  [0.0; DIM],
            ghost_err: 0.0,
        }
    }

    /// Linear interpolate: z_synth = 0.5·z_prev + 0.5·z_curr
    /// More accurate than extrapolation; lower latency than waiting.
    pub fn interpolate(&mut self) {
        for k in 0..DIM {
            self.z_synth[k] = 0.5 * self.z_prev[k] + 0.5 * self.z_curr[k];
        }
    }

    /// Extrapolate: z_synth = 2·z_curr − z_prev  (velocity model)
    /// Risk: amplifies noise. Only use when latency budget allows correction.
    pub fn extrapolate(&mut self) {
        for k in 0..DIM {
            self.z_synth[k] = 2.0 * self.z_curr[k] - self.z_prev[k];
        }
    }

    /// Anti-ghost check: measures prediction error after real next frame.
    /// Returns true if ghost error exceeds threshold (triggers rebirth).
    pub fn check_ghost(&mut self, z_actual: &[f32; DIM], threshold: f32) -> bool {
        let mut err = 0.0_f32;
        for k in 0..DIM {
            let d = self.z_synth[k] - z_actual[k];
            err += d * d;
        }
        self.ghost_err = err.sqrt();
        self.ghost_err > threshold
    }

    /// Advance: shift curr → prev, write new real frame into curr.
    pub fn advance(&mut self, z_new: &[f32; DIM]) {
        self.z_prev = self.z_curr;
        self.z_curr = *z_new;
    }
}

// ---------------------------------------------------------------------------
// 4.  CORE STATE
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DVSMState {
    /// Primary state vector Z (16D)
    pub z:   [f32; DIM],
    /// EMA memory vector S: S_k = β·S_k + (1−β)·Z_k
    pub s:   [f32; DIM],
    /// κ coupling matrix (antisymmetric: κ_{kj} = −κ_{jk})
    /// Stored as upper triangle row-major; κ_{kj} accessed via kappa_get()
    pub kappa: [f32; DIM * DIM],
    /// Norm‖Z‖² (updated each step, used by backreaction and ghost guard)
    pub norm_sq: f32,
    /// Frame replay hash (rolling XOR of Q31 state — deterministic)
    pub replay_hash: u64,
}

impl DVSMState {
    pub fn new_identity() -> Self {
        let mut s = Self {
            z: [0.0; DIM],
            s: [0.0; DIM],
            kappa: [0.0; DIM * DIM],
            norm_sq: 0.0,
            replay_hash: 0,
        };
        // Diagonal Z = 1/√DIM so ‖Z‖² = 1
        let v = 1.0 / (DIM as f32).sqrt();
        for k in 0..DIM { s.z[k] = v; s.s[k] = v; }
        // Default antisymmetric κ: κ_{k,k+1} = 0.1, κ_{k+1,k} = −0.1
        for k in 0..(DIM - 1) {
            s.kappa[k * DIM + k + 1] =  0.1;
            s.kappa[(k + 1) * DIM + k] = -0.1;
        }
        s.norm_sq = 1.0;
        s
    }

    #[inline]
    pub fn kappa_get(&self, k: usize, j: usize) -> f32 {
        self.kappa[k * DIM + j]
    }

    /// Update norm — call after each step
    #[inline]
    pub fn update_norm(&mut self) {
        self.norm_sq = self.z.iter().map(|x| x * x).sum();
    }
}

// ---------------------------------------------------------------------------
// 5.  LIE-BRACKET KERNEL (core equation step)
//     Includes gravitational backreaction term B_k
// ---------------------------------------------------------------------------

/// Single integration step.
///
/// dZ_k/dt = Σ_j κ_{kj}(Z_k S_j − Z_j S_k) − λ Z_k + B_k
///
/// where B_k = −α(‖Z‖² − E_target) · Z_k
///
/// Dev note: the backreaction term is a NONLINEAR correction. It couples
/// every Z_k to the global norm. This means the system is NOT purely local —
/// a single component with high variance will damp the entire state.
/// That is intentional: it models energy redistribution across the wave.
pub fn dvsm_step(state: &mut DVSMState, p: &WattageProfile) {
    let mut acc = [0.0_f32; DIM];

    // --- Lie-bracket accumulation ---
    // O(DIM²) = 256 FMA ops. On Wave64 RDNA3: parallelizable to O(DIM) = 16 passes.
    for k in 0..DIM {
        let zk = state.z[k];
        let sk = state.s[k];
        for j in 0..DIM {
            if j == k { continue; }
            // [Z,S]_{kj} = Z_k·S_j − Z_j·S_k  (antisymmetric bracket)
            let bracket = zk * state.s[j] - state.z[j] * sk;
            acc[k] += state.kappa_get(k, j) * bracket;
        }
    }

    // --- Gravitational backreaction ---
    // B_k = −α · (‖Z‖² − E_target) · Z_k
    // This is the "mass-like" restoring force in state space.
    // When ‖Z‖² > E_target: B_k is negative (pushes norm down)
    // When ‖Z‖² < E_target: B_k is positive (pushes norm up)
    let backreaction_coeff = -p.alpha * (state.norm_sq - p.e_target);

    // --- Euler integration + EMA update ---
    for k in 0..DIM {
        let b_k = backreaction_coeff * state.z[k];
        let dz  = p.dt * (acc[k] - p.lambda * state.z[k] + b_k);
        state.z[k] += dz;
        // EMA memory: S_k tracks Z_k with lag
        state.s[k] = p.ema_beta * state.s[k] + (1.0 - p.ema_beta) * state.z[k];
    }

    state.update_norm();
    state.update_replay_hash();
}

impl DVSMState {
    /// Deterministic replay hash: XOR-fold Q31 representation of Z.
    /// Hash changes iff any Z_k changes by more than 1 ULP in Q31.
    /// Used for frame-by-frame replay verification and security audit.
    pub fn update_replay_hash(&mut self) {
        let mut h: u64 = self.replay_hash;
        for k in 0..DIM {
            // Q31 encode: clamp to [-1,1) then scale
            let clamped = self.z[k].clamp(-1.0 + 1e-7, 1.0 - 1e-7);
            let q = (clamped * Q31_SCALE) as i32 as u32;
            // Fibonacci-hashed fold to avoid correlation across lanes
            h ^= (q as u64).wrapping_mul(0x9e3779b97f4a7c15)
                           .wrapping_add((k as u64) << 32);
        }
        self.replay_hash = h;
    }
}

// ---------------------------------------------------------------------------
// 6.  GHOST GUARD
//     A "ghost" in this system = a Z_k that collapsed but should persist.
//     Different from frame-gen ghosts (those are rendering artifacts).
//     Here: ghost = spurious attractor at Z_k ≈ 0 when the basis should
//     remain active. We rebirth from EMA memory S_k.
// ---------------------------------------------------------------------------

pub struct GhostGuard {
    pub collapse_threshold: f32,  // |Z_k| < this → ghost candidate
    pub rebirth_scale:      f32,  // S_k rescaled by this on rebirth
    pub ghost_count:        u32,  // diagnostics
}

impl GhostGuard {
    pub fn new() -> Self {
        Self { collapse_threshold: 0.01, rebirth_scale: 0.5, ghost_count: 0 }
    }

    /// Scan Z for collapsed components. Rebirth from S (EMA memory).
    /// Returns number of rebirths this pass.
    pub fn scan_and_rebirth(&mut self, state: &mut DVSMState) -> u32 {
        let mut reborn = 0u32;
        for k in 0..DIM {
            if state.z[k].abs() < self.collapse_threshold {
                // Rebirth: restore from EMA memory at reduced scale
                // This preserves continuity — Z_k jumps to a known prior,
                // not a random reset. Critical for replay determinism.
                state.z[k] = state.s[k] * self.rebirth_scale;
                reborn += 1;
            }
        }
        self.ghost_count += reborn;
        reborn
    }
}

// ---------------------------------------------------------------------------
// 7.  VRS COHERENCE GATE
//     Decides whether Variable Rate Shading should reduce rate on a region.
//     Input: variance proxy from Z norm fluctuation.
// ---------------------------------------------------------------------------

/// VRS gate: returns shading rate multiplier (1.0 = full rate, 0.5 = half, etc.)
/// Variance proxy: use rolling σ²(‖Z‖²) as proxy for scene complexity.
///
/// Dev note: this is NOT pixel-level VRS. It is a compute-level gate that
/// feeds the VRS hint buffer. The driver still decides actual tile rates.
pub fn vrs_rate(norm_variance: f32, enabled: bool) -> f32 {
    if !enabled { return 1.0; }
    if norm_variance < 0.02 { 0.5 }       // stable region → half rate OK
    else if norm_variance < 0.10 { 0.75 } // mild motion → 3/4 rate
    else { 1.0 }                           // high variance → full rate
}

// ---------------------------------------------------------------------------
// 8.  TIMESTAMP / STALL PROFILER  (CPU-side surrogate; GPU path in platform/)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTimestamp {
    pub dispatch_ns:  u64,
    pub complete_ns:  u64,
    pub step_count:   u32,
}

impl FrameTimestamp {
    #[inline]
    pub fn delta_ns(&self) -> u64 { self.complete_ns - self.dispatch_ns }

    #[inline]
    pub fn delta_us(&self) -> f32 { self.delta_ns() as f32 / 1_000.0 }
}

// ---------------------------------------------------------------------------
// 9.  FRAME REPLAY RECORD
//     One record per real frame. Immutable after write. Used for:
//       - Frame-by-frame playback (scrub, debug)
//       - Security audit (hash chain integrity)
//       - Anti-cheat: replay hash must match on identical input
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameReplay {
    pub frame_index:  u64,
    pub timestamp:    FrameTimestamp,
    pub state_snap:   DVSMState,
    pub frame_gen_err: f32,        // ghost error from FrameGenState
    pub wattage_tdp:  f32,
    pub hash_chain:   u64,         // replay_hash XOR'd with prev hash_chain
}

impl FrameReplay {
    pub fn new(
        idx: u64,
        ts: FrameTimestamp,
        state: DVSMState,
        fge: f32,
        tdp: f32,
        prev_chain: u64,
    ) -> Self {
        let hash_chain = state.replay_hash ^ prev_chain;
        Self {
            frame_index: idx,
            timestamp: ts,
            state_snap: state,
            frame_gen_err: fge,
            wattage_tdp: tdp,
            hash_chain,
        }
    }

    /// Verify chain: given previous hash_chain, does this record's chain match?
    pub fn verify(&self, prev_chain: u64) -> bool {
        self.hash_chain == (self.state_snap.replay_hash ^ prev_chain)
    }
}

// ---------------------------------------------------------------------------
// 10.  ROLLING VARIANCE TRACKER  (feeds VRS gate + ghost detection)
//      Welford online algorithm — O(1) space, numerically stable.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct RollingVariance {
    pub n:    u32,
    pub mean: f32,
    pub m2:   f32,
}

impl RollingVariance {
    pub fn update(&mut self, x: f32) {
        self.n += 1;
        let delta  = x - self.mean;
        self.mean += delta / self.n as f32;
        let delta2 = x - self.mean;
        self.m2   += delta * delta2;
    }

    /// Population variance (n≥2 required; returns 0 otherwise)
    pub fn variance(&self) -> f32 {
        if self.n < 2 { 0.0 } else { self.m2 / self.n as f32 }
    }
}

// ---------------------------------------------------------------------------
// 11.  TOP-LEVEL SUPERVISOR TICK
//      One call per real frame. Orchestrates: step → ghost scan →
//      frame gen → VRS gate → replay record → hash chain.
// ---------------------------------------------------------------------------

pub struct DVSMSupervisor {
    pub state:       DVSMState,
    pub profile:     WattageProfile,
    pub frame_gen:   FrameGenState,
    pub ghost_guard: GhostGuard,
    pub norm_var:    RollingVariance,
    pub frame_idx:   u64,
    pub hash_chain:  u64,
}

impl DVSMSupervisor {
    pub fn new(profile: WattageProfile) -> Self {
        Self {
            state:       DVSMState::new_identity(),
            profile,
            frame_gen:   FrameGenState::new(),
            ghost_guard: GhostGuard::new(),
            norm_var:    RollingVariance::default(),
            frame_idx:   0,
            hash_chain:  0,
        }
    }

    /// Full supervisor tick. Returns FrameReplay record.
    pub fn tick(&mut self, dispatch_ns: u64, complete_ns: u64) -> FrameReplay {
        // 1. Core dynamics step (Lie-bracket + backreaction)
        dvsm_step(&mut self.state, &self.profile);

        // 2. Ghost guard pass
        self.ghost_guard.scan_and_rebirth(&mut self.state);

        // 3. Frame gen: advance gen state, synthesize next
        self.frame_gen.advance(&self.state.z);
        match self.profile.frame_gen {
            FrameGenMode::Interpolate => self.frame_gen.interpolate(),
            FrameGenMode::Extrapolate => self.frame_gen.extrapolate(),
            FrameGenMode::Off => {}
        }

        // 4. Anti-ghost check (0.05 = 5% norm error threshold)
        // On ghost detect: could trigger rebirth or flag to renderer
        let _ghost_triggered = self.frame_gen.check_ghost(
            &self.state.z.clone(),  // next "actual" is this frame's Z
            0.05,
        );

        // 5. Rolling variance of ‖Z‖² for VRS
        self.norm_var.update(self.state.norm_sq);

        // 6. Build timestamp record
        let ts = FrameTimestamp { dispatch_ns, complete_ns, step_count: 1 };

        // 7. Build replay record and advance hash chain
        let rec = FrameReplay::new(
            self.frame_idx,
            ts,
            self.state,
            self.frame_gen.ghost_err,
            self.profile.tdp_watts,
            self.hash_chain,
        );
        self.hash_chain = rec.hash_chain;
        self.frame_idx += 1;

        rec
    }

    /// VRS rate for current frame
    pub fn vrs_rate(&self) -> f32 {
        vrs_rate(self.norm_var.variance(), self.profile.vrs_enabled)
    }
}

// ---------------------------------------------------------------------------
// 12.  POWER-RAIL INTEGRATION HOOK
//      Called by engine each frame BEFORE tick().
//      Applies telemetry-scaled profile and builds GPU uniform payload.
//
//      Math recap — what gets patched and why:
//
//      b = actual_watts / tdp_ceiling  ∈ [0,1]
//
//      λ_actual = λ_base · (0.5 + 0.5·b)
//        → Dissipation never fully disabled (prevents norm explosion).
//        → At b=1: full dissipation. At b=0: half dissipation.
//
//      α_actual = α_base · b
//        → Backreaction proportional to available compute.
//        → At b=0: backreaction off. Norm drifts; doesn't fight.
//
//      d‖Z‖²/dt revised under throttle:
//        = −2·λ_actual·‖Z‖² − 2·α_actual·(‖Z‖²−E_target)·‖Z‖²
//        At b→0:
//        = −2·(0.5·λ_base)·‖Z‖²   (only half-strength decay remains)
//        Norm decays more slowly — correct, since we have less budget
//        to spend on enforcing E_target.
// ---------------------------------------------------------------------------

impl DVSMSupervisor {
    /// Apply power-rail telemetry to profile in-place.
    /// `actual_watts`: current measured APU draw.
    /// `thermal_headroom_c`: TjMax − T_current.
    /// Returns `dispatch_approved`: false = skip math kernel this frame.
    pub fn apply_telemetry(
        &mut self,
        actual_watts:       f32,
        thermal_headroom_c: f32,
    ) -> bool {
        let b = if self.profile.tdp_watts > 0.0 {
            (actual_watts / self.profile.tdp_watts).clamp(0.0, 1.0)
        } else { 0.0 };

        // Patch dissipation and backreaction (see math above)
        // NOTE: we patch the live profile, not the base. The platform layer
        // (ally_x_power.rs ProfilePatcher) owns the base and calls this.
        self.profile.lambda = self.profile.lambda * (0.5 + 0.5 * b);
        self.profile.alpha  = self.profile.alpha  * b;

        // Update norm variance for VRS
        self.norm_var.update(self.state.norm_sq);

        // Gate: suppress if thermal headroom < 5°C or budget < 20%
        thermal_headroom_c >= 5.0 && b >= 0.20
    }
}

// ---------------------------------------------------------------------------
// 13.  FPS LAYER INTEGRATION
//      Supervisor responds to StutterAction from CpuStutterGuard.
//      Scaling is temporary — restored after restore_after_frames.
// ---------------------------------------------------------------------------

impl DVSMSupervisor {
    /// Apply stutter action from fps_layer::CpuStutterGuard.
    /// base_lambda / base_alpha: the WattageProfile ceiling values (unscaled).
    /// Returns true if kernel dispatch should be skipped this frame.
    pub fn apply_stutter_action(
        &mut self,
        action: u8,          // 0=None,1=ReducePrefetch,2=SuspendPrefetch,3=Elevate
        base_lambda: f32,
        base_alpha:  f32,
    ) -> bool {
        match action {
            // None: restore if previously scaled
            0 => {
                self.profile.lambda = base_lambda;
                self.profile.alpha  = base_alpha;
                false
            }
            // ReducePrefetch: light scaling, no skip
            1 => {
                self.profile.lambda = base_lambda * 0.85;
                self.profile.alpha  = base_alpha  * 0.7;
                false
            }
            // SuspendPrefetch / ElevateThreadPriority: heavy scaling
            2 | 3 => {
                self.profile.lambda = base_lambda * 0.7;
                self.profile.alpha  = base_alpha  * 0.4;
                // Skip spectral IO governor this frame — return true to caller
                true
            }
            _ => false,
        }
    }
}
