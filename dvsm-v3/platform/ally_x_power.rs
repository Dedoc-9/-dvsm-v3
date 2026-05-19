// =============================================================================
// platform/ally_x_power.rs  |  Ally X / Z2 Extreme power-rail telemetry bridge
// =============================================================================
// Reads actual APU power draw and thermal headroom via ASUS WMI / ACPI,
// then continuously patches WattageProfile fields so the dynamics kernel
// tracks real available compute budget rather than static TDP presets.
//
// WMI device IDs (ASUS_WMI_DEVID_*):
//   0x001200C0  PPT_LIMIT_APU   — sustained power limit (SPL), watts
//   0x001200C1  PPT_APU_SPPT    — slow PPT (average over ~30s window)
//   0x001200C2  PPT_APU_FPPT    — fast PPT (burst ceiling)
//   0x00110019  GPU_TEMP        — iGPU die temperature (°C × 1000 on some FW)
//   0x00110020  CPU_TEMP        — CPU die temperature
//
// On ROG Xbox Ally X (Z2 Extreme): Armoury Crate SE exposes these via
// ASUS_WMI_MGMT_GUID. Access path: WMI → asus-wmi kernel driver (Linux)
// or WbemLocator / IWbemServices (Windows).
//
// Windows production path (stub below):
//   WMI query: SELECT * FROM MSAcpi_ThermalZoneTemperature
//   OR:        NtPowerInformation(ProcessorInformation) for CPU package watts
//   OR:        AMD µProf / RyzenMaster SDK for per-domain power
//
// WHAT THIS FILE DOES:
//   - Polls telemetry at configurable interval (default: every 16 frames ~60Hz)
//   - Computes dispatch_budget = actual_watts / tdp_ceiling
//   - Scales WattageProfile.lambda, alpha, dt proportionally
//   - Writes SupervisorParams for GPU supervisor shader
//   - Emits PowerEvent on significant budget change (>10% delta)
//
// WHAT THIS FILE DOES NOT DO:
//   - Control the GPU scheduler
//   - Override OS power management
//   - Guarantee thermal safety (driver/firmware owns that)
//
// Math: proportional scaling of dissipation and backreaction
//
//   Given budget b = actual_watts / tdp_ceiling ∈ [0,1]:
//
//   λ_actual = λ_base · (1 − 0.5·(1−b))
//            = λ_base · (0.5 + 0.5·b)
//
//   Rationale: at full budget (b=1), λ_actual = λ_base.
//              at zero budget  (b=0), λ_actual = 0.5·λ_base (halved, not zeroed).
//              We never zero λ — zero dissipation → norm can grow unbounded.
//
//   α_actual = α_base · b
//
//   Rationale: backreaction strength scales directly with available compute.
//              At b=0 (thermal suppressed), backreaction off — norm drifts
//              slowly but kernel doesn't fight it. Cheaper per tick.
//
//   dt is NOT scaled. dt is determined by target frame rate (240/120/60 Hz).
//   Changing dt mid-session causes discontinuities in the integration.
//   Frame rate changes go through WattageProfile hot-swap, not telemetry scaling.
// =============================================================================

use crate::{WattageProfile, DIM};

// ---------------------------------------------------------------------------
// TELEMETRY SAMPLE  (one reading from hardware)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PowerTelemetrySample {
    /// Actual measured APU package power (watts).
    /// Source: ASUS WMI PPT_LIMIT_APU or NtPowerInformation.
    pub actual_watts:       f32,

    /// iGPU die temperature (°C).
    pub gpu_temp_c:         f32,

    /// CPU die temperature (°C). Used for TjMax headroom.
    pub cpu_temp_c:         f32,

    /// TjMax for this APU (°C). Z2 Extreme: 100°C typical.
    pub tj_max_c:           f32,

    /// Battery state: true = on battery (→ conservative budget)
    pub on_battery:         bool,

    /// Sample timestamp (ns, CPU monotonic clock)
    pub timestamp_ns:       u64,
}

impl PowerTelemetrySample {
    /// Thermal headroom = TjMax - max(GPU_temp, CPU_temp)
    /// Used by occupancy_gate_pass in supervisor shader.
    #[inline]
    pub fn thermal_headroom_c(&self) -> f32 {
        let hottest = self.gpu_temp_c.max(self.cpu_temp_c);
        (self.tj_max_c - hottest).max(0.0)
    }

