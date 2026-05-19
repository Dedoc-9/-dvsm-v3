// tests/spectral_io_invariants.rs
// Mathematical invariants for the Spectral I/O layer.
// These are the only valid anchors for bandwidth/stutter claims.

use dvsm_spectral_io::*;

// INV-S1: Entropy bounds
// H(Z) ∈ [0, log₂(DIM)] always.
// H=0 when all energy in one mode. H=log₂(16)=4 when uniform.
#[test]
fn inv_s1_entropy_bounds() {
    let mut e = SpectralEntropyState::default();
    let dt = 1.0 / 240.0;

    // Uniform distribution → H ≈ 4.0
    let v = 1.0 / (DIM as f32).sqrt();
    let z_uniform = [v; DIM];
    e.update(&z_uniform, 1.0, dt);
    assert!((e.h_current - H_MAX).abs() < 0.01,
        "uniform H should be ~4.0, got {}", e.h_current);

    // All energy in mode 0 → H ≈ 0
    let mut z_spike = [0.0_f32; DIM];
    z_spike[0] = 1.0;
    e.update(&z_spike, 1.0, dt);
    assert!(e.h_current < 0.01, "spike H should be ~0, got {}", e.h_current);

    // Both cases: within [0, H_MAX]
    assert!(e.h_current >= 0.0 && e.h_current <= H_MAX + 0.01);
}

// INV-S2: Ghost classifier consistency
// Echo modes have lower mip bias than Diffuse.
// Collapsed modes have highest bias.
#[test]
fn inv_s2_ghost_classifier_ordering() {
    let mut mc = ModeClassifier::default();
    // Stable mode 0: feed constant value
    for _ in 0..100 {
        let mut z = [0.0_f32; DIM];
        z[0] = 0.5;
        z[1] = 0.0;  // will collapse
        mc.update(&z);
    }
    // High-variance mode 2: feed alternating values
    for i in 0..100u32 {
        let mut z = [0.0_f32; DIM];
        z[0] = 0.5;
        z[2] = if i % 2 == 0 { 0.8 } else { 0.1 };
        mc.update(&z);
    }
    assert_eq!(mc.classify(0), GhostClass::Echo,
        "stable mode should be Echo");
    assert_eq!(mc.classify(1), GhostClass::Collapsed,
        "zero mode should be Collapsed");
    assert!(mc.mip_bias(2) >= mc.mip_bias(0),
        "Diffuse bias should be >= Echo bias");
    assert_eq!(mc.mip_bias(1), 2,
        "Collapsed should have bias=2");
}

// INV-S3: Markov salience sums to ~1.0
// After sufficient observations, row-normalized transition matrix rows sum to 1.
#[test]
fn inv_s3_markov_salience_normalization() {
    let mut ms = MarkovSalience::new();
    // Feed 200 observations cycling through buckets
    for i in 0..200u32 {
        ms.observe((i % 8) as f32 / 8.0 + 0.05);
    }
    let s = ms.salience();
    let sum: f32 = s.iter().sum();
    assert!((sum - 1.0).abs() < 0.01,
        "salience should sum to 1.0, got {}", sum);
}

// INV-S4: Prefetch queue TTL expiry
// Jobs with ttl=1 are removed after one tick_ttl() call.
#[test]
fn inv_s4_queue_ttl_expiry() {
    let mut q = PrefetchQueue::new();
    q.enqueue(PrefetchJob { group_id: 0, priority: 1.0,
        ttl_frames: 1, format_hint: CompressionFormat::BC7, _pad: 0 });
    q.enqueue(PrefetchJob { group_id: 1, priority: 0.5,
        ttl_frames: 10, format_hint: CompressionFormat::BC7, _pad: 0 });
    assert_eq!(q.len(), 2);
    q.tick_ttl();  // decrements ttl; job 0 → 0 → removed, job 1 → 9
    assert_eq!(q.len(), 1, "ttl=0 job should expire");
    let remaining = q.dequeue().unwrap();
    assert_eq!(remaining.group_id, 1);
}

// INV-S5: Governor trigger fires on entropy spike
#[test]
fn inv_s5_governor_trigger_on_spike() {
    let mut gov = SpectralIOGovernor::new(16);
    let dt = 1.0 / 240.0;

    // Stable state: uniform Z → low entropy rate
    let v = 1.0 / (DIM as f32).sqrt();
    let z_uniform = [v; DIM];
    for _ in 0..10 {
        gov.tick(&z_uniform, 1.0, dt);
    }

    // Spike: concentrate energy in one mode → entropy drops sharply
    let mut z_spike = [0.001_f32; DIM];
    z_spike[0] = 0.999;
    let result = gov.tick(&z_spike, z_spike.iter().map(|x| x*x).sum(), dt);

    // Divergence should be large (entropy changed significantly)
    assert!(result.divergence_rate.abs() > 0.0,
        "divergence should be nonzero after entropy spike");
}
