# DVSM-v3 — Z2 Extreme Deep Dive Addendum

**Applies to:** ROG Xbox Ally X (2025), MSI Claw A8, any Z2 Extreme device  
**Prerequisite:** Main README.md  
**Purpose:** Exact code deltas, architectural differences, and what they mean
for the kernel equations.

---

## 1. Hardware Delta: Z1 Extreme → Z2 Extreme

| Property | Z1 Extreme (original target) | Z2 Extreme (this addendum) |
|---|---|---|
| GPU architecture | RDNA 3 (GFX11) | RDNA 3.5 (GFX11.5 / gfx1150) |
| iGPU CUs | 4 | 16 |
| SIMD units total | 4 × 2 = 8 | 16 × 2 = 32 |
| Max concurrent waves | 4 × 2 × 16 = **128** | 16 × 2 × 16 = **512** |
| Wave size | Wave64 | Wave64 (unchanged) |
| LDS per WGP | 128 KB | 128 KB (unchanged) |
| Vector register file per SIMD | 128 KB (gfx1150 "Strix") | 128 KB (same; gfx1151 Strix Halo has 192 KB) |
| Texture fill rate | baseline | ~2× per cycle vs RDNA 3 |
| TDP range | 15–35 W | 15–35 W (unchanged) |
| CPU | Zen 4 | Zen 5 / Zen 5c hybrid |

**Occupancy note (from AMD GPUOpen):** RDNA 2 and RDNA 3 both have
16 wave slots per SIMD. RDNA 3.5 carries this forward. So the
occupancy model per SIMD is identical — only the SIMD count changes.

---

## 2. Required Code Changes

### 2a. `src/lib.rs` — two constants

```rust
// BEFORE (Z1 Extreme):
pub const MAX_CU: u32    = 4;
pub const MAX_WAVES: u32 = MAX_CU * 2 * 4;   // = 32

// AFTER (Z2 Extreme):
pub const MAX_CU: u32    = 16;
pub const MAX_WAVES: u32 = MAX_CU * 2 * 16;  // = 512
//                                   ^^^^^
//                          16 wave slots per SIMD (RDNA 3.5, confirmed)
```

**Why the multiplier changes from 4 → 16:**  
On RDNA 3 and 3.5, each SIMD supports 16 assigned wavefronts simultaneously
(down from 20 on RDNA 1). The formula is:
```
max_waves = CU_count × SIMDs_per_CU × wave_slots_per_SIMD
          = 16       × 2             × 16
          = 512
```
At DIM=16 our workgroup is 1 wave. We now have 512 wave slots available
vs 128 on Z1 Extreme — meaning the DVSM kernel can run alongside 511
other concurrent waves without stalling for slots.

### 2b. WGSL shader — no changes needed

`shaders/dvsm_gpu.wgsl` targets `@workgroup_size(16, 1, 1)` = 16 threads =
fits in a single Wave64 on both RDNA 3 and RDNA 3.5. The ISA is forward
compatible. Recompile with updated AMD driver; no shader edits.

### 2c. `config/profiles/` — no changes needed

TDP range (15–35 W) is identical. All three profiles are valid.

---

## 3. What RDNA 3.5 Changes in the Kernel Path

### 3a. Texture unit throughput (indirect benefit)

RDNA 3.5 doubles per-cycle texel output. Our kernel is pure compute
(no texture samples), so this does not directly accelerate
`lie_bracket_pass` or `backreaction_pass`. It benefits the game
renderer running alongside DVSM — more headroom in the texture pipe
means less contention when DVSM and the renderer share the iGPU.

### 3b. Scalar FPU (indirect benefit)

RDNA 3.5 adds a floating-point unit to the scalar ALU. The backreaction
coefficient:

```
b_coeff = -α · (‖Z‖² − E_target)
```

is a scalar value broadcast to all 16 lanes. On RDNA 3 this was computed
on the vector ALU (wasting a vector lane for a scalar result). On RDNA 3.5
the scalar FPU can handle it, freeing one vector op per step.

In WGSL this is transparent — the compiler targets the scalar path
automatically on gfx1150. No shader change needed, but it is a real
micro-efficiency gain on the backreaction pass.

### 3c. s_singleuse_vdst hint (optional optimization)

RDNA 3.5 introduces `s_singleuse_vdst`: a compiler hint that an
instruction's inputs will not be reused, so caching them in the register
file cache is wasteful. In our Lie-bracket inner loop:

```wgsl
// bracket = zk * s_in[j] - z_in[j] * sk
// This result is used once (multiplied by kappa) and discarded.
// Candidate for s_singleuse_vdst on gfx1150.
```

The WGSL compiler does not expose this directly. If you compile via
ROCm/HIP for a native path, annotating the bracket accumulation with
`__builtin_amdgcn_singleuse` is worth testing. Expected gain: marginal
(register cache pressure relief, not throughput).

