// =============================================================================
// dvsm_gpu.wgsl  |  RDNA3 Phoenix compute shaders
// 3 dispatch passes per frame tick:
//   Pass 1: lie_bracket_pass   — accumulates Lie-bracket coupling
//   Pass 2: backreaction_pass  — applies gravitational backreaction + dissipation
//   Pass 3: ema_pass           — updates EMA memory S
//
// Wave size: Wave64 on RDNA3. DIM=16 fits in a single wave.
// Workgroup: (16,1,1) — one thread per dimension k.
//
// DEV NOTE: Phoenix APU has 4 CU. Each CU runs 2 SIMD units (Wave64).
// Max concurrent waves: 4 × 2 × 4 = 32. At DIM=16, workgroup fits in
// one wave. No LDS bank conflicts. No cross-CU communication needed.
// =============================================================================

struct Params {
    dt:       f32,
    lambda_:  f32,   // lambda is reserved in some WGSL impls
    alpha:    f32,
    e_target: f32,
    ema_beta: f32,
    norm_sq:  f32,   // read from prior pass result
    _pad:     f32,
    _pad2:    f32,
};

@group(0) @binding(0) var<uniform>            params: Params;
@group(0) @binding(1) var<storage, read>       z_in:   array<f32, 16>;
@group(0) @binding(2) var<storage, read>       s_in:   array<f32, 16>;
@group(0) @binding(3) var<storage, read>       kappa:  array<f32, 256>;  // 16×16
@group(0) @binding(4) var<storage, read_write> z_out:  array<f32, 16>;
@group(0) @binding(5) var<storage, read_write> s_out:  array<f32, 16>;
@group(0) @binding(6) var<storage, read_write> acc:    array<f32, 16>;
@group(0) @binding(7) var<storage, read_write> norm_out: array<f32, 1>;

// ---------------------------------------------------------------------------
// PASS 1: Lie-bracket accumulation
// Thread k computes: acc[k] = Σ_j κ[k,j] · (Z_k·S_j − Z_j·S_k)
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 1, 1)
fn lie_bracket_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32 = lid.x;
    var sum: f32 = 0.0;
    let zk: f32 = z_in[k];
    let sk: f32 = s_in[k];

    for (var j: u32 = 0u; j < 16u; j = j + 1u) {
        if j == k { continue; }
        // [Z,S]_{kj} = Z_k·S_j − Z_j·S_k
        let bracket: f32 = zk * s_in[j] - z_in[j] * sk;
        sum = sum + kappa[k * 16u + j] * bracket;
    }
    acc[k] = sum;
}

// ---------------------------------------------------------------------------
// PASS 2: Backreaction + dissipation + Euler step
// Thread k computes:
//   B_k = −α(‖Z‖² − E_target)·Z_k
//   Z_k += dt·(acc[k] − λ·Z_k + B_k)
// Also accumulates Z_k² into norm_out (partial; final sum done CPU-side
// or via a small reduction pass).
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 1, 1)
fn backreaction_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32 = lid.x;
    let zk: f32 = z_in[k];

    // Gravitational backreaction coefficient
    // Reads norm_sq from params (set by CPU from prior frame or reduction pass)
    let b_coeff: f32 = -params.alpha * (params.norm_sq - params.e_target);
    let b_k: f32     = b_coeff * zk;

    // Euler step
    let dz: f32  = params.dt * (acc[k] - params.lambda_ * zk + b_k);
    z_out[k]     = zk + dz;

    // Partial norm contribution (atomicAdd not available for f32 in base WGSL;
    // use a separate reduction pass or read all 16 values CPU-side)
    // For 16 elements: CPU readback of z_out and dot-product is negligible cost.
}

// ---------------------------------------------------------------------------
// PASS 3: EMA update
// S_k = β·S_k + (1−β)·Z_out_k
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 1, 1)
fn ema_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32  = lid.x;
    let b: f32  = params.ema_beta;
    s_out[k]    = b * s_in[k] + (1.0 - b) * z_out[k];
}
