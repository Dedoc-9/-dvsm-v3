// =============================================================================
// platform/windows.rs  |  Windows integration layer
//
// DX12 timestamp wrapper, registry user control, power event hook,
// P99 frame variance ring. No kernel hooks. No scheduler override.
// We measure and adjust workload shape — not hardware control.
// =============================================================================

use crate::{DVSMSupervisor, WattageProfile};

// ---------------------------------------------------------------------------
// DX12 TIMESTAMP PAIR
// Real path: ID3D12QueryHeap (D3D12_QUERY_TYPE_TIMESTAMP)
// GPU clock ticks; convert with GetTimestampFrequency
// ---------------------------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Dx12TimestampPair {
    pub begin_ticks:      u64,
    pub end_ticks:        u64,
    pub gpu_frequency_hz: u64,
}

impl Dx12TimestampPair {
    pub fn delta_ns(&self) -> u64 {
        if self.gpu_frequency_hz == 0 { return 0; }
        let delta = self.end_ticks.saturating_sub(self.begin_ticks);
        delta.saturating_mul(1_000_000_000) / self.gpu_frequency_hz
    }
    pub fn delta_us(&self) -> f32 { self.delta_ns() as f32 / 1_000.0 }
}

// ---------------------------------------------------------------------------
// USER CONTROL (registry-backed)
// HKCU\Software\DVSM — no elevation required
// ---------------------------------------------------------------------------
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TdpPreset { LowPower = 0, Balanced = 1, Perf = 2 }

#[derive(Clone, Copy, Debug)]
pub struct WindowsUserControl {
    pub enabled:          bool,
    pub tdp_preset:       TdpPreset,
    pub frame_gen_enable: bool,
    pub vrs_enable:       bool,
    pub ghost_threshold:  f32,
}

impl WindowsUserControl {
    pub fn load_from_registry() -> Self {
        // TODO: winreg crate read from HKCU\Software\DVSM
        Self {
            enabled: true,
            tdp_preset: TdpPreset::Balanced,
            frame_gen_enable: true,
            vrs_enable: true,
            ghost_threshold: 0.05,
        }
    }
    pub fn to_wattage_profile(&self) -> WattageProfile {
        match self.tdp_preset {
            TdpPreset::LowPower => WattageProfile::LOW_POWER,
            TdpPreset::Balanced => WattageProfile::ALLY_X_BALANCED,
            TdpPreset::Perf     => WattageProfile::ALLY_X_PERF,
        }
    }
}

// Power event: call on WM_POWERBROADCAST / PBT_APMPOWERSTATUSCHANGE
pub fn on_power_event(sup: &mut DVSMSupervisor, on_battery: bool) {
    if on_battery {
        sup.profile = WattageProfile::LOW_POWER;
    } else {
        sup.profile = WindowsUserControl::load_from_registry()
                          .to_wattage_profile();
    }
}

// ---------------------------------------------------------------------------
// P99 FRAME VARIANCE RING  (256-frame rolling window)
// The ONLY valid source for performance claims. No ring = no claim.
// ---------------------------------------------------------------------------
pub const RING_SIZE: usize = 256;

pub struct FrameVarianceRing {
    pub buf:   [f32; RING_SIZE],
    pub head:  usize,
    pub count: usize,
}

impl FrameVarianceRing {
    pub fn new() -> Self { Self { buf: [0.0; RING_SIZE], head: 0, count: 0 } }

    pub fn push(&mut self, frame_us: f32) {
        self.buf[self.head] = frame_us;
        self.head = (self.head + 1) % RING_SIZE;
        if self.count < RING_SIZE { self.count += 1; }
    }

    pub fn mean(&self) -> f32 {
        if self.count == 0 { return 0.0; }
        self.buf[..self.count].iter().sum::<f32>() / self.count as f32
    }

    pub fn variance(&self) -> f32 {
        if self.count < 2 { return 0.0; }
        let m = self.mean();
        self.buf[..self.count].iter().map(|x| (x - m).powi(2)).sum::<f32>()
            / self.count as f32
    }

    // Sorts a copy — call for diagnostics only, not on hot path
    pub fn p99(&self) -> f32 {
        if self.count == 0 { return 0.0; }
        let mut tmp = [0.0_f32; RING_SIZE];
        tmp[..self.count].copy_from_slice(&self.buf[..self.count]);
        tmp[..self.count].sort_by(|a, b| a.partial_cmp(b).unwrap());
        tmp[((self.count as f32 * 0.99) as usize).min(self.count - 1)]
    }
}
