#pragma once
#include <stdint.h>
#include <stdbool.h>
#ifdef __cplusplus
extern "C" {
#endif

#define DVSM_ABI_VERSION 3
#define DVSM_DIM         16

typedef struct { float z[16]; float s[16]; float kappa[256];
                 float norm_sq; uint64_t replay_hash; float _pad; } DVSMState;
typedef enum { DVSM_FG_OFF=0, DVSM_FG_INTERP=1, DVSM_FG_EXTRAP=2 } DVSMFrameGen;
typedef struct { float tdp_watts; float dt; float lambda; float alpha;
                 float e_target; float ema_beta; DVSMFrameGen frame_gen;
                 uint8_t vrs_enabled; uint8_t _pad[3]; } DVSMProfile;
typedef struct { uint64_t frame_index; uint64_t dispatch_ns; uint64_t complete_ns;
                 uint32_t step_count; DVSMState state_snap; float frame_gen_err;
                 float wattage_tdp; uint64_t hash_chain; } DVSMFrameReplay;

void*    dvsm_create        (DVSMProfile p);
void     dvsm_destroy       (void* h);
void     dvsm_tick          (void* h, uint64_t t0, uint64_t t1, DVSMFrameReplay* out);
float    dvsm_vrs_rate      (void* h);
void     dvsm_set_profile   (void* h, DVSMProfile p);
uint32_t dvsm_verify_replay (const DVSMFrameReplay* frames, uint32_t n);
#ifdef __cplusplus
}
#endif
