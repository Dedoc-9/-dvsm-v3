// =============================================================================
// dvsm_gpu_supervisor.wgsl  |  DVSM-v3 GPU Supervisor Shader
// =============================================================================
// Role: Dispatch orchestration, occupancy gating, VRS hint writes.
//       NOT the math kernel — that lives in dvsm_gpu.wgsl.
//
// Execution order each frame tick (CPU drives sequence):
//   [1] dvsm_gpu.wgsl :: lie_bracket_pass      (math)
//   [2] dvsm_gpu.wgsl :: backreaction_pass      (math)
//   [3] dvsm_gpu.wgsl :: ema_pass               (math)
//   [4] THIS FILE     :: norm_reduction_pass    (reduce z_out → scalar norm)
//   [5] THIS FILE     :: vrs_hint_pass          (write VRS hint tile buffer)
//   [6] THIS FILE     :: ghost_scan_pass        (flag collapsed Z_k lanes)
//   [7] THIS FILE     :: occupancy_gate_pass    (suppress dispatch if over budget)
//
// Z2 Extreme occupancy model:
//   16 CU × 2 SIMD × 16 slots = 512 wave slots
//   DIM=16 workgroup = 1 wave. DVSM consumes 1/512 = 0.19% of capacity.
//   Occupancy gate is a safety valve for thermal throttle scenarios only.
//
// DEV NOTE — coarse-grained event boundaries:
//   Frame buckets (60/120/240 Hz) are model constructs, not physical limits.
//   The supervisor operates sub-frame. `dispatch_budget` is a continuous
//   watt-proportional value, not a hard frame counter.
//
// GFX target: gfx1150 (Z2 Extreme / Strix Point RDNA 3.5)
//             gfx1103 (Z1 Extreme / Phoenix RDNA 3)
//             Both compile identically from WGSL — driver handles target.
// =============================================================================

// ---------------------------------------------------------------------------
// BINDINGS  (Group 1 — supervisor layer; Group 0 is math kernel's bindings)
// ---------------------------------------------------------------------------

struct SupervisorParams {
    // Power-rail telemetry (updated CPU-side each frame from ally_x_power.rs)
    actual_watts:       f32,    // current measured APU draw
    tdp_ceiling:        f32,    // profile ceiling (15/25/35 W)
    thermal_headroom_c: f32,    // TjMax − T_current (°C)
    dispatch_budget:    f32,    // [0.0, 1.0] — fraction of CU budget available
                                // = actual_watts / tdp_ceiling, clamped

    // VRS parameters
    vrs_enabled:        u32,    // 0 = off, 1 = on
    vrs_tile_count:     u32,    // number of VRS hint tiles (screen-dependent)
    norm_variance:      f32,    // rolling σ²(‖Z‖²) from CPU RollingVariance
    ghost_threshold:    f32,    // |Z_k| < this → ghost flag

    // Replay
    frame_index:        u32,
    _pad:               u32,
};

// Supervisor uniform
@group(1) @binding(0) var<uniform>            sup: SupervisorParams;

// z_out from math kernel passes (read-only here)
@group(1) @binding(1) var<storage, read>       z_out:      array<f32, 16>;

// Outputs written by supervisor passes
@group(1) @binding(2) var<storage, read_write> norm_buf:   array<f32, 1>;
    // [0] = ‖Z‖² (scalar result of norm_reduction_pass)

@group(1) @binding(3) var<storage, read_write> vrs_hints:  array<u32>;
    // One u32 per tile. Encoding:
    //   0x00 = full rate (1×1)
    //   0x01 = half rate (2×2)
    //   0x02 = quarter rate (4×4)
    // Written by vrs_hint_pass, read by engine VRS hint buffer.

@group(1) @binding(4) var<storage, read_write> ghost_flags: array<u32, 1>;
    // Bit k set → Z[k] is a ghost candidate. CPU reads and triggers rebirth.
    // 16 bits used (DIM=16). Upper 16 bits: count of ghosts this frame.

@group(1) @binding(5) var<storage, read_write> gate_buf:   array<u32, 1>;
    // [0] = 1 → dispatch approved, 0 → suppressed (over thermal budget)
    // CPU checks this before submitting next frame's math kernel dispatch.

// ---------------------------------------------------------------------------
// PASS 4: norm_reduction_pass
// Reduce z_out[0..16] → ‖Z‖² scalar.
// Thread 0 does the reduction (DIM=16; no need for parallel tree at this size).
// Result written to norm_buf[0] and fed back to CPU for next frame's params.
//
// Math: ‖Z‖² = Σ_k Z_k²
// This is the same norm used in backreaction: B_k = −α(‖Z‖² − E_target)·Z_k
// We compute it GPU-side to avoid a CPU readback on the hot path.
// ---------------------------------------------------------------------------
@compute @workgroup_size(1, 1, 1)
fn norm_reduction_pass() {
    var n: f32 = 0.0;
    for (var k: u32 = 0u; k < 16u; k = k + 1u) {
        n = n + z_out[k] * z_out[k];
    }
    norm_buf[0] = n;
}