    /// Dispatch budget [0.0, 1.0] = actual / ceiling, clamped.
    #[inline]
    pub fn dispatch_budget(&self, tdp_ceiling: f32) -> f32 {
        if tdp_ceiling <= 0.0 { return 0.0; }
        (self.actual_watts / tdp_ceiling).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// TELEMETRY READER  (platform stub — replace internals with real WMI calls)
// ---------------------------------------------------------------------------

pub struct AllyXPowerReader {
    /// TjMax for this device. Read once at init from CPUID / WMI.
    pub tj_max_c: f32,
    /// Poll interval in frames. Default: 16 (~60Hz poll at 240Hz render).
    pub poll_interval_frames: u32,
    /// Frame counter for poll gating.
    frame_counter: u32,
    /// Last valid sample (returned on non-poll frames).
    last_sample: PowerTelemetrySample,
}

impl AllyXPowerReader {
    pub fn new(tj_max_c: f32) -> Self {
        Self {
            tj_max_c,
            poll_interval_frames: 16,
            frame_counter: 0,
            last_sample: PowerTelemetrySample {
                actual_watts: 25.0,    // safe default: balanced preset
                gpu_temp_c:   60.0,
                cpu_temp_c:   60.0,
                tj_max_c,
                on_battery:   false,
                timestamp_ns: 0,
            },
        }
    }

    /// Z2 Extreme default: TjMax = 100°C
    pub fn new_ally_x_z2() -> Self { Self::new(100.0) }

    /// Call once per frame. Returns cached sample on non-poll frames.
    /// Replace `read_hardware_stub()` with real WMI/ACPI calls.
    pub fn sample(&mut self, timestamp_ns: u64) -> PowerTelemetrySample {
        self.frame_counter += 1;
        if self.frame_counter < self.poll_interval_frames {
            return self.last_sample;
        }
        self.frame_counter = 0;
        self.last_sample = self.read_hardware_stub(timestamp_ns);
        self.last_sample
    }

    /// STUB — replace with:
    /// Windows: WbemLocator query or NtPowerInformation(ProcessorInformation)
    /// Linux:   read /sys/class/hwmon/hwmon*/power1_input (µW → W)
    ///          or asus-wmi sysfs: /sys/devices/platform/asus-nb-wmi/
    fn read_hardware_stub(&self, ts: u64) -> PowerTelemetrySample {
        PowerTelemetrySample {
            actual_watts: 25.0,   // TODO: real WMI read
            gpu_temp_c:   65.0,   // TODO: real thermal read
            cpu_temp_c:   68.0,
            tj_max_c:     self.tj_max_c,
            on_battery:   false,
            timestamp_ns: ts,
        }
    }
}

// ---------------------------------------------------------------------------
// PROFILE PATCHER  (core of this file)
// Applies telemetry to WattageProfile in-place.
// Called CPU-side each frame before DVSMSupervisor::tick().
// ---------------------------------------------------------------------------

pub struct ProfilePatcher {
    /// Base profile (the "ceiling" values — never exceeded).
    base: WattageProfile,
    /// Threshold for emitting a PowerEvent (fractional budget change).
    event_threshold: f32,
    /// Previous budget for change detection.
    prev_budget: f32,
}

impl ProfilePatcher {
    pub fn new(base: WattageProfile) -> Self {
        Self { base, event_threshold: 0.10, prev_budget: 1.0 }
    }

    /// Patch `profile` in-place from telemetry sample.
    /// Returns Some(PowerEvent) if budget changed significantly.
    pub fn patch(
        &mut self,
        profile: &mut WattageProfile,
        sample: &PowerTelemetrySample,
    ) -> Option<PowerEvent> {
        let b = sample.dispatch_budget(self.base.tdp_watts);

        // Scale dissipation: λ_actual = λ_base · (0.5 + 0.5·b)
        // At b=1.0: λ_actual = λ_base  (full budget, full dissipation)
        // At b=0.0: λ_actual = 0.5·λ_base  (throttled, halved — never zeroed)
        profile.lambda = self.base.lambda * (0.5 + 0.5 * b);

        // Scale backreaction: α_actual = α_base · b
        // At b=0: backreaction off. Norm drifts slowly — acceptable under throttle.
        profile.alpha = self.base.alpha * b;

        // dt: NOT patched. See file header comment.
        // ema_beta: NOT patched. Memory lag is independent of power budget.
        // frame_gen: NOT patched here. Power event path handles mode change.

        // Emit event on significant budget change
        let delta = (b - self.prev_budget).abs();
        let event = if delta > self.event_threshold {
            let evt = PowerEvent {
                prev_budget: self.prev_budget,
                new_budget:  b,
                thermal_headroom_c: sample.thermal_headroom_c(),
                on_battery: sample.on_battery,
            };
            self.prev_budget = b;
            Some(evt)
        } else {
            None
        };

        event
    }
}

// ---------------------------------------------------------------------------
// SUPERVISOR PARAMS BUILDER
// Converts live telemetry + profile into SupervisorParams for the GPU shader.
// This struct matches the SupervisorParams uniform in dvsm_gpu_supervisor.wgsl.
// Layout must stay in sync with the WGSL struct (std140 rules apply).
// ---------------------------------------------------------------------------

/// Must match SupervisorParams in dvsm_gpu_supervisor.wgsl exactly.
/// std140 layout: each f32 is 4 bytes, u32 is 4 bytes, no implicit padding
/// between same-type fields. Explicit _pad fields where needed.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SupervisorParamsGpu {
    pub actual_watts:       f32,
    pub tdp_ceiling:        f32,
    pub thermal_headroom_c: f32,
    pub dispatch_budget:    f32,
    pub vrs_enabled:        u32,
    pub vrs_tile_count:     u32,
    pub norm_variance:      f32,
    pub ghost_threshold:    f32,
    pub frame_index:        u32,
    pub _pad:               u32,
}

impl SupervisorParamsGpu {
    pub fn build(
        sample: &PowerTelemetrySample,
        profile: &WattageProfile,
        norm_variance: f32,
        ghost_threshold: f32,
        vrs_tile_count: u32,
        frame_index: u32,
    ) -> Self {
        let budget = sample.dispatch_budget(profile.tdp_watts);
        Self {
            actual_watts:       sample.actual_watts,
            tdp_ceiling:        profile.tdp_watts,
            thermal_headroom_c: sample.thermal_headroom_c(),
            dispatch_budget:    budget,
            vrs_enabled:        profile.vrs_enabled as u32,
            vrs_tile_count,
            norm_variance,
            ghost_threshold,
            frame_index,
            _pad: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// POWER EVENT  (emitted by ProfilePatcher on significant budget change)
// Consumed by DVSMSupervisor to decide frame_gen mode changes.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct PowerEvent {
    pub prev_budget:        f32,
    pub new_budget:         f32,
    pub thermal_headroom_c: f32,
    pub on_battery:         bool,
}

impl PowerEvent {
    /// Should we downgrade frame gen mode?
    /// Rule: if budget drops below 0.6 or thermal headroom < 10°C, drop to Off.
    pub fn should_disable_frame_gen(&self) -> bool {
        self.new_budget < 0.6 || self.thermal_headroom_c < 10.0
    }

    /// Should we upgrade frame gen mode?
    /// Rule: budget recovered above 0.8 AND headroom comfortable AND not on battery.
    pub fn should_enable_frame_gen(&self) -> bool {
        self.new_budget > 0.8
            && self.thermal_headroom_c > 20.0
            && !self.on_battery
    }
}

// ---------------------------------------------------------------------------
// FULL FRAME TICK INTEGRATION EXAMPLE
// Shows how all pieces wire together. This is NOT a separate struct —
// it is documentation for how the caller (DVSMSupervisor or engine plugin)
// should call these in sequence.
//
// Per-frame sequence:
//
//   1. reader.sample(timestamp_ns)           → PowerTelemetrySample
//   2. patcher.patch(&mut profile, &sample)  → Option<PowerEvent>
//   3. if PowerEvent::should_disable_frame_gen() → profile.frame_gen = Off
//   4. SupervisorParamsGpu::build(...)       → GPU uniform buffer update
//   5. GPU: submit supervisor shader passes (norm_reduction, vrs_hint,
//           ghost_scan, occupancy_gate)
//   6. CPU: read gate_buf[0] — if 0, skip math kernel this frame
//   7. CPU: read ghost_flags[0] — if nonzero, run GhostGuard::scan_and_rebirth()
//   8. CPU: read norm_buf[0] — update DVSMState.norm_sq
//   9. GPU: submit math kernel passes (lie_bracket, backreaction, ema)
//  10. DVSMSupervisor::tick() bookkeeping (replay hash, frame record)
//
// The gate check (step 6) is the power-rail telemetry → kernel connection.
// Without it, the math kernel dispatches at full rate regardless of thermal state.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// WMI QUERY SKELETON  (Windows production replacement for stub)
// Uncomment and fill in when winreg / wmi crate is available.
// ---------------------------------------------------------------------------

/*
use wmi::{COMLibrary, WMIConnection};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct ThermalZone {
    #[serde(rename = "CurrentTemperature")]
    current_temperature: u32,  // in tenths of Kelvin
}

pub fn read_thermal_wmi() -> Option<f32> {
    let com  = COMLibrary::new().ok()?;
    let conn = WMIConnection::new(com.into()).ok()?;
    let zones: Vec<ThermalZone> = conn
        .raw_query("SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature")
        .ok()?;
    zones.first().map(|z| (z.current_temperature as f32 / 10.0) - 273.15)
}
*/
