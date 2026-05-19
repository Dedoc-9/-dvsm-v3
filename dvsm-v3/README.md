# DVSM-v3

**Deterministic Variable-State Machine** — frame coherence supervisor for ROG Ally X (Z1 Extreme, RDNA3 Phoenix APU) with tunable wattage, frame generation, anti-ghosting, and cryptographic replay verification.

## Core Equation

```
dZ_k/dt = Σ_j κ_{kj}(Z_k·S_j − Z_j·S_k)  −  λ·Z_k  +  B_k(Z)
```

Where the **gravitational backreaction** term is:

```
B_k = −α · (‖Z‖² − E_target) · Z_k
```

Without B_k: dissipation alone collapses the norm under large λ.  
With B_k: ‖Z‖² → E_target regardless of λ. Energy conservation by soft constraint.

Energy identity (antisymmetric κ):
```
d‖Z‖²/dt = −2λ‖Z‖²   (coupling contributes zero; only dissipation and B_k matter)
```

## Hardware Reality

| Claim | Value | Source |
|-------|-------|--------|
| iGPU CUs | 4 | AMD Phoenix die |
| Concurrent waves | 32 max (4 CU × 2 SIMD × 4 waves) | RDNA3 occupancy model |
| Wave size | Wave64 | RDNA3 default |
| TDP range | 15–35 W | Ally X firmware |
| DIM=16 in one wave | Yes — no cross-CU comm needed | workgroup_size(16,1,1) |

No overclaims. 4 CUs ≠ discrete GPU.

## File Layout

```
dvsm-v3/
├── src/lib.rs                     Core: state, kernel, backreaction, ghost guard,
│                                  frame gen, VRS gate, replay hash chain
├── shaders/dvsm_gpu.wgsl          3 WGSL compute passes (lie_bracket, backreaction, ema)
├── include/dvsm.h                 C ABI header (engine integration)
├── platform/windows.rs            DX12 timestamps, registry control, power events, P99 ring
├── binary_api/
│   ├── abi/dvsm_abi_v3.h          Stable binary ABI contract
│   └── schemas/control.json       JSON control surface (UI/tools)
├── config/profiles/
│   ├── ally_x_perf.toml           35 W / 240 Hz
│   ├── ally_x_balanced.toml       25 W / 120 Hz
│   └── low_power.toml             15 W / 60 Hz
├── tools/hash_manifest.rs         SHA-256 build reproducibility
└── tests/invariants.rs            5 mathematical invariant tests
```

## Ghost Classification

Two distinct ghost types (important distinction):

| Ghost type | Location | Cause | Fix |
|------------|----------|-------|-----|
| **State ghost** | `src/lib.rs` GhostGuard | Z_k collapses to zero (false attractor) | Rebirth from EMA memory S_k |
| **Render ghost** | `src/lib.rs` FrameGenState | Synthetic frame prediction error | Anti-ghost check: ‖z_synth − z_actual‖ > threshold |

## Frame Replay & Security

Every frame produces a `FrameReplay` record with:
- Full state snapshot (DVSMState)
- SHA-like hash chain: `hash_chain_N = replay_hash_N XOR hash_chain_{N-1}`
- Tamper detection: any mutation of state_snap breaks the chain

Use `dvsm_verify_replay()` for:
- Anti-cheat: replay must match on identical input
- Debug: scrub frame-by-frame to pinpoint divergence
- Security audit: chain integrity proves no mid-flight mutation

## Wattage Tuning

Hot-swap profiles at runtime via `dvsm_set_profile()` or `on_power_event()`.  
Windows power event (AC→battery) automatically downgrades to LOW_POWER profile.

| Profile | TDP | dt | Frame gen | λ | α |
|---------|-----|----|-----------|---|---|
| ALLY_X_PERF | 35 W | 1/240 | interpolate | 0.12 | 0.08 |
| ALLY_X_BALANCED | 25 W | 1/120 | interpolate | 0.10 | 0.06 |
| LOW_POWER | 15 W | 1/60 | off | 0.08 | 0.04 |

## Building

```toml
# Cargo.toml consumer
[dependencies]
dvsm-v3 = { path = "path/to/dvsm-v3/src" }
```

```bash
cargo test                          # run 5 invariant tests
cargo build --release               # optimized; LTO; panic=abort
cargo build --target wasm32-unknown-unknown  # WASM for browser/Steam Deck tooling
```

## License

AGPL-3.0-or-later (open source default).  
Commercial dual-license available — see DUAL_LICENSE.md.
