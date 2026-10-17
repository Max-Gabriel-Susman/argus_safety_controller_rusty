/* argus_replay_client.h
 *
 * Transport-agnostic client for the Argus replay protocol.
 *
 * One implementation, two transports. The host test client wraps this in BSD
 * sockets; the Zynq PS wraps it in lwIP raw callbacks. Neither owns the
 * protocol logic, so a fix lands once.
 *
 * Constraints, driven by the bare-metal side:
 *   - C99, no allocation, no stdio, no threads, no exceptions.
 *   - Caller owns every buffer. This struct stores pointers, never memory.
 *   - Transport is injected as a send callback plus a receive entry point,
 *     so it works with a blocking read loop or an interrupt callback.
 *   - Time is supplied by the caller, so it works off a systick or a
 *     clock_gettime.
 *
 * Header-only by the same reasoning as argus_wire.h: everything is static
 * inline, so vendoring is a file copy and no build system changes.
 *
 * USAGE
 *   argus_replay_client_init(&cl, send_fn, ctx, 96, 200, 3);
 *   argus_replay_client_fetch(&cl, offset, 147, dst, now_ms);
 *   ... on each received datagram:  argus_replay_client_on_packet(&cl, buf, n);
 *   ... periodically:               argus_replay_client_poll(&cl, now_ms);
 *   until cl.state is COMPLETE or FAILED.
 */

#ifndef ARGUS_CORE_ARGUS_REPLAY_CLIENT_H
#define ARGUS_CORE_ARGUS_REPLAY_CLIENT_H

#include <stdint.h>
#include <stddef.h>
#include <string.h>

#include "argus_wire.h"

/* A burst is bounded by SAMPLES_PER_HALF divided by the smallest chunk that
 * fits an MTU. At 124 channels that is 5 samples per chunk, so 30 chunks.
 * 32 gives headroom and lets the arrival set be one uint32_t. */
#define ARGUS_REPLAY_MAX_CHUNKS 32u

typedef enum {
    ARGUS_REPLAY_IDLE = 0,
    ARGUS_REPLAY_WAITING,   /* request outstanding                     */
    ARGUS_REPLAY_COMPLETE,  /* every chunk landed; dst is filled       */
    ARGUS_REPLAY_FAILED     /* retries exhausted; dst is partial       */
} argus_replay_state_t;

/* Return 0 on success. The client does not retry on a send failure -- that is
 * a local fault, distinct from a lost datagram, and the caller sees it as a
 * return code from fetch() or poll(). */
typedef int (*argus_replay_send_fn)(void *ctx, const void *data, uint16_t len);

typedef struct {
    /* transport */
    argus_replay_send_fn send;
    void                *send_ctx;

    /* configuration */
    uint16_t channel_count;
    uint16_t samples_per_chunk;   /* derived from channel_count           */
    uint32_t timeout_ms;
    uint8_t  max_retries;

    /* destination, owned by the caller */
    uint16_t *dst;
    uint32_t  dst_capacity_samples;

    /* current fetch */
    argus_replay_state_t state;
    uint32_t seq;
    uint32_t base_offset;
    uint16_t total_samples;
    uint16_t total_chunks;
    uint32_t arrived;             /* bitmap over the original burst       */
    uint16_t arrived_count;
    uint32_t deadline_ms;
    uint8_t  retries;

    /* Retransmits restart chunk_index at zero, so responses carry an offset
     * into the original burst rather than an absolute position. */
    uint16_t req_first_chunk;
    uint16_t req_chunks;

    /* counters, for status reporting */
    uint32_t requests_sent;
    uint32_t retransmits_sent;
    uint32_t chunks_accepted;
    uint32_t chunks_rejected;
    uint32_t timeouts;
} argus_replay_client_t;

/* --- internal helpers ---------------------------------------------------- */

static inline uint16_t argus_replay_chunk_capacity(uint16_t channel_count)
{
    if (channel_count == 0u) {
        return 0u;
    }
    return (uint16_t)(ARGUS_REPLAY_MAX_PAYLOAD / ((uint32_t)channel_count * 2u));
}