// ---------------------------------------------------------------------------
// PASS 5: vrs_hint_pass
// Write VRS hint per tile based on norm_variance (rolling σ²(‖Z‖²)).
//
// Rate decision (mirrors vrs_rate() in src/lib.rs — must stay in sync):
//   σ² < 0.02 → 0x01 (2×2, half rate) — stable region
//   σ² < 0.10 → 0x00 (1×1, full rate) — mild motion  [corrected: was 0x01]
//   σ² ≥ 0.10 → 0x00 (1×1, full rate) — high variance
//
// DEV NOTE: VRS hint tiles are model constructs. Tile boundaries do not
// correspond to physical scene features — they are a uniform grid imposed
// by the hint buffer layout. The supervisor does not know scene content;
// it uses norm_variance as a proxy for compute complexity, not pixel complexity.
// The driver applies hints; final tile rates are not guaranteed.
//
// Each thread handles one tile. vrs_tile_count tiles total.
// Workgroup: (64,1,1) — one wave covers 64 tiles per dispatch.
// ---------------------------------------------------------------------------
@compute @workgroup_size(64, 1, 1)
fn vrs_hint_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tile: u32 = gid.x;
    if tile >= sup.vrs_tile_count { return; }

    var rate: u32 = 0x00u; // default: full rate

    if sup.vrs_enabled != 0u {
        let v: f32 = sup.norm_variance;
        if v < 0.02 {
            rate = 0x01u; // 2×2 half rate — stable, low variance
        }
        // v >= 0.02: full rate (rate stays 0x00)
        // Note: quarter rate (0x02) reserved for explicit low-power override
        // triggered by dispatch_budget < 0.5 (thermal throttle path below)
        if sup.dispatch_budget < 0.5 && v < 0.05 {
            rate = 0x02u; // 4×4 quarter rate — thermal throttle + stable scene
        }
    }

    vrs_hints[tile] = rate;
}

// ---------------------------------------------------------------------------
// PASS 6: ghost_scan_pass
// Scan z_out for collapsed components (|Z_k| < ghost_threshold).
// Pack results into ghost_flags[0] as a 16-bit bitmask.
// Upper 16 bits = popcount (number of ghosts).
//
// CPU reads ghost_flags[0] each frame. If any bits set, GhostGuard::scan_and_rebirth()
// runs on the CPU side (rebirth requires EMA memory S which lives CPU-side).
//
// Why not rebirth GPU-side:
//   S (EMA memory) is in DVSMState on CPU. Copying S to GPU each frame for
//   a rare rebirth event is more expensive than the CPU rebirth itself.
//   At DIM=16 the rebirth loop is 16 multiplications — negligible CPU cost.
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 1, 1)
fn ghost_scan_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32 = lid.x;
    let is_ghost: u32 = select(0u, 1u, abs(z_out[k]) < sup.ghost_threshold);

    // Each thread writes its bit. Use workgroupUniformLoad pattern:
    // thread 0 assembles the final mask after all threads have written.
    // We use a simple approach: each thread atomically ORs its bit.
    // ghost_flags[0] must be zeroed by CPU before dispatch.
    if is_ghost != 0u {
        // Bit k in lower 16 bits
        atomicOr(&ghost_flags[0], 1u << k);
    }

    // Synchronize before popcount
    workgroupBarrier();

    // Thread 0 writes popcount into upper 16 bits
    if k == 0u {
        let mask: u32 = ghost_flags[0] & 0xFFFFu;
        let cnt:  u32 = countOneBits(mask);
        // OR in the count — lower 16 already set by atomic ops above
        atomicOr(&ghost_flags[0], cnt << 16u);
    }
}

// ---------------------------------------------------------------------------
// PASS 7: occupancy_gate_pass
// Single thread. Evaluates whether the next frame's math kernel dispatch
// should proceed or be suppressed due to thermal/power budget.
//
// Gate logic:
//   dispatch_budget = actual_watts / tdp_ceiling  (computed CPU-side)
//   thermal_headroom_c < 5°C → suppress (imminent throttle)
//   dispatch_budget < 0.20   → suppress (over 80% of TDP consumed elsewhere)
//   otherwise                → approve
//
// Writing gate_buf[0] = 0 does NOT cancel an in-flight dispatch.
// The CPU must check gate_buf[0] BEFORE submitting the next frame's
// lie_bracket_pass + backreaction_pass + ema_pass command buffer.
//
// Gravitational backreaction analogy:
//   The gate is a discrete event boundary — on/off per frame.
//   But thermal headroom is continuous. The suppression threshold (5°C, 0.20)
//   are model constructs. Real thermal margin varies by workload mix.
//   Do not treat gate_buf[0] == 1 as a guarantee of safe dispatch.
// ---------------------------------------------------------------------------
@compute @workgroup_size(1, 1, 1)
fn occupancy_gate_pass() {
    var approved: u32 = 1u;

    // Thermal headroom check
    if sup.thermal_headroom_c < 5.0 {
        approved = 0u;
    }

    // Power budget check
    if sup.dispatch_budget < 0.20 {
        approved = 0u;
    }

    gate_buf[0] = approved;
}
