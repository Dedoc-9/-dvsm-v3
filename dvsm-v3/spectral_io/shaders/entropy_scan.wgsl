// =============================================================================
// spectral_io/shaders/entropy_scan.wgsl  |  GPU-side entropy + mip hint pass
// =============================================================================
// Optional GPU path for entropy computation and mip hint writes.
// CPU path (entropy.rs) is sufficient at DIM=16.
// GPU path is useful when SpectralIOGovernor runs at DIM=64+ or higher.
//
// For Ally X at DIM=16: use CPU path. This shader is provided for
// future scaling and as documentation of the GPU-mapping of the math.
//
// PASSES:
//   Pass A: entropy_compute_pass  — H(Z) = −Σ p_k log₂(p_k)
//   Pass B: mip_hint_write_pass   — write per-mode mip bias to hint buffer
//
// Workgroup (DIM, 1, 1): one thread per mode.
// =============================================================================

struct EntropyParams {
    norm_sq:          f32,
    dt:               f32,
    h_prev:           f32,
    divergence_thresh: f32,
    diffuse_var_thresh: f32,
    collapse_thresh:   f32,
    _pad:             f32,
    _pad2:            f32,
};

@group(0) @binding(0) var<uniform>             ep:        EntropyParams;
@group(0) @binding(1) var<storage, read>        z_in:      array<f32, 16>;
@group(0) @binding(2) var<storage, read>        z_var:     array<f32, 16>; // per-mode Welford var
@group(0) @binding(3) var<storage, read>        z_mean:    array<f32, 16>; // per-mode mean |Z_k|
@group(0) @binding(4) var<storage, read_write>  h_out:     array<f32, 1>;  // H(Z) scalar
@group(0) @binding(5) var<storage, read_write>  mip_hints: array<u32, 16>; // per-mode mip bias

// ---------------------------------------------------------------------------
// PASS A: entropy_compute_pass
// Thread k computes p_k contribution.
// Thread 0 accumulates (serial at DIM=16; parallel reduction for DIM>32).
// ---------------------------------------------------------------------------
var<workgroup> partial_h: array<f32, 16>;

@compute @workgroup_size(16, 1, 1)
fn entropy_compute_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32 = lid.x;
    let p: f32 = (z_in[k] * z_in[k]) / max(ep.norm_sq, 1e-9);

    // p_k · log₂(p_k) contribution (0 if p_k ≈ 0)
    var contrib: f32 = 0.0;
    if p > 1e-9 {
        // log₂(x) = log(x) / log(2); WGSL has log() = natural log
        contrib = -p * (log(p) / 0.6931471806); // ln(2)
    }
    partial_h[k] = contrib;

    workgroupBarrier();

    // Thread 0 sums and writes H
    if k == 0u {
        var h: f32 = 0.0;
        for (var j: u32 = 0u; j < 16u; j = j + 1u) {
            h = h + partial_h[j];
        }
        h_out[0] = h;
    }
}

// ---------------------------------------------------------------------------
// PASS B: mip_hint_write_pass
// Thread k classifies mode k and writes mip bias.
//   Collapsed (mean < collapse_thresh)   → 2
//   Diffuse   (var  > diffuse_var_thresh) → 1
//   Echo                                  → 0
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 1, 1)
fn mip_hint_write_pass(@builtin(local_invocation_id) lid: vec3<u32>) {
    let k: u32  = lid.x;
    let mean: f32 = z_mean[k];
    let var_:  f32 = z_var[k];

    var bias: u32 = 0u;
    if mean < ep.collapse_thresh {
        bias = 2u;
    } else if var_ > ep.diffuse_var_thresh {
        bias = 1u;
    }
    mip_hints[k] = bias;
}
