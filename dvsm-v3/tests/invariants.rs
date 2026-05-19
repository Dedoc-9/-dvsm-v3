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

// INV-2: Backreaction pulls norm toward E_target
#[test]
fn inv2_backreaction_convergence() {
    let p = WattageProfile::ALLY_X_PERF; // alpha=0.08, e_target=1.0
    let mut s = DVSMState::new_identity();
    // Perturb norm away from target
    for k in 0..DIM { s.z[k] *= 2.0; }
    s.update_norm(); // ‖Z‖² ≈ 4.0
    for _ in 0..500 {
        dvsm_step(&mut s, &p);
    }
    // Should converge toward 1.0 (within 10%)
    assert!((s.norm_sq - 1.0).abs() < 0.10,
        "backreaction failed to converge: norm_sq={}", s.norm_sq);
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
    let p = WattageProfile::ALLY_X_PERF;
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
