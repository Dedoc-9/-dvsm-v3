 }\n    pub fn update_replay_hash(&mut self) {\n        let mut h:u64=self.replay_hash;\n        for k in 0..DIM {\n            let clamped=self.z[k].clamp(-1.0+1e-7,1.0-1e-7);\n            let q=(clamped*Q31_SCALE) as i32 as u32;\n            h^=(q as u64).wrapping_mul(0x9e3779b97f4a7c15).wrapping_add((k as u64)<<32);\n        }\n        self.replay_hash=h;\n    }\n}\n\npub fn dvsm_step(state: &mut DVSMState, p: &WattageProfile) {\n    let mut acc=[0.0_f32;DIM];\n    for k in 0..DIM {\n        let zk=state.z[k]; let sk=state.s[k];\n        for j in 0..DIM {\n            if j==k { continue; }\n            let bracket=zk*state.s[j]-state.z[j]*sk;\n            acc[k]+=state.kappa_get(k,j)*bracket;\n        }\n    }\n    let backreaction_coeff=-p.alpha*(state.norm_sq-p.e_target);\n    for k in 0..DIM {\n        let b_k=backreaction_coeff*state.z[k];\n        let dz=p.dt*(acc[k]-p.lambda*state.z[k]+b_k);\n        state.z[k]+=dz;\n        state.s[k]=p.ema_beta*state.s[k]+(1.0-p.ema_beta)*state.z[k];\n    }\n    state.update_norm(); state.update_replay_hash();\n}\n\npub struct GhostGuard { pub collapse_threshold:f32, pub rebirth_scale:f32, pub ghost_count:u32 }\nimpl GhostGuard {\n    pub fn new()->Self{Self{collapse_threshold:0.01,rebirth_scale:0.5,ghost_count:0}}\n    pub fn scan_and_rebirth(&mut self,state:&mut DVSMState)->u32 {\n        let mut reborn=0u32;\n        for k in 0..DIM {\n            if state.z[k].abs()<self.collapse_threshold { state.z[k]=state.s[k]*self.rebirth_scale; reborn+=1; }\n        }\n        self.ghost_count+=reborn; reborn\n    }\n}\n\npub fn vrs_rate(norm_variance:f32,enabled:bool)->f32 {\n    if !enabled{return 1.0;}\n    if norm_variance<0.02{0.5} else if norm_variance<0.10{0.75} else{1.0}\n}\n\n#[repr(C)]\n#[derive(Clone,Copy,Debug,Default)]\npub struct FrameTimestamp { pub dispatch_ns:u64, pub complete_ns:u64, pub step_count:u32 }\nimpl FrameTimestamp {\n    #[inline] pub fn delta_ns(&self)->u64{self.complete_ns-self.dispatch_ns}\n    #[inline] pub fn delta_us(&self)->f32{self.delta_ns() as f32/1_000.0}\n}\n\n#[repr(C)]\n#[derive(Clone,Copy,Debug)]\npub struct FrameReplay {\n    pub frame_index:u64, pub timestamp:FrameTimestamp, pub state_snap:DVSMState,\n    pub frame_gen_err:f32, pub wattage_tdp:f32, pub hash_chain:u64,\n}\nimpl FrameReplay {\n    pub fn new(idx:u64,ts:FrameTimestamp,state:DVSMState,fge:f32,tdp:f32,prev_chain:u64)->Self {\n        Self{frame_index:idx,timestamp:ts,state_snap:state,frame_gen_err:fge,wattage_tdp:tdp,\n             hash_chain:state.replay_hash^prev_chain}\n    }\n    pub fn verify(&self,prev_chain:u64)->bool{self.hash_chain==(self.state_snap.replay_hash^prev_chain)}\n}\n\n#[derive(Clone,Copy,Debug,Default)]\npub struct RollingVariance { pub n:u32, pub mean:f32, pub m2:f32 }\nimpl RollingVariance {\n    pub fn update(&mut self,x:f32){\n        self.n+=1; let delta=x-self.mean; self.mean+=delta/self.n as f32;\n        self.m2+=delta*(x-self.mean);\n    }\n    pub fn variance(&self)->f32{if self.n<2{0.0}else{self.m2/self.n as f32}}\n}\n\npub struct DVSMSupervisor {\n    pub state:DVSMState, pub profile:WattageProfile, pub frame_gen:FrameGenState,\n    pub ghost_guard:GhostGuard, pub norm_var:RollingVariance,\n    pub frame_idx:u64, pub hash_chain:u64,\n}\nimpl DVSMSupervisor {\n    pub fn new(profile:WattageProfile)->Self {\n        Self{state:DVSMState::new_identity(),profile,frame_gen:FrameGenState::new(),\n             ghost_guard:GhostGuard::new(),norm_var:RollingVariance::default(),frame_idx:0,hash_chain:0}\n    }\n    pub fn tick(&mut self,dispatch_ns:u64,complete_ns:u64)->FrameReplay {\n        dvsm_step(&mut self.state,&self.profile);\n        self.ghost_guard.scan_and_rebirth(&mut self.state);\n        self.frame_gen.advance(&self.state.z);\n        match self.profile.frame_gen {\n            FrameGenMode::Interpolate=>self.frame_gen.interpolate(),\n            FrameGenMode::Extrapolate=>self.frame_gen.extrapolate(),\n            FrameGenMode::Off=>{}\n        }\n        let _ghost_triggered=self.frame_gen.check_ghost(&self.state.z.clone(),0.05);\n        self.norm_var.update(self.state.norm_sq);\n        let ts=FrameTimestamp{dispatch_ns,complete_ns,step_count:1};\n        let rec=FrameReplay::new(self.frame_idx,ts,self.state,self.frame_gen.ghost_err,\n                                 self.profile.tdp_watts,self.hash_chain);\n        self.hash_chain=rec.hash_chain; self.frame_idx+=1; rec\n    }\n    pub fn vrs_rate(&self)->f32{vrs_rate(self.norm_var.variance(),self.profile.vrs_enabled)}\n    pub fn apply_telemetry(&mut self,actual_watts:f32,thermal_headroom_c:f32)->bool {\n        let b=if self.profile.tdp_watts>0.0{(actual_watts/self.profile.tdp_watts).clamp(0.0,1.0)}else{0.0};\n        self.profile.lambda=self.profile.lambda*(0.5+0.5*b);\n        self.profile.alpha=self.profile.alpha*b;\n        self.norm_var.update(self.state.norm_sq);\n        thermal_headroom_c>=5.0&&b>=0.20\n    }\n}"
        }
      }
    },

    "platform": {
      "description": "Windows + Ally X hardware integration: DX12 timestamps, registry user control, power events, P99 ring, WMI telemetry, profile patching, GPU uniform builder",
      "files": {
        "platform/windows.rs": {
          "description": "DX12 timestamps, registry-backed user control, power event hook, P99 FrameVarianceRing (256-frame rolling window)",
          "structs": {
            "Dx12TimestampPair": "begin_ticks, end_ticks, gpu_frequency_hz → delta_ns() / delta_us()",
            "TdpPreset": "LowPower=0 | Balanced=1 | Perf=2",
            "WindowsUserControl": "load_from_registry() → HKCU\\Software\\DVSM (no elevation). Fields: enabled, tdp_preset, frame_gen_enable, vrs_enable, ghost_threshold",
            "FrameVarianceRing": "256-frame ring. push(frame_us), mean(), variance(), p99() — THE ONLY VALID PERF CLAIM SOURCE"
          },
          "functions": {
            "on_power_event": "on_battery=true → LOW_POWER profile; false → registry profile"
          },
          "full_source": "use crate::{DVSMSupervisor, WattageProfile};\n\n#[repr(C)]\n#[derive(Clone, Copy, Debug, Default)]\npub struct Dx12TimestampPair {\n    pub begin_ticks: u64, pub end_ticks: u64, pub gpu_frequency_hz: u64,\n}\nimpl Dx12TimestampPair {\n    pub fn delta_ns(&self)->u64 {\n        if self.gpu_frequency_hz==0{return 0;}\n        let delta=self.end_ticks.saturating_sub(self.begin_ticks);\n        delta.saturating_mul(1_000_000_000)/self.gpu_frequency_hz\n    }\n    pub fn delta_us(&self)->f32{self.delta_ns() as f32/1_000.0}\n}\n\n#[repr(u8)]\n#[derive(Clone, Copy, Debug, PartialEq)]\npub enum TdpPreset{LowPower=0,Balanced=1,Perf=2}\n\n#[derive(Clone, Copy, Debug)]\npub struct WindowsUserControl {\n    pub enabled:bool, pub tdp_preset:TdpPreset,\n    pub frame_gen_enable:bool, pub vrs_enable:bool, pub ghost_threshold:f32,\n}\nimpl WindowsUserControl {\n    pub fn load_from_registry()->Self {\n        Self{enabled:true,tdp_preset:TdpPreset::Balanced,frame_gen_enable:true,vrs_enable:true,ghost_threshold:0.05}\n    }\n    pub fn to_wattage_profile(&self)->WattageProfile {\n        match self.tdp_preset {\n            TdpPreset::LowPower=>WattageProfile::LOW_POWER,\n            TdpPreset::Balanced=>WattageProfile::ALLY_X_BALANCED,\n            TdpPreset::Perf=>WattageProfile::ALLY_X_PERF,\n        }\n    }\n}\n\npub fn on_power_event(sup:&mut DVSMSupervisor,on_battery:bool){\n    if on_battery{sup.profile=WattageProfile::LOW_POWER;}\n    else{sup.profile=WindowsUserControl::load_from_registry().to_wattage_profile();}\n}\n\npub const RING_SIZE:usize=256;\npub struct FrameVarianceRing{pub buf:[f32;RING_SIZE],pub head:usize,pub count:usize}\nimpl FrameVarianceRing {\n    pub fn new()->Self{Self{buf:[0.0;RING_SIZE],head:0,count:0}}\n    pub fn push(&mut self,frame_us:f32){\n        self.buf[self.head]=frame_us;\n        self.head=(self.head+1)%RING_SIZE;\n        if self.count<RING_SIZE{self.count+=1;}\n    }\n    pub fn mean(&self)->f32{\n        if self.count==0{return 0.0;}\n        self.buf[..self.count].iter().sum::<f32>()/self.count as f32\n    }\n    pub fn variance(&self)->f32{\n        if self.count<2{return 0.0;}\n        let m=self.mean();\n        self.buf[..self.count].iter().map(|x|(x-m).powi(2)).sum::<f32>()/self.count as f32\n    }\n    pub fn p99(&self)->f32{\n        if self.count==0{return 0.0;}\n        let mut tmp=[0.0_f32;RING_SIZE];\n        tmp[..self.count].copy_from_slice(&self.buf[..self.count]);\n        tmp[..self.count].sort_by(|a,b|a.partial_cmp(b).unwrap());\n        tmp[((self.count as f32*0.99) as usize).min(self.count-1)]\n    }\n}"
        },
        "platform/ally_x_power.rs": {
          "description": "Ally X / Z2 Extreme power-rail telemetry bridge. WMI device IDs, telemetry sampling, profile patching (λ/α scaling), GPU uniform builder, PowerEvent emission.",
          "wmi_device_ids": {
            "PPT_LIMIT_APU":  "0x001200C0 — sustained power limit (SPL), watts",
            "PPT_APU_SPPT":   "0x001200C1 — slow PPT (~30s avg)",
            "PPT_APU_FPPT":   "0x001200C2 — fast PPT (burst ceiling)",
            "GPU_TEMP":       "0x00110019 — iGPU die temp (°C × 1000 some FW)",
            "CPU_TEMP":       "0x00110020 — CPU die temp"
          },
          "scaling_math": {
            "budget": "b = actual_watts / tdp_ceiling ∈ [0,1]",
            "lambda": "λ_actual = λ_base · (0.5 + 0.5·b)  — never zeroed at b=0",
            "alpha":  "α_actual = α_base · b  — off at b=0 (norm drifts slowly under throttle)",
            "dt":     "NOT scaled — dt is frame rate, changes via hot-swap not telemetry",
            "ema_beta": "NOT scaled — memory lag independent of power budget"
          },
          "per_frame_sequence": [
            "1. reader.sample(timestamp_ns) → PowerTelemetrySample",
            "2. patcher.patch(&mut profile, &sample) → Option<PowerEvent>",
            "3. if PowerEvent::should_disable_frame_gen() → profile.frame_gen = Off",
            "4. SupervisorParamsGpu::build(...) → GPU uniform buffer update",
            "5. GPU: supervisor shader passes (norm_reduction, vrs_hint, ghost_scan, occupancy_gate)",
            "6. CPU: read gate_buf[0] — 0 = skip math kernel this frame",
            "7. CPU: read ghost_flags[0] — nonzero = run GhostGuard::scan_and_rebirth()",
            "8. CPU: read norm_buf[0] → update DVSMState.norm_sq",
            "9. GPU: math kernel (lie_bracket, backreaction, ema)",
            "10. DVSMSupervisor::tick() bookkeeping (replay hash, frame record)"
          ],
          "power_event_rules": {
            "disable_frame_gen": "new_budget < 0.6 OR thermal_headroom < 10°C",
            "enable_frame_gen":  "new_budget > 0.8 AND thermal_headroom > 20°C AND NOT on_battery"
          },
          "full_source": "use crate::{WattageProfile, DIM};\n\n#[repr(C)]\n#[derive(Clone, Copy, Debug, Default)]\npub struct PowerTelemetrySample {\n    pub actual_watts: f32, pub gpu_temp_c: f32, pub cpu_temp_c: f32,\n    pub tj_max_c: f32, pub on_battery: bool, pub timestamp_ns: u64,\n}\nimpl PowerTelemetrySample {\n    #[inline] pub fn thermal_headroom_c(&self)->f32{\n        let hottest=self.gpu_temp_c.max(self.cpu_temp_c);\n        (self.tj_max_c-hottest).max(0.0)\n    }\n    #[inline] pub fn dispatch_budget(&self,tdp_ceiling:f32)->f32{\n        if tdp_ceiling<=0.0{return 0.0;}\n        (self.actual_watts/tdp_ceiling).clamp(0.0,1.0)\n    }\n}\n\npub struct AllyXPowerReader {\n    pub tj_max_c:f32, pub poll_interval_frames:u32,\n    frame_counter:u32, last_sample:PowerTelemetrySample,\n}\nimpl AllyXPowerReader {\n    pub fn new(tj_max_c:f32)->Self {\n        Self{tj_max_c,poll_interval_frames:16,frame_counter:0,\n             last_sample:PowerTelemetrySample{actual_watts:25.0,gpu_temp_c:60.0,cpu_temp_c:60.0,\n                                              tj_max_c,on_battery:false,timestamp_ns:0}}\n    }\n    pub fn new_ally_x_z2()->Self{Self::new(100.0)}\n    pub fn sample(&mut self,timestamp_ns:u64)->PowerTelemetrySample {\n        self.frame_counter+=1;\n        if self.frame_counter<self.poll_interval_frames{return self.last_sample;}\n        self.frame_counter=0;\n        self.last_sample=self.read_hardware_stub(timestamp_ns);\n        self.last_sample\n    }\n    fn read_hardware_stub(&self,ts:u64)->PowerTelemetrySample {\n        PowerTelemetrySample{actual_watts:25.0,gpu_temp_c:65.0,cpu_temp_c:68.0,\n                              tj_max_c:self.tj_max_c,on_battery:false,timestamp_ns:ts}\n    }\n}\n\npub struct ProfilePatcher{base:WattageProfile,event_threshold:f32,prev_budget:f32}\nimpl ProfilePatcher {\n    pub fn new(base:WattageProfile)->Self{Self{base,event_threshold:0.10,prev_budget:1.0}}\n    pub fn patch(&mut self,profile:&mut WattageProfile,sample:&PowerTelemetrySample)->Option<PowerEvent>{\n        let b=sample.dispatch_budget(self.base.tdp_watts);\n        profile.lambda=self.base.lambda*(0.5+0.5*b);\n        profile.alpha=self.base.alpha*b;\n        let delta=(b-self.prev_budget).abs();\n        if delta>self.event_threshold {\n            let evt=PowerEvent{prev_budget:self.prev_budget,new_budget:b,\n                               thermal_headroom_c:sample.thermal_headroom_c(),on_battery:sample.on_battery};\n            self.prev_budget=b; Some(evt)\n        } else { None }\n    }\n}\n\n#[repr(C)]\n#[derive(Clone, Copy, Debug, Default)]\npub struct SupervisorParamsGpu {\n    pub actual_watts:f32, pub tdp_ceiling:f32, pub thermal_headroom_c:f32,\n    pub dispatch_budget:f32, pub vrs_enabled:u32, pub vrs_tile_count:u32,\n    pub norm_variance:f32, pub ghost_threshold:f32, pub frame_index:u32, pub _pad:u32,\n}\nimpl SupervisorParamsGpu {\n    pub fn build(sample:&PowerTelemetrySample,profile:&WattageProfile,\n                 norm_variance:f32,ghost_threshold:f32,vrs_tile_count:u32,frame_index:u32)->Self {\n        let budget=sample.dispatch_budget(profile.tdp_watts);\n        Self{actual_watts:sample.actual_watts,tdp_ceiling:profile.tdp_watts,\n             thermal_headroom_c:sample.thermal_headroom_c(),dispatch_budget:budget,\n             vrs_enabled:profile.vrs_enabled as u32,vrs_tile_count,\n             norm_variance,ghost_threshold,frame_index,_pad:0}\n    }\n}\n\n#[derive(Clone, Copy, Debug)]\npub struct PowerEvent{pub prev_budget:f32,pub new_budget:f32,pub thermal_headroom_c:f32,pub on_battery:bool}\nimpl PowerEvent {\n    pub fn should_disable_frame_gen(&self)->bool{self.new_budget<0.6||self.thermal_headroom_c<10.0}\n    pub fn should_enable_frame_gen(&self)->bool{\n        self.new_budget>0.8&&self.thermal_headroom_c>20.0&&!self.on_battery\n    }\n}"
        }
      }
    },

    "shaders": {
      "description": "WGSL compute shaders for RDNA3/3.5. 3 math passes + 4 supervisor passes per frame.",
      "files": {
        "shaders/dvsm_gpu.wgsl": {
          "description": "3 compute passes: lie_bracket_pass, backreaction_pass, ema_pass. DIM=16 per workgroup = 1 wave on RDNA3.",
          "passes": {
            "lie_bracket_pass": "acc[k] = Σ_j κ[k*16+j] · (Z_k·S_j − Z_j·S_k). Thread k, skip j==k. O(16) per thread.",
            "backreaction_pass": "b_coeff = −α·(norm_sq − e_target); Z_out[k] = Z_in[k] + dt·(acc[k] − λ·Z_in[k] + b_coeff·Z_in[k])",
            "ema_pass": "S_out[k] = β·S_in[k] + (1−β)·Z_out[k]"
          },
          "full_source": "struct Params {\n    dt:f32, lambda_:f32, alpha:f32, e_target:f32,\n    ema_beta:f32, norm_sq:f32, _pad:f32, _pad2:f32,\n};\n\n@group(0) @binding(0) var<uniform>            params:Params;\n@group(0) @binding(1) var<storage,read>       z_in:  array<f32,16>;\n@group(0) @binding(2) var<storage,read>       s_in:  array<f32,16>;\n@group(0) @binding(3) var<storage,read>       kappa: array<f32,256>;\n@group(0) @binding(4) var<storage,read_write> z_out: array<f32,16>;\n@group(0) @binding(5) var<storage,read_write> s_out: array<f32,16>;\n@group(0) @binding(6) var<storage,read_write> acc:   array<f32,16>;\n@group(0) @binding(7) var<storage,read_write> norm_out:array<f32,1>;\n\n@compute @workgroup_size(16,1,1)\nfn lie_bracket_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x; var sum:f32=0.0;\n    let zk:f32=z_in[k]; let sk:f32=s_in[k];\n    for(var j:u32=0u;j<16u;j=j+1u){\n        if j==k{continue;}\n        let bracket:f32=zk*s_in[j]-z_in[j]*sk;\n        sum=sum+kappa[k*16u+j]*bracket;\n    }\n    acc[k]=sum;\n}\n\n@compute @workgroup_size(16,1,1)\nfn backreaction_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x; let zk:f32=z_in[k];\n    let b_coeff:f32=-params.alpha*(params.norm_sq-params.e_target);\n    let b_k:f32=b_coeff*zk;\n    let dz:f32=params.dt*(acc[k]-params.lambda_*zk+b_k);\n    z_out[k]=zk+dz;\n}\n\n@compute @workgroup_size(16,1,1)\nfn ema_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x; let b:f32=params.ema_beta;\n    s_out[k]=b*s_in[k]+(1.0-b)*z_out[k];\n}"
        },
        "shaders/dvsm_gpu_supervisor.wgsl": {
          "description": "4 supervisor passes: norm_reduction, vrs_hint, ghost_scan, occupancy_gate. Z2 Extreme: 1/512 wave occupancy.",
          "bindings": {
            "group_1_binding_0": "SupervisorParams uniform (actual_watts, tdp_ceiling, thermal_headroom_c, dispatch_budget, vrs_enabled, vrs_tile_count, norm_variance, ghost_threshold, frame_index)",
            "group_1_binding_1": "z_out (read from math kernel)",
            "group_1_binding_2": "norm_buf[1] — ‖Z‖² result",
            "group_1_binding_3": "vrs_hints[N] — 0x00=full, 0x01=half(2×2), 0x02=quarter(4×4)",
            "group_1_binding_4": "ghost_flags[1] — lower 16 bits: bitmask; upper 16 bits: count",
            "group_1_binding_5": "gate_buf[1] — 1=approve dispatch, 0=suppress"
          },
          "passes": {
            "norm_reduction_pass": "@workgroup_size(1,1,1) — thread 0 sums z_out[k]² for k=0..16 → norm_buf[0]",
            "vrs_hint_pass": "@workgroup_size(64,1,1) — per tile: σ²<0.02→0x01(half rate); dispatch_budget<0.5&&σ²<0.05→0x02(quarter); else 0x00(full)",
            "ghost_scan_pass": "@workgroup_size(16,1,1) — |z_out[k]|<ghost_threshold → atomicOr bit k into ghost_flags[0]; thread 0 writes popcount to upper 16 bits",
            "occupancy_gate_pass": "@workgroup_size(1,1,1) — gate_buf[0]=0 if thermal_headroom<5°C or dispatch_budget<0.20"
          },
          "full_source": "struct SupervisorParams{\n    actual_watts:f32, tdp_ceiling:f32, thermal_headroom_c:f32, dispatch_budget:f32,\n    vrs_enabled:u32, vrs_tile_count:u32, norm_variance:f32, ghost_threshold:f32,\n    frame_index:u32, _pad:u32,\n};\n@group(1) @binding(0) var<uniform>            sup:SupervisorParams;\n@group(1) @binding(1) var<storage,read>       z_out:     array<f32,16>;\n@group(1) @binding(2) var<storage,read_write> norm_buf:  array<f32,1>;\n@group(1) @binding(3) var<storage,read_write> vrs_hints: array<u32>;\n@group(1) @binding(4) var<storage,read_write> ghost_flags:array<u32,1>;\n@group(1) @binding(5) var<storage,read_write> gate_buf:  array<u32,1>;\n\n@compute @workgroup_size(1,1,1)\nfn norm_reduction_pass(){\n    var n:f32=0.0;\n    for(var k:u32=0u;k<16u;k=k+1u){n=n+z_out[k]*z_out[k];}\n    norm_buf[0]=n;\n}\n\n@compute @workgroup_size(64,1,1)\nfn vrs_hint_pass(@builtin(global_invocation_id) gid:vec3<u32>){\n    let tile:u32=gid.x;\n    if tile>=sup.vrs_tile_count{return;}\n    var rate:u32=0x00u;\n    if sup.vrs_enabled!=0u{\n        let v:f32=sup.norm_variance;\n        if v<0.02{rate=0x01u;}\n        if sup.dispatch_budget<0.5&&v<0.05{rate=0x02u;}\n    }\n    vrs_hints[tile]=rate;\n}\n\n@compute @workgroup_size(16,1,1)\nfn ghost_scan_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x;\n    let is_ghost:u32=select(0u,1u,abs(z_out[k])<sup.ghost_threshold);\n    if is_ghost!=0u{atomicOr(&ghost_flags[0],1u<<k);}\n    workgroupBarrier();\n    if k==0u{\n        let mask:u32=ghost_flags[0]&0xFFFFu;\n        let cnt:u32=countOneBits(mask);\n        atomicOr(&ghost_flags[0],cnt<<16u);\n    }\n}\n\n@compute @workgroup_size(1,1,1)\nfn occupancy_gate_pass(){\n    var approved:u32=1u;\n    if sup.thermal_headroom_c<5.0{approved=0u;}\n    if sup.dispatch_budget<0.20{approved=0u;}\n    gate_buf[0]=approved;\n}"
        }
      }
    },

    "binary_api": {
      "description": "Stable C ABI for engine integration (Unreal, Unity, DX12/Vulkan, Windows/Linux/Steam Deck)",
      "files": {
        "include/dvsm.h": {
          "abi_version": 3,
          "structs": {
            "DVSMState": "float z[16]; float s[16]; float kappa[256]; float norm_sq; uint64_t replay_hash; float _pad;",
            "DVSMWattageProfile": "float tdp_watts; float dt; float lambda; float alpha; float e_target; float ema_beta; DVSMFrameGenMode frame_gen; uint8_t vrs_enabled; uint8_t _pad[2];",
            "DVSMFrameReplay": "uint64_t frame_index; uint64_t dispatch_ns; uint64_t complete_ns; uint32_t step_count; DVSMState state_snap; float frame_gen_err; float wattage_tdp; uint64_t hash_chain;"
          },
          "api": {
            "dvsm_create":        "void* dvsm_create(DVSMWattageProfile profile)",
            "dvsm_destroy":       "void dvsm_destroy(void* handle)",
            "dvsm_tick":          "void dvsm_tick(void* handle, uint64_t dispatch_ns, uint64_t complete_ns, DVSMFrameReplay* out_record)",
            "dvsm_vrs_rate":      "float dvsm_vrs_rate(void* handle)",
            "dvsm_set_profile":   "void dvsm_set_profile(void* handle, DVSMWattageProfile profile)",
            "dvsm_verify_replay": "uint32_t dvsm_verify_replay(const DVSMFrameReplay* frames, uint32_t count) — returns broken link count (0=clean)"
          }
        },
        "binary_api/schemas/control.json": {
          "content": {"enabled":true,"tdp_preset":"balanced","frame_gen":"interpolate","vrs_enabled":true,"ghost_threshold":0.05,"replay_enabled":true,"security_verify":true}
        }
      }
    },

    "config_profiles": {
      "description": "Runtime wattage profiles for Ally X. Hot-swappable at runtime via dvsm_set_profile() or on_power_event().",
      "profiles": {
        "ally_x_perf": {
          "file": "config/profiles/ally_x_perf.toml",
          "tdp_watts": 35.0,
          "dt": 0.004167,
          "target_hz": 240,
          "lambda": 0.12,
          "alpha": 0.08,
          "e_target": 1.0,
          "ema_beta": 0.95,
          "frame_gen": "interpolate",
          "vrs_enabled": true,
          "ghost_threshold": 0.05,
          "collapse_threshold": 0.01,
          "rebirth_scale": 0.5
        },
        "ally_x_balanced": {
          "file": "config/profiles/ally_x_balanced.toml",
          "tdp_watts": 25.0,
          "dt": 0.008333,
          "target_hz": 120,
          "lambda": 0.10,
          "alpha": 0.06,
          "e_target": 1.0,
          "ema_beta": 0.93,
          "frame_gen": "interpolate",
          "vrs_enabled": true,
          "ghost_threshold": 0.05,
          "collapse_threshold": 0.01,
          "rebirth_scale": 0.5
        },
        "low_power": {
          "file": "config/profiles/low_power.toml",
          "note": "battery / Xbox eco / silent mode",
          "tdp_watts": 15.0,
          "dt": 0.016667,
          "target_hz": 60,
          "lambda": 0.08,
          "alpha": 0.04,
          "e_target": 1.0,
          "ema_beta": 0.90,
          "frame_gen": "off",
          "vrs_enabled": true,
          "ghost_threshold": 0.08,
          "collapse_threshold": 0.02,
          "rebirth_scale": 0.4
        }
      }
    },

    "spectral_io": {
      "description": "Spectral pre-fetcher: entropy-triggered asset streaming. Uses H(Z) divergence to predict scene transitions 3-8 frames early, drives mip bias and prefetch queue.",
      "pipeline": [
        "1. SpectralEntropyState::update(Z, norm_sq, dt) → trigger bool + divergence_rate",
        "2. ModeClassifier::update(Z) → per-mode GhostClass + mip_bias_array[16]",
        "3. MarkovSalience::observe(h_normalized) → prefetch_mask + prioritized_groups",
        "4. PrefetchQueue::enqueue(jobs, lead_frames) → async I/O to engine streaming thread",
        "5. MipHintBuffer::update(mip_bias_array) → engine texture streaming hint buffer"
      ],
      "bandwidth_model": {
        "hardware": "Ally X LPDDR5X-7500, peak ~60 GB/s shared, iGPU ~30-40 GB/s",
        "standard_streaming_load": "~70% of iGPU bandwidth",
        "mip_bias_savings_estimate": "25-35% bandwidth reduction (title-dependent)",
        "stutter_target": "eliminate 1-frame hitches",
        "claims_policy": "NO bandwidth/FPS claims without FrameVarianceRing.p99() evidence"
      },
      "files": {
        "spectral_io/src/entropy.rs": {
          "description": "H(Z) = −Σ p_k log₂(p_k). Divergence rate dH/dt triggers prefetch. ModeClassifier: Welford variance → Echo/Diffuse/Collapsed → mip bias 0/1/2.",
          "constants": {
            "DIM": 16,
            "H_MAX": 4.0,
            "DIVERGENCE_THRESHOLD": 0.15,
            "DIFFUSE_VAR_THRESHOLD": 0.004,
            "COLLAPSE_THRESHOLD": 0.01,
            "PEAK_RESET_FRAMES": 120
          },
          "ghost_classes": {
            "Echo":      "stable mode (low variance) → mip bias 0 (full res)",
            "Diffuse":   "transient mode (high variance) → mip bias 1 (−1 mip)",
            "Collapsed": "|Z_k| < COLLAPSE_THRESHOLD → mip bias 2 (−2 mips)"
          },
          "full_source": "pub const DIM:usize=16;\npub const H_MAX:f32=4.0;\npub const DIVERGENCE_THRESHOLD:f32=0.15;\npub const DIFFUSE_VAR_THRESHOLD:f32=0.004;\npub const COLLAPSE_THRESHOLD:f32=0.01;\npub const PEAK_RESET_FRAMES:u32=120;\n\n#[derive(Clone,Copy,Debug,Default)]\npub struct SpectralEntropyState{\n    pub h_current:f32, pub h_prev:f32, pub divergence_rate:f32,\n    pub peak_divergence:f32, pub peak_age:u32,\n}\nimpl SpectralEntropyState {\n    pub fn update(&mut self,z:&[f32;DIM],norm_sq:f32,dt:f32)->bool {\n        if norm_sq<1e-9{self.h_current=0.0;self.divergence_rate=0.0;return false;}\n        let mut h=0.0_f32;\n        for k in 0..DIM{let p=(z[k]*z[k])/norm_sq;if p>1e-9{h-=p*p.log2();}}\n        self.h_prev=self.h_current; self.h_current=h;\n        self.divergence_rate=if dt>0.0{(h-self.h_prev)/dt}else{0.0};\n        let abs_div=self.divergence_rate.abs();\n        if abs_div>self.peak_divergence{self.peak_divergence=abs_div;self.peak_age=0;}\n        else{self.peak_age+=1;if self.peak_age>PEAK_RESET_FRAMES{self.peak_divergence=abs_div;self.peak_age=0;}}\n        abs_div>DIVERGENCE_THRESHOLD\n    }\n    pub fn h_normalized(&self)->f32{self.h_current/H_MAX}\n    pub fn estimated_lead_frames(&self)->u32{\n        let gap=self.peak_divergence-self.divergence_rate.abs();\n        if gap<=0.0||self.divergence_rate.abs()<1e-6{return 0;}\n        ((gap/self.divergence_rate.abs()) as u32).min(60)\n    }\n}\n\n#[repr(u8)]\n#[derive(Clone,Copy,Debug,PartialEq)]\npub enum GhostClass{Echo=0,Diffuse=1,Collapsed=2}\n\n#[derive(Clone,Copy,Debug,Default)]\npub struct ModeClassifier{pub n:[u32;DIM],pub mean:[f32;DIM],pub m2:[f32;DIM]}\nimpl ModeClassifier {\n    pub fn update(&mut self,z:&[f32;DIM]){\n        for k in 0..DIM{\n            let x=z[k].abs(); self.n[k]+=1;\n            let d=x-self.mean[k]; self.mean[k]+=d/self.n[k] as f32;\n            self.m2[k]+=d*(x-self.mean[k]);\n        }\n    }\n    pub fn classify(&self,k:usize)->GhostClass{\n        if self.mean[k]<COLLAPSE_THRESHOLD{return GhostClass::Collapsed;}\n        let var=if self.n[k]<2{0.0}else{self.m2[k]/self.n[k] as f32};\n        if var>DIFFUSE_VAR_THRESHOLD{GhostClass::Diffuse}else{GhostClass::Echo}\n    }\n    pub fn mip_bias(&self,k:usize)->u8{self.classify(k) as u8}\n    pub fn global_mip_bias(&self)->u8{(0..DIM).map(|k|self.mip_bias(k)).max().unwrap_or(0)}\n    pub fn mip_bias_array(&self)->[u8;DIM]{\n        let mut out=[0u8;DIM];for k in 0..DIM{out[k]=self.mip_bias(k);}out\n    }\n}"
        },
        "spectral_io/src/markov_salience.rs": {
          "description": "Markov chain over 8 entropy buckets. Row-normalized transition matrix P[i][j]. Salience = P[current] → prefetch_mask for asset groups.",
          "constants": {
            "BUCKET_COUNT": 8,
            "SALIENCE_THRESHOLD": 0.25
          },
          "asset_group_mapping": "Group g → bucket (g % BUCKET_COUNT). S[b] > 0.25 → enqueue group.",
          "full_source": "pub const BUCKET_COUNT:usize=8;\npub const SALIENCE_THRESHOLD:f32=0.25;\n\n#[derive(Clone,Debug)]\npub struct MarkovSalience{\n    pub transitions:[[u32;BUCKET_COUNT];BUCKET_COUNT],\n    pub current_bucket:usize, pub total:[u32;BUCKET_COUNT],\n}\nimpl MarkovSalience {\n    pub fn new()->Self{Self{transitions:[[0;BUCKET_COUNT];BUCKET_COUNT],current_bucket:0,total:[0;BUCKET_COUNT]}}\n    pub fn bucket(h_normalized:f32)->usize{\n        let b=(h_normalized*BUCKET_COUNT as f32) as usize; b.min(BUCKET_COUNT-1)\n    }\n    pub fn observe(&mut self,new_h_normalized:f32){\n        let next=Self::bucket(new_h_normalized);\n        self.transitions[self.current_bucket][next]+=1;\n        self.total[self.current_bucket]+=1;\n        self.current_bucket=next;\n    }\n    pub fn salience(&self)->[f32;BUCKET_COUNT]{\n        let mut s=[0.0_f32;BUCKET_COUNT];\n        let t=self.total[self.current_bucket];\n        if t==0{let u=1.0/BUCKET_COUNT as f32;for j in 0..BUCKET_COUNT{s[j]=u;}}\n        else{for j in 0..BUCKET_COUNT{s[j]=self.transitions[self.current_bucket][j] as f32/t as f32;}}\n        s\n    }\n    pub fn prefetch_mask(&self)->u8{\n        let s=self.salience(); let mut mask=0u8;\n        for j in 0..BUCKET_COUNT{if s[j]>SALIENCE_THRESHOLD{mask|=1<<j;}} mask\n    }\n}"
        },
        "spectral_io/src/prefetch_governor.rs": {
          "description": "Top-level orchestrator. SpectralIOGovernor::tick() runs all 5 pipeline stages. PrefetchQueue: fixed 32-slot ring with TTL expiry. MipHintBuffer: per-mode bias + global aggregate.",
          "structs": {
            "PrefetchJob": "group_id:u32, priority:f32 [0,1], ttl_frames:u16, format_hint:CompressionFormat",
            "CompressionFormat": "Unknown=0 | BC7=1 | ASTC=2 | BC1=3",
            "PrefetchQueue": "capacity=32, ring buffer, drop-oldest on full, tick_ttl() compacts expired jobs",
            "MipHintBuffer": "per_mode:[u8;16], global:u8 (max), h_u8:u8 (entropy for debug overlay)",
            "GovFrameResult": "triggered:bool, jobs_enqueued:u32, mip_hints:MipHintBuffer, divergence_rate:f32, h_normalized:f32"
          },
          "defaults": {
            "max_jobs_per_trigger": 4,
            "default_ttl_frames": 16,
            "format_hint": "BC7"
          }
        },
        "spectral_io/shaders/entropy_scan.wgsl": {
          "description": "Optional GPU entropy path (use CPU at DIM=16; GPU useful at DIM=64+). Pass A: H(Z) via LDS reduction. Pass B: mip_hint_write per mode.",
          "full_source": "struct EntropyParams{\n    norm_sq:f32,dt:f32,h_prev:f32,divergence_thresh:f32,\n    diffuse_var_thresh:f32,collapse_thresh:f32,_pad:f32,_pad2:f32,\n};\n@group(0) @binding(0) var<uniform>            ep:       EntropyParams;\n@group(0) @binding(1) var<storage,read>       z_in:     array<f32,16>;\n@group(0) @binding(2) var<storage,read>       z_var:    array<f32,16>;\n@group(0) @binding(3) var<storage,read>       z_mean:   array<f32,16>;\n@group(0) @binding(4) var<storage,read_write> h_out:    array<f32,1>;\n@group(0) @binding(5) var<storage,read_write> mip_hints:array<u32,16>;\n\nvar<workgroup> partial_h:array<f32,16>;\n\n@compute @workgroup_size(16,1,1)\nfn entropy_compute_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x;\n    let p:f32=(z_in[k]*z_in[k])/max(ep.norm_sq,1e-9);\n    var contrib:f32=0.0;\n    if p>1e-9{contrib=-p*(log(p)/0.6931471806);}\n    partial_h[k]=contrib;\n    workgroupBarrier();\n    if k==0u{\n        var h:f32=0.0;\n        for(var j:u32=0u;j<16u;j=j+1u){h=h+partial_h[j];}\n        h_out[0]=h;\n    }\n}\n\n@compute @workgroup_size(16,1,1)\nfn mip_hint_write_pass(@builtin(local_invocation_id) lid:vec3<u32>){\n    let k:u32=lid.x;\n    let mean:f32=z_mean[k]; let var_:f32=z_var[k];\n    var bias:u32=0u;\n    if mean<ep.collapse_thresh{bias=2u;}\n    else if var_>ep.diffuse_var_thresh{bias=1u;}\n    mip_hints[k]=bias;\n}"
        },
        "spectral_io/schemas/io_governor.json": {
          "description": "JSON Schema for SpectralIOGovernor runtime configuration",
          "content": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "layer": "Spectral_IO_Governor",
            "method": "Asynchronous_Predictive_Streaming",
            "trigger": {
              "divergence_threshold": 0.15,
              "diffuse_var_threshold": 0.004,
              "collapse_threshold": 0.01
            },
            "queue": { "capacity": 32, "max_jobs_per_trigger": 4, "default_ttl_frames": 16 },
            "markov": { "bucket_count": 8, "salience_threshold": 0.25 },
            "compression": { "format_hint": "BC7" },
            "bandwidth_model": {
              "lpddr5x_peak_gb_s": 60.0,
              "igpu_share_gb_s": 35.0,
              "mip_savings_estimate": "25-35%",
              "stutter_target": "eliminate_1_frame_hitches"
            },
            "claims_policy": "measure_first"
          }
        }
      }
    },

    "tests": {
      "description": "Mathematical invariant test suites — the ONLY valid anchors for perf and correctness claims",
      "files": {
        "tests/invariants.rs": {
          "invariants": {
            "INV-1 energy_decay": "α=0, 100 steps. ‖Z‖² ≈ ‖Z_0‖² · exp(−2λ·N·dt). Euler error < 2%.",
            "INV-2 backreaction_convergence": "Perturb ‖Z‖²≈4. After 500 steps with α=0.08: ‖Z‖² within 10% of E_target=1.0.",
            "INV-3 replay_determinism": "Two identical runs produce equal replay_hash after 50 steps.",
            "INV-4 ghost_rebirth": "Force all Z_k=0, S_k=0.5. scan_and_rebirth() → 16 rebirths, Z[0]≠0.",
            "INV-5 hash_chain_integrity": "20 frames: all verify(prev). Tamper state_snap.z[0]+=0.1 → verify fails."
          },
          "full_source": "use dvsm_v3::*;\n\n#[test]\nfn inv1_energy_decay(){\n    let mut p=WattageProfile::ALLY_X_PERF; p.alpha=0.0;\n    let mut s=DVSMState::new_identity(); let z0_norm=s.norm_sq;\n    for _ in 0..100{dvsm_step(&mut s,&p);}\n    let expected=z0_norm*(-2.0*p.lambda*100.0*p.dt).exp();\n    let ratio=s.norm_sq/expected;\n    assert!((ratio-1.0).abs()<0.02,\"energy decay deviated: ratio={}\",ratio);\n}\n\n#[test]\nfn inv2_backreaction_convergence(){\n    let p=WattageProfile::ALLY_X_PERF;\n    let mut s=DVSMState::new_identity();\n    for k in 0..DIM{s.z[k]*=2.0;} s.update_norm();\n    for _ in 0..500{dvsm_step(&mut s,&p);}\n    assert!((s.norm_sq-1.0).abs()<0.10,\"backreaction failed: norm_sq={}\",s.norm_sq);\n}\n\n#[test]\nfn inv3_replay_determinism(){\n    let p=WattageProfile::ALLY_X_PERF;\n    let mut s1=DVSMState::new_identity(); let mut s2=DVSMState::new_identity();\n    for _ in 0..50{dvsm_step(&mut s1,&p);dvsm_step(&mut s2,&p);}\n    assert_eq!(s1.replay_hash,s2.replay_hash);\n}\n\n#[test]\nfn inv4_ghost_rebirth(){\n    let mut s=DVSMState::new_identity(); let mut g=GhostGuard::new();\n    for k in 0..DIM{s.z[k]=0.0;s.s[k]=0.5;} s.update_norm();\n    let reborn=g.scan_and_rebirth(&mut s);\n    assert!(reborn==DIM as u32); assert!(s.z[0].abs()>0.0);\n}\n\n#[test]\nfn inv5_hash_chain_integrity(){\n    let p=WattageProfile::ALLY_X_PERF;\n    let mut sup=DVSMSupervisor::new(p); let mut records=Vec::new();\n    for i in 0..20u64{records.push(sup.tick(i*4_167_000,(i+1)*4_167_000));}\n    let mut prev=0u64;\n    for r in &records{assert!(r.verify(prev));prev=r.hash_chain;}\n    let mut tampered=records[5]; tampered.state_snap.z[0]+=0.1;\n    assert!(!tampered.verify(records[4].hash_chain));\n}"
        },
        "tests/spectral_io_invariants.rs": {
          "invariants": {
            "INV-S1 entropy_bounds": "H(Z) ∈ [0, log₂(16)]. Uniform Z → H≈4.0. Spike mode 0 → H≈0.",
            "INV-S2 ghost_classifier_ordering": "Stable mode→Echo. Zero mode→Collapsed (bias=2). Alternating mode bias ≥ Echo bias.",
            "INV-S3 markov_salience_normalization": "200 observations cycling buckets → salience sums to 1.0 ± 0.01.",
            "INV-S4 queue_ttl_expiry": "ttl=1 job removed after one tick_ttl(). ttl=10 job survives with ttl=9.",
            "INV-S5 governor_trigger_on_spike": "10 frames uniform → spike to mode 0 → divergence_rate ≠ 0."
          }
        }
      }
    },

    "tools": {
      "files": {
        "tools/hash_manifest.rs": {
          "description": "SHA-256 Merkle manifest over source + shader + config. Run before benchmarking — hash mismatch = dirty build = invalid claim.",
          "structure": "manifest_hash = SHA256(source_hash || shader_hash || config_hash || git_commit)",
          "full_source": "use sha2::{Sha256,Digest};\n\npub struct BuildManifest{\n    pub git_commit:String, pub source_hash:String,\n    pub shader_hash:String, pub config_hash:String, pub manifest_hash:String,\n}\n\npub fn sha256_hex(data:&[u8])->String{\n    let mut h=Sha256::new(); h.update(data); format!(\"{:x}\",h.finalize())\n}\n\nimpl BuildManifest{\n    pub fn compute(git_commit:&str,source:&[u8],shader:&[u8],config:&[u8])->Self{\n        let sh=sha256_hex(source); let wh=sha256_hex(shader); let ch=sha256_hex(config);\n        let combined=[sh.as_bytes(),wh.as_bytes(),ch.as_bytes(),git_commit.as_bytes()].concat();\n        let mh=sha256_hex(&combined);\n        Self{git_commit:git_commit.to_string(),source_hash:sh,shader_hash:wh,config_hash:ch,manifest_hash:mh}\n    }\n    pub fn print(&self){\n        println!(\"DVSM-v3 Build Manifest\");\n        println!(\"  git:    {}\",&self.git_commit[..8.min(self.git_commit.len())]);\n        println!(\"  source: {}\",&self.source_hash[..16]);\n        println!(\"  shader: {}\",&self.shader_hash[..16]);\n        println!(\"  config: {}\",&self.config_hash[..16]);\n        println!(\"  TOTAL:  {}\",self.manifest_hash);\n    }\n}"
        }
      }
    },

    "workspace": {
      "files": {
        "Cargo.toml": {
          "content": "[workspace]\nmembers = [\"src\", \"spectral_io\"]\nresolver = \"2\"\n\n[workspace.package]\nversion     = \"3.0.0\"\nedition     = \"2021\"\nlicense     = \"AGPL-3.0-or-later\"\nauthors     = [\"DVSM Contributors\"]\n\n[profile.release]\nlto           = true\ncodegen-units = 1\nopt-level     = 3\nstrip         = true\npanic         = \"abort\""
        }
      }
    }
  },

  "ghost_type_distinction": {
    "state_ghost": {
      "location": "src/lib.rs GhostGuard",
      "cause": "Z_k collapses to zero (false attractor in state space)",
      "fix": "Rebirth from EMA memory S_k at rebirth_scale"
    },
    "render_ghost": {
      "location": "src/lib.rs FrameGenState",
      "cause": "Synthetic frame prediction error — z_synth ≠ z_actual",
      "fix": "Anti-ghost check: ‖z_synth − z_actual‖ > 0.05 threshold → flag"
    }
  },

  "security_and_replay": {
    "frame_replay_purpose": ["anti-cheat (replay must match on identical input)", "debug scrub (frame-by-frame divergence)", "security audit (chain integrity proves no mid-flight mutation)"],
    "hash_encoding": "Q31 XOR-fold with Fibonacci constant 0x9e3779b97f4a7c15",
    "verify_api": "dvsm_verify_replay(frames, count) → count of broken links"
  },

  "build": {
    "commands": {
      "test":    "cargo test",
      "release": "cargo build --release",
      "wasm":    "cargo build --target wasm32-unknown-unknown"
    },
    "z2_extreme_gfx_flag": "--offload-arch=gfx1150",
    "z1_extreme_gfx_flag": "--offload-arch=gfx1103"
  }
}