static inline int argus_replay_send_request(
    argus_replay_client_t *cl, uint32_t offset, uint16_t count,
    uint16_t flags, uint32_t now_ms)
{
    argus_replay_request_t req;
    memset(&req, 0, sizeof(req));

    req.magic         = ARGUS_FRAME_MAGIC;
    req.version       = ARGUS_REPLAY_VERSION;
    req.type          = ARGUS_MSG_REPLAY_REQUEST;
    req.seq           = cl->seq;
    req.sample_offset = offset;
    req.sample_count  = count;
    req.flags         = flags;
    req.crc = crc16_ccitt((const uint8_t *)&req,
                          offsetof(argus_replay_request_t, crc));

    cl->deadline_ms = now_ms + cl->timeout_ms;
    cl->state       = ARGUS_REPLAY_WAITING;

    if (flags & ARGUS_REPLAY_F_RETRANSMIT) {
        cl->retransmits_sent++;
    } else {
        cl->requests_sent++;
    }

    return cl->send(cl->send_ctx, &req, (uint16_t)sizeof(req));
}

/* --- public API ---------------------------------------------------------- */

/* Returns 0 on success, -1 if the channel count cannot fit one MTU. */
static inline int argus_replay_client_init(
    argus_replay_client_t *cl,
    argus_replay_send_fn send, void *send_ctx,
    uint16_t channel_count, uint32_t timeout_ms, uint8_t max_retries)
{
    memset(cl, 0, sizeof(*cl));

    cl->samples_per_chunk = argus_replay_chunk_capacity(channel_count);
    if (cl->samples_per_chunk == 0u) {
        return -1;
    }

    cl->send          = send;
    cl->send_ctx      = send_ctx;
    cl->channel_count = channel_count;
    cl->timeout_ms    = timeout_ms;
    cl->max_retries   = max_retries;
    cl->state         = ARGUS_REPLAY_IDLE;
    return 0;
}

/* Begin a fetch. `dst` must hold sample_count * channel_count uint16 values
 * and stay alive until the fetch completes; on the PS this is one half of the
 * ping-pong BRAM, written through directly.
 *
 * Returns 0 if the request went out, -1 on a parameter or send fault. */
static inline int argus_replay_client_fetch(
    argus_replay_client_t *cl, uint32_t sample_offset, uint16_t sample_count,
    uint16_t *dst, uint32_t now_ms)
{
    uint16_t chunks;

    if (sample_count == 0u || dst == NULL) {
        return -1;
    }

    chunks = (uint16_t)((sample_count + cl->samples_per_chunk - 1u) /
                        cl->samples_per_chunk);
    if (chunks > ARGUS_REPLAY_MAX_CHUNKS) {
        return -1;
    }

    cl->dst                  = dst;
    cl->dst_capacity_samples = sample_count;
    cl->base_offset          = sample_offset;
    cl->total_samples        = sample_count;
    cl->total_chunks         = chunks;
    cl->arrived              = 0u;
    cl->arrived_count        = 0u;
    cl->retries              = 0u;
    cl->req_first_chunk      = 0u;
    cl->req_chunks           = chunks;
    cl->seq++;

    return argus_replay_send_request(cl, sample_offset, sample_count, 0u, now_ms);
}

/* Feed one received datagram. Malformed, stale, and duplicate packets are
 * counted and dropped -- never fatal, because a UDP endpoint sees all three
 * in normal operation. */
