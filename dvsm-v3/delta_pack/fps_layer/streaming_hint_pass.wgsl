// =============================================================================
// fps_layer/streaming_hint_pass.wgsl
// =============================================================================
// Single compute pass: writes a streaming priority hint buffer each frame.
// Consumed by the engine's asset streaming thread to gate decompression jobs.
//
// WHY THIS EXISTS SEPARATELY FROM dvsm_gpu_supervisor.wgsl:
//   The supervisor shader runs every frame regardless of CPU state.
//   This pass runs ONLY when cpu_bound == false (GPU has headroom).
//   When BF6 is CPU-bound, this pass is skipped entirely — no iGPU
//   overhead added on top of an already-saturated simulation thread.
//   The CpuStutterGuard in cpu_stutter_guard.rs controls the skip.
//
// INPUTS:
//   hint_params: divergence_rate, h_normalized, global_mip_bias, cpu_bound
//   mip_bias_in: per-mode [16] bias from entropy.rs ModeClassifier
//
// OUTPUT:
//   stream_priority[N]: u32 per asset slot.
//     Encoding:
//       bits [0..7]  = priority score 0–255 (255 = prefetch immediately)
//       bits [8..9]  = mip_drop (0=keep, 1=drop1, 2=drop2)
//       bits [10]    = stale flag (set if ttl expired, engine can evict)
//       bits [11..31]= reserved
//
//   N = stream_slot_count (engine-defined, typically 64–256 asset slots)
//
// WORKGROUP: (64,1,1) — one thread per asset slot.
// At 256 slots: 4 dispatches of 64. On Z2 Extreme 16-CU iGPU: negligible.
// =============================================================================

struct HintParams {
    divergence_rate:  f32,
    h_normalized:     f32,
    global_mip_bias:  u32,
    cpu_bound:        u32,   // 1 = CPU-bound; pass should have been skipped by CPU
    frame_index:      u32,
    stream_slot_count: u32,
    _pad:             f32,
    _pad2:            f32,
};

@group(0) @binding(0) var<uniform>            hp:           HintParams;
@group(0) @binding(1) var<storage, read>       mip_bias_in:  array<u32, 16>;
@group(0) @binding(2) var<storage, read>       slot_ttl:     array<u32>;  // remaining TTL per slot
@group(0) @binding(3) var<storage, read_write> stream_priority: array<u32>; // output

@compute @workgroup_size(64, 1, 1)
fn streaming_hint_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slot: u32 = gid.x;
    if slot >= hp.stream_slot_count { return; }

    // Safety valve: if cpu_bound was set, CPU should have skipped dispatch.
    // If we're here anyway, write neutral hints (priority=0, no drop).
    if hp.cpu_bound != 0u {
        stream_priority[slot] = 0u;
        return;
    }

    // Map slot → mode bucket (slot % 16 → mip_bias_in index)
    let mode_idx: u32 = slot % 16u;
    let mip_drop: u32 = mip_bias_in[mode_idx];

    // Priority score from divergence_rate and h_normalized
    // High divergence + high entropy → high priority (imminent scene change)
    // Clamp divergence contribution to [0, 1] range
    let div_contrib: f32 = clamp(hp.divergence_rate / 0.5, 0.0, 1.0);
    let h_contrib:   f32 = hp.h_normalized;
    let raw_priority: f32 = (div_contrib * 0.7 + h_contrib * 0.3) * 255.0;
    let priority: u32 = u32(clamp(raw_priority, 0.0, 255.0));

    // Stale flag: ttl == 0
    let stale: u32 = select(0u, 1u, slot_ttl[slot] == 0u);

    // Pack output word
    //   bits 0–7:  priority
    //   bits 8–9:  mip_drop
    //   bit  10:   stale
    let out: u32 = (priority & 0xFFu)
                 | ((mip_drop & 0x3u) << 8u)
                 | ((stale    & 0x1u) << 10u);

    stream_priority[slot] = out;
}
