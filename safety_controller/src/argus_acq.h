/* argus_acq.h
 *
 * PS-side view of argus_acq_top: register offsets, bit definitions, and the
 * replay BRAM layout. Mirrors the register map comment in argus_acq_axi.vhd;
 * if the two disagree, the hardware wins and this is wrong.
 *
 * Base addresses are pinned in the block design (tools/bd_add_acq.tcl and
 * tools/bd_add_bram.tcl) so they survive regeneration. They are hardcoded
 * here rather than taken from xparameters.h because module references get
 * no driver entry in the SDT flow.
 */

#ifndef ARGUS_ACQ_H
#define ARGUS_ACQ_H

#include <stdint.h>
#include "xil_io.h"
#include "argus_wire.h"

/* --- register block ------------------------------------------------------ */

#define ARGUS_ACQ_BASE            0x43C00000u

#define ARGUS_ACQ_CTRL            (ARGUS_ACQ_BASE + 0x000u)
#define ARGUS_ACQ_STATUS          (ARGUS_ACQ_BASE + 0x004u)
#define ARGUS_ACQ_FRAME_INDEX     (ARGUS_ACQ_BASE + 0x008u)
#define ARGUS_ACQ_ID              (ARGUS_ACQ_BASE + 0x00Cu)
#define ARGUS_ACQ_REPLAY_STATUS   (ARGUS_ACQ_BASE + 0x010u)
#define ARGUS_ACQ_REPLAY_ACK      (ARGUS_ACQ_BASE + 0x014u)
#define ARGUS_ACQ_FEATURE_INDEX   (ARGUS_ACQ_BASE + 0x018u)
#define ARGUS_ACQ_FEAT_DROPPED    (ARGUS_ACQ_BASE + 0x01Cu)
#define ARGUS_ACQ_FRAME_BASE      (ARGUS_ACQ_BASE + 0x100u)
#define ARGUS_ACQ_FEATURE_BASE    (ARGUS_ACQ_BASE + 0x400u)

/* Fabric revision. Bumped with id_value in argus_acq_axi.vhd on any change
 * this header depends on; acq_smoke_test() checks it before anything else.
 * ACQ2 was the first build with CTRL.hold / STATUS.held; ACQ3 adds the
 * feature bank. */
#define ARGUS_ACQ_ID_EXPECT       0x41435133u   /* "ACQ3" */
#define ARGUS_ACQ_CH_PER_CHIP     32

/* The fabric's sweep rate, 125 MHz / (35 slots x 119 clocks), and the bin
 * length argus_feature accumulates over. FRAME_INDEX / SWEEP_HZ is a frame's
 * time in seconds since the chain was reset; FEATURE_INDEX * BIN_LEN /
 * SWEEP_HZ is a bin's. Both match the generics in argus_feature.vhd. */
#define ARGUS_ACQ_SWEEP_HZ        30012.0f
#define ARGUS_ACQ_BIN_LEN         1500u

/* CTRL */
#define ARGUS_ACQ_CTRL_ENABLE     (1u << 0)
#define ARGUS_ACQ_CTRL_SOFT_RESET (1u << 1)
#define ARGUS_ACQ_CTRL_EXT_MODE   (1u << 2)
/* Freeze the frame read bank. The assembler keeps sweeping and discards
 * what it completes, so hold for as long as a coherent read needs and no
 * longer. FRAME_INDEX freezes with it and names the frame being read. */
#define ARGUS_ACQ_CTRL_HOLD       (1u << 3)
/* Same for the feature bank. A bin that completes under hold is deferred,
 * not lost, unless the hold outlasts a whole bin (FEAT_DROPPED counts). */
#define ARGUS_ACQ_CTRL_FEAT_HOLD  (1u << 4)

/* STATUS */
#define ARGUS_ACQ_STATUS_READY    (1u << 0)
#define ARGUS_ACQ_STATUS_OVERRUN  (1u << 1)
/* The freeze is in effect and the bank has settled. Poll this after
 * setting CTRL.hold; do not assume the write took immediately. */
#define ARGUS_ACQ_STATUS_HELD     (1u << 2)
#define ARGUS_ACQ_STATUS_FEAT_HELD (1u << 3)

/* FEATURE[n] is two words at FEATURE_BASE + 8n: the low 32 bits of the
 * bin's sum of squared high-passed samples, then the crossing count in the
 * high half-word over the sum's top 16 bits. Sum / BIN_LEN is the mean
 * square in ADC code^2 -- Willett's spikePow up to a constant. */
#define ARGUS_ACQ_FEATURE_LO(n)   (ARGUS_ACQ_FEATURE_BASE + 8u * (uint32_t)(n))
#define ARGUS_ACQ_FEATURE_HI(n)   (ARGUS_ACQ_FEATURE_BASE + 8u * (uint32_t)(n) + 4u)
#define ARGUS_ACQ_FEAT_COUNT(hi)  ((uint16_t)((hi) >> 16))
#define ARGUS_ACQ_FEAT_SUM(hi, lo) ((((uint64_t)(hi) & 0xFFFFu) << 32) | (uint64_t)(lo))

/* REPLAY_STATUS */
#define ARGUS_ACQ_RS_PLAY_HALF    (1u << 0)
#define ARGUS_ACQ_RS_CONSUMED0    (1u << 1)
#define ARGUS_ACQ_RS_CONSUMED1    (1u << 2)
#define ARGUS_ACQ_RS_UNDERRUN     (1u << 3)
#define ARGUS_ACQ_RS_ROW(v)       (((v) >> 8) & 0xFFFFu)

/* REPLAY_ACK -- write 1 to clear */
#define ARGUS_ACQ_RA_CONSUMED0    (1u << 0)
#define ARGUS_ACQ_RA_CONSUMED1    (1u << 1)
#define ARGUS_ACQ_RA_UNDERRUN     (1u << 2)

/* --- replay BRAM ---------------------------------------------------------- */

/* Two halves, sample-major channel-minor, matching the relay's chunk payload
 * so chunks are copied in unchanged. Sized for ARGUS_REPLAY_SAMPLES_PER_HALF
 * rows of ARGUS_MAX_CHANNELS 16-bit samples. */
#define ARGUS_BRAM_BASE           0x40000000u
#define ARGUS_BRAM_SIZE           0x10000u
#define ARGUS_BRAM_HALF_BYTES     (ARGUS_REPLAY_SAMPLES_PER_HALF * ARGUS_MAX_CHANNELS * 2u)
#define ARGUS_BRAM_HALF(h)        ((uint16_t *)(ARGUS_BRAM_BASE + (uint32_t)(h) * ARGUS_BRAM_HALF_BYTES))

#if (2 * ARGUS_REPLAY_SAMPLES_PER_HALF * ARGUS_MAX_CHANNELS * 2) > 0x10000
#error "two replay halves do not fit the 64 KB BRAM aperture"
#endif

/* --- helpers -------------------------------------------------------------- */

static inline uint32_t argus_acq_rd(uint32_t reg)              { return Xil_In32(reg); }
static inline void     argus_acq_wr(uint32_t reg, uint32_t v)  { Xil_Out32(reg, v); }

#endif /* ARGUS_ACQ_H */