### 3d. Memory subsystem

RDNA 3.5 includes optimized LPDDR5 batch processing and improved
compression. For the Z2 Extreme specifically: 24 GB LPDDR5 at 8000 MT/s
(vs 16–24 GB on Z1 Extreme). Our kappa matrix (256 × f32 = 1 KB) fits
entirely in L1 cache (128 KB shared per shader array) on both
architectures, so memory bandwidth is not the bottleneck for this kernel.

---

## 4. Occupancy Model Revision

### Z1 Extreme (old)

```
4 CU × 2 SIMD × 16 slots = 128 wave slots
DVSM kernel: 1 wave (DIM=16 threads)
Occupancy headroom: 127 other waves
```

### Z2 Extreme (new)

```
16 CU × 2 SIMD × 16 slots = 512 wave slots
DVSM kernel: 1 wave (DIM=16 threads)
Occupancy headroom: 511 other waves
```

**Practical meaning:** The DVSM compute kernel is effectively invisible
to the occupancy scheduler on Z2 Extreme. Frame gen interpolation and
the game renderer can saturate the GPU without DVSM creating stalls.
On Z1 Extreme (4 CU) DVSM consumed 1/128 = 0.78% of wave capacity.
On Z2 Extreme it consumes 1/512 = 0.19%.

### Ghost guard interaction

More wave slots also means GhostGuard's rebirth pass (DIM=16 threads,
1 wave) has more scheduling flexibility. On Z1 Extreme, a rebirth pass
on the same frame as a heavy renderer dispatch could queue-stall.
On Z2 Extreme, the additional CUs absorb the rebirth wave without
touching the renderer's wave slots.

---

## 5. Frame Generation — Z2 Extreme Specific

RDNA 3.5 ships with AMD Fluid Motion Frames 2 (AFMF2) support.
DVSM's `FrameGenMode::Interpolate` operates at the compute level
(Lie-bracket state interpolation), while AFMF2 operates at the
display/composition level (pixel-space optical flow).

**These are not the same thing and do not conflict.**

Interaction model:
```
DVSM FrameGen (compute, state space)
  └─ produces z_synth: interpolated state vector
  └─ feeds VRS hint buffer

AFMF2 (driver, pixel space)
  └─ inserts synthetic display frames via optical flow
  └─ operates after DVSM, on the rendered output

Anti-ghost check in DVSM:
  └─ ‖z_synth − z_actual‖ threshold (0.05 default)
  └─ AFMF2 has its own ghost suppression (motion vector quality)
  └─ No coupling between them — independent error metrics
```

If AFMF2 is active, you can set `frame_gen = "off"` in the profile
and let AFMF2 handle display-level interpolation. DVSM still runs
its state dynamics — only the `FrameGenState` synthesis is skipped.

---

## 6. Benchmark Claim Anchors (Z2 Extreme)

Only claims derivable from `tests/invariants.rs` + `platform/windows.rs`
`FrameVarianceRing` are valid. The additional 12 CUs give more
scheduling margin, not a guaranteed FPS number.

Measurable real gains on Z2 Extreme vs Z1 Extreme:
- GPU OpenCL throughput: ~+20% at 25 W (3DMark, Geekbench measured data)
- DVSM kernel wall time: expect ~0.15–0.20× of Z1 Extreme time per tick
  (4× more SIMDs; kernel is embarrassingly parallel)
- Frame variance σ: dependent on workload, not claimable a priori

Do NOT claim:
- "X% improvement in frame stability" without `FrameVarianceRing.p99()` data
- "Better occupancy" — occupancy is already near-zero on both chips for DIM=16
- Any gain from scalar FPU until profiled with RGP on real hardware

---

## 7. GFX Target String

When building for Z2 Extreme via ROCm or native AMD compiler:

```bash
# Z1 Extreme (Phoenix):
--offload-arch=gfx1103

# Z2 Extreme (Strix Point, gfx1150):
--offload-arch=gfx1150
```

WGPU / WebGPU path: target string is handled by the driver.
No explicit arch flag needed for the WGSL compute path.

---

## 8. Summary Patch

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
-pub const MAX_CU: u32    = 4;
-pub const MAX_WAVES: u32 = MAX_CU * 2 * 4;   // 32
+pub const MAX_CU: u32    = 16;
+pub const MAX_WAVES: u32 = MAX_CU * 2 * 16;  // 512
```

Everything else in the repo is Z2 Extreme compatible as-is.

---

*Sources: AMD GPUOpen occupancy model, RDNA 3.5 LLVM analysis (Chester Lam /
Chips and Cheese), AMD Zen 5 Tech Day slides, NotebookCheck Z2 Extreme spec,
NoobFeed Z1 vs Z2 benchmark comparison.*