static inline void argus_replay_client_on_packet(
    argus_replay_client_t *cl, const uint8_t *data, uint16_t len)
{
    argus_replay_chunk_hdr_t hdr;
    uint16_t crc;
    uint16_t global_chunk;
    uint16_t expect_samples;
    size_t   row_bytes;
    size_t   dst_offset;

    if (cl->state != ARGUS_REPLAY_WAITING) {
        return;
    }
    if (len < sizeof(hdr)) {
        cl->chunks_rejected++;
        return;
    }

    /* memcpy rather than a cast: the receive buffer carries no alignment
     * guarantee, and unaligned struct access faults on Cortex-A9 for some
     * member types. */
    memcpy(&hdr, data, sizeof(hdr));

    if (hdr.magic != ARGUS_FRAME_MAGIC ||
        hdr.version != ARGUS_REPLAY_VERSION ||
        hdr.type != ARGUS_MSG_REPLAY_CHUNK) {
        cl->chunks_rejected++;
        return;
    }
    if (hdr.seq != cl->seq) {
        /* Response to a superseded request. Expected after a retransmit. */
        cl->chunks_rejected++;
        return;
    }
    if (hdr.channel_count != cl->channel_count) {
        cl->chunks_rejected++;
        return;
    }
    if (hdr.chunk_total != cl->req_chunks) {
        cl->chunks_rejected++;
        return;
    }
    if (hdr.chunk_index >= hdr.chunk_total) {
        cl->chunks_rejected++;
        return;
    }
    if (len != sizeof(hdr) + hdr.payload_len) {
        cl->chunks_rejected++;
        return;
    }
    if (hdr.payload_len !=
        (uint16_t)((uint32_t)hdr.sample_count * cl->channel_count * 2u)) {
        cl->chunks_rejected++;
        return;
    }

    crc = crc16_ccitt(data, offsetof(argus_replay_chunk_hdr_t, crc));
    crc = crc16_ccitt_update(crc, data + sizeof(hdr), hdr.payload_len);
    if (crc != hdr.crc) {
        cl->chunks_rejected++;
        return;
    }

    global_chunk = (uint16_t)(cl->req_first_chunk + hdr.chunk_index);
    if (global_chunk >= cl->total_chunks) {
        cl->chunks_rejected++;
        return;
    }
    if (cl->arrived & (1u << global_chunk)) {
        cl->chunks_rejected++;   /* duplicate */
        return;
    }

    /* Every chunk but the last carries a full load. Checking this catches a
     * server that sizes chunks differently from us, which would otherwise
     * scatter samples to the wrong offsets. */
    expect_samples = cl->samples_per_chunk;
    if (global_chunk == cl->total_chunks - 1u) {
        expect_samples = (uint16_t)(cl->total_samples -
                                    (uint32_t)global_chunk * cl->samples_per_chunk);
    }
    if (hdr.sample_count != expect_samples) {
        cl->chunks_rejected++;
        return;
    }

    /* Destination is derived from chunk_index, not sample_offset: when the
     * dataset wraps mid-burst the offsets are not monotonic, but the index
     * always is. */
    row_bytes  = (size_t)cl->channel_count * 2u;
    dst_offset = (size_t)global_chunk * cl->samples_per_chunk * row_bytes;

    if (dst_offset + hdr.payload_len >
        (size_t)cl->dst_capacity_samples * row_bytes) {
        cl->chunks_rejected++;
        return;
    }

    memcpy((uint8_t *)cl->dst + dst_offset, data + sizeof(hdr), hdr.payload_len);

    cl->arrived |= (1u << global_chunk);
    cl->arrived_count++;
    cl->chunks_accepted++;

    if (cl->arrived_count == cl->total_chunks) {
        cl->state = ARGUS_REPLAY_COMPLETE;
    }
}

/* Call periodically. Reissues the first contiguous run of missing chunks when
 * the deadline passes; gives up after max_retries. */
static inline void argus_replay_client_poll(
    argus_replay_client_t *cl, uint32_t now_ms)
{
    uint16_t first = 0xFFFFu;
    uint16_t last;
    uint16_t n_chunks;
    uint16_t count;
    uint32_t offset;
    uint16_t i;

    if (cl->state != ARGUS_REPLAY_WAITING) {
        return;
    }
    /* Unsigned difference, so a wrapping millisecond counter is handled. */
    if ((uint32_t)(now_ms - cl->deadline_ms) > 0x7FFFFFFFu) {
        return;   /* deadline still ahead */
    }

    cl->timeouts++;

    if (cl->retries >= cl->max_retries) {
        cl->state = ARGUS_REPLAY_FAILED;
        return;
    }
    cl->retries++;

    for (i = 0u; i < cl->total_chunks; i++) {
        if ((cl->arrived & (1u << i)) == 0u) {
            first = i;
            break;
        }
    }
    if (first == 0xFFFFu) {
        cl->state = ARGUS_REPLAY_COMPLETE;   /* nothing missing after all */
        return;
    }

    last = first;
    while ((uint16_t)(last + 1u) < cl->total_chunks &&
           (cl->arrived & (1u << (last + 1u))) == 0u) {
        last++;
    }

    n_chunks = (uint16_t)(last - first + 1u);
    offset   = cl->base_offset + (uint32_t)first * cl->samples_per_chunk;

    if (last == cl->total_chunks - 1u) {
        count = (uint16_t)(cl->total_samples -
                           (uint32_t)first * cl->samples_per_chunk);
    } else {
        count = (uint16_t)(n_chunks * cl->samples_per_chunk);
    }

    /* New seq so late chunks from the previous attempt are dropped rather
     * than landing at the wrong offsets. */
    cl->seq++;
    cl->req_first_chunk = first;
    cl->req_chunks      = n_chunks;

    (void)argus_replay_send_request(cl, offset, count,
                                    ARGUS_REPLAY_F_RETRANSMIT, now_ms);
}

static inline int argus_replay_client_is_done(const argus_replay_client_t *cl)
{
    return cl->state == ARGUS_REPLAY_COMPLETE || cl->state == ARGUS_REPLAY_FAILED;
}

#endif /* ARGUS_CORE_ARGUS_REPLAY_CLIENT_H */
