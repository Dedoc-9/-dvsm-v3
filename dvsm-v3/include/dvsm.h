/*
 * dvsm.h  —  DVSM-v3 C ABI  (stable binary contract)
 * =====================================================
 * Compatible with: Unreal Engine native plugin, Unity DllImport,
 * custom DX12/Vulkan engines, Windows/Linux/Steam Deck.
 *
 * ABI version: 3.0.0
 * Breaking changes: bump DVSM_ABI_VERSION and add new typedef.
 *
 * LAYOUT RULES:
 *   All structs are packed to natural alignment (no #pragma pack needed
 *   because all fields are f32/u64/u8 with explicit pad).
 *   Pointers are not in any ABI struct — no fat pointers.
 */

#pragma once
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DVSM_ABI_VERSION  3
#define DVSM_DIM          16

/* -----------------------------------------------------------------------
 * Core state (maps 1:1 to DVSMState in Rust)
 * ----------------------------------------------------------------------- */
typedef struct DVSMState {
    float    z[16];
    float    s[16];
    float    kappa[256];   /* 16×16 coupling matrix */
    float    norm_sq;
    uint64_t replay_hash;
    float    _pad[1];      /* align to 8 bytes */
} DVSMState;

/* -----------------------------------------------------------------------
 * Wattage profile (hardware tuning surface)
 * ----------------------------------------------------------------------- */
typedef enum DVSMFrameGenMode : uint8_t {
    DVSM_FRAMEGEN_OFF         = 0,
    DVSM_FRAMEGEN_INTERPOLATE = 1,
    DVSM_FRAMEGEN_EXTRAPOLATE = 2,
} DVSMFrameGenMode;

typedef struct DVSMWattageProfile {
    float             tdp_watts;
    float             dt;
    float             lambda;
    float             alpha;
    float             e_target;
    float             ema_beta;
    DVSMFrameGenMode  frame_gen;
    uint8_t           vrs_enabled;
    uint8_t           _pad[2];
} DVSMWattageProfile;

/* -----------------------------------------------------------------------
 * Frame replay record — immutable after write
 * ----------------------------------------------------------------------- */
typedef struct DVSMFrameReplay {
    uint64_t   frame_index;
    uint64_t   dispatch_ns;
    uint64_t   complete_ns;
    uint32_t   step_count;
    DVSMState  state_snap;
    float      frame_gen_err;
    float      wattage_tdp;
    uint64_t   hash_chain;
} DVSMFrameReplay;

/* -----------------------------------------------------------------------
 * C API surface
 * ----------------------------------------------------------------------- */

/** Initialize supervisor with given profile. Returns handle (opaque). */
void* dvsm_create(DVSMWattageProfile profile);

/** Destroy supervisor. */
void  dvsm_destroy(void* handle);

/** Execute one full supervisor tick. Fills record. */
void  dvsm_tick(void* handle, uint64_t dispatch_ns, uint64_t complete_ns,
                DVSMFrameReplay* out_record);

/** Read current VRS rate hint [0.5, 1.0]. */
float dvsm_vrs_rate(void* handle);

/** Change wattage profile (hot-swap, e.g. on power event). */
void  dvsm_set_profile(void* handle, DVSMWattageProfile profile);

/** Verify hash chain integrity of a replay sequence.
 *  Returns number of broken links (0 = clean). */
uint32_t dvsm_verify_replay(const DVSMFrameReplay* frames, uint32_t count);

#ifdef __cplusplus
}
#endif
