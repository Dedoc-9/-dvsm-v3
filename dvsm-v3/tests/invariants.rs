// tests/invariants.rs
// Core mathematical invariants that must hold every build.
// These are the ONLY valid performance claims anchors.

use dvsm_v3::*;

// INV-1: Energy decay without backreaction
// d‖Z‖²/dt = −2λ‖Z‖²  (antisymmetric κ, α=0)
// After N steps: ‖Z‖² ≈ ‖Z_0‖² · exp(−2λ·N·dt)
#[test]
fn inv1_energy_decay() {
    let mut p = WattageProfile::ALLY_X_PERF;
    p.alpha = 0.0; // disable backreaction to test pure decay
    let mut s = DVSMState::new_identity();
    let z0_norm = s.norm_sq;
    for _ in 0..100 {
        dvsm_step(&mut s, &p);
    }
    let expected = z0_norm * (-2.0 * p.lambda * 100.0 * p.dt).exp();
    let ratio = s.norm_sq / expected;
    // Allow 2% deviation (Euler method truncation error)
    assert!((ratio - 1.0).abs() < 0.02, "energy decay deviated: ratio={}", ratio);
}

// INV-2: Backreaction moves norm closer to E_target than pure dissipation.
// Note: backreaction does not converge TO E_target (dissipation always decays
// toward zero). What it guarantees: starting above E_target, norm_sq reaches
// closer to E_target than the alpha=0 (pure exponential decay) baseline.
// Analytical check at t=500*(1/240)≈2.08s: with alpha=0.08 → ≈1.52,
// without → ≈2.43. Both verified against the Bernoulli ODE solution.
#[test]
fn inv2_backreaction_convergence() {
    let p_with        = WattageProfile::ALLY_X_PERF;
    let mut p_without = WattageProfile::ALLY_X_PERF;
    p_without.alpha   = 0.0;

    let mut s_with    = DVSMState::new_identity();
    let mut s_without = DVSMState::new_identity();
    for k in 0..DIM { s_with.z[k] *= 2.0; s_without.z[k] *= 2.0; }
    s_with.update_norm();
    s_without.update_norm();

    for _ in 0..500 {
        dvsm_step(&mut s_with,    &p_with);
        dvsm_step(&mut s_without, &p_without);
    }

    let dist_with    = (s_with.norm_sq    - p_with.e_target).abs();
    let dist_without = (s_without.norm_sq - p_with.e_target).abs();
    assert!(
        dist_with < dist_without,
        "backreaction should move norm closer to E_target than pure dissipation: \
         with={:.4} (dist {:.4}), without={:.4} (dist {:.4})",
        s_with.norm_sq, dist_with, s_without.norm_sq, dist_without
    );
}

// INV-3: Replay hash is deterministic
#[test]
fn inv3_replay_determinism() {
    let p = WattageProfile::ALLY_X_PERF;
    let mut s1 = DVSMState::new_identity();
    let mut s2 = DVSMState::new_identity();
    for _ in 0..50 {
        dvsm_step(&mut s1, &p);
        dvsm_step(&mut s2, &p);
    }
    assert_eq!(s1.replay_hash, s2.replay_hash,
        "replay hash diverged on identical input");
}

// INV-4: Ghost guard prevents Z_k ≡ 0 fixation
#[test]
fn inv4_ghost_rebirth() {
    let _p = WattageProfile::ALLY_X_PERF;
    let mut s = DVSMState::new_identity();
    let mut g = GhostGuard::new();
    // Force collapse
    for k in 0..DIM { s.z[k] = 0.0; s.s[k] = 0.5; }
    s.update_norm();
    let reborn = g.scan_and_rebirth(&mut s);
    assert!(reborn == DIM as u32, "expected {} rebirths, got {}", DIM, reborn);
    assert!(s.z[0].abs() > 0.0, "rebirth left Z[0] at zero");
}

// INV-5: Frame replay chain verification
#[test]
fn inv5_hash_chain_integrity() {
    let p = WattageProfile::ALLY_X_PERF;
    let mut sup = DVSMSupervisor::new(p);
    let mut records = Vec::new();
    for i in 0..20u64 {
        records.push(sup.tick(i * 4_167_000, (i + 1) * 4_167_000));
    }
    // Verify chain — all should pass
    let mut prev = 0u64;
    for r in &records {
        assert!(r.verify(prev), "hash chain broken at frame {}", r.frame_index);
        prev = r.hash_chain;
    }
    // Tamper with one record — should break
    let mut tampered = records[5];
    tampered.state_snap.z[0] += 0.1;
    assert!(!tampered.verify(records[4].hash_chain),
        "tampered record passed verification");
}
