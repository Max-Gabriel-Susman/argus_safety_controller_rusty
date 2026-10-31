/* shim.c -- the C side of the differential tests.
 *
 * Compiles the firmware's own argus_replay_client.h, unchanged, and wraps it
 * in a handful of functions Rust can call. The wrapper adds nothing to the
 * protocol: it supplies the send callback (which records what was sent),
 * owns the destination buffer, and copies the client's state out so the
 * Rust side can compare it.
 *
 * All the shim's state lives in one block of memory that Rust allocates and
 * passes in on every call. Every function takes fixed-size arrays rather
 * than a pointer and a length, so the Rust declarations of them can take
 * references and stay safe to call.
 */

#include <stdint.h>
#include <stddef.h>
#include <string.h>

#include "argus_replay_client.h"

/* The largest destination a fetch can need: every chunk full. */
#define SHIM_DST_BYTES   (ARGUS_REPLAY_MAX_CHUNKS * ARGUS_REPLAY_MAX_PAYLOAD)
/* Room for the last request sent; a request is 22 bytes. */
#define SHIM_SENT_BYTES  64u
/* Room for one received datagram, more than a chunk can be. */
#define SHIM_PACKET_BYTES 2048u

typedef struct {
    argus_replay_client_t cl;
    int      send_fails;       /* nonzero: the send callback reports failure */
    uint32_t sends;            /* send attempts, including failed ones       */
    uint32_t sent_len;         /* length of the last attempt                 */
    uint8_t  sent[SHIM_SENT_BYTES];
    uint16_t dst[SHIM_DST_BYTES / 2u];
} shim_t;

/* What Rust sees of the client after each step. Every field is 32 bits
 * apart from the bytes at the end, so the layout has no padding and the
 * Rust mirror of it is plain. */
typedef struct {
    uint32_t state;
    uint32_t seq;
    uint32_t arrived_count;
    uint32_t total_chunks;
    uint32_t samples_per_chunk;
    uint32_t requests_sent;
    uint32_t retransmits_sent;
    uint32_t chunks_accepted;
    uint32_t chunks_rejected;
    uint32_t timeouts;
    uint32_t sends;
    uint32_t sent_len;
    uint8_t  sent[SHIM_SENT_BYTES];
} shim_snapshot_t;

/* The size of the block Rust must allocate; Rust checks its own figure
 * against this before the first call. */
size_t shim_bytes(void)
{
    return sizeof(shim_t);
}

static int shim_send(void *ctx, const void *data, uint16_t len)
{
    shim_t *s = ctx;
    size_t n = len < SHIM_SENT_BYTES ? len : SHIM_SENT_BYTES;

    s->sends++;
    s->sent_len = len;
    memset(s->sent, 0, sizeof(s->sent));
    memcpy(s->sent, data, n);
    return s->send_fails ? -1 : 0;
}

/* The client keeps two pointers into the block: its send context and its
 * destination. Pointing them at the block again on every call keeps them
 * right even if Rust has moved the block since. */
static shim_t *shim_at(void *mem)
{
    shim_t *s = mem;
    s->cl.send_ctx = s;
    if (s->cl.dst != NULL) {
        s->cl.dst = s->dst;
    }
    return s;
}

/* Returns argus_replay_client_init()'s result, or -2 if the block is too
 * small or misaligned for a shim_t. */
int shim_init(void *mem, size_t size, uint16_t channel_count,
              uint32_t timeout_ms, uint8_t max_retries)
{
    shim_t *s = mem;

    if (size < sizeof(shim_t) || ((uintptr_t)mem % _Alignof(shim_t)) != 0u) {
        return -2;
    }
    memset(s, 0, sizeof(*s));
    return argus_replay_client_init(&s->cl, shim_send, s, channel_count,
                                    timeout_ms, max_retries);
}

void shim_set_send_fails(void *mem, int fails)
{
    shim_at(mem)->send_fails = fails;
}

int shim_fetch(void *mem, uint32_t sample_offset, uint16_t sample_count,
               uint32_t now_ms)
{
    shim_t *s = shim_at(mem);
    return argus_replay_client_fetch(&s->cl, sample_offset, sample_count,
                                     s->dst, now_ms);
}

void shim_on_packet(void *mem, const uint8_t (*packet)[SHIM_PACKET_BYTES],
                    uint16_t len)
{
    shim_t *s = shim_at(mem);
    if (len > SHIM_PACKET_BYTES) {
        len = SHIM_PACKET_BYTES;
    }
    argus_replay_client_on_packet(&s->cl, *packet, len);
}

void shim_poll(void *mem, uint32_t now_ms)
{
    argus_replay_client_poll(&shim_at(mem)->cl, now_ms);
}

void shim_snapshot(void *mem, shim_snapshot_t *out)
{
    const shim_t *s = shim_at(mem);

    out->state             = (uint32_t)s->cl.state;
    out->seq               = s->cl.seq;
    out->arrived_count     = s->cl.arrived_count;
    out->total_chunks      = s->cl.total_chunks;
    out->samples_per_chunk = s->cl.samples_per_chunk;
    out->requests_sent     = s->cl.requests_sent;
    out->retransmits_sent  = s->cl.retransmits_sent;
    out->chunks_accepted   = s->cl.chunks_accepted;
    out->chunks_rejected   = s->cl.chunks_rejected;
    out->timeouts          = s->cl.timeouts;
    out->sends             = s->sends;
    out->sent_len          = s->sent_len;
    memcpy(out->sent, s->sent, sizeof(out->sent));
}

/* The offset of the first byte where the C destination differs from
 * `other`, or -1 if they are equal. memcmp first, because almost every
 * call finds them equal. */
int64_t shim_dst_diff(void *mem, const uint8_t (*other)[SHIM_DST_BYTES])
{
    const uint8_t *mine = (const uint8_t *)shim_at(mem)->dst;
    size_t i;

    if (memcmp(mine, *other, SHIM_DST_BYTES) == 0) {
        return -1;
    }
    for (i = 0u; i < SHIM_DST_BYTES; i++) {
        if (mine[i] != (*other)[i]) {
            break;
        }
    }
    return (int64_t)i;
}

/* The packing half of argus_send_frame() in argus_net.c, copied as it
 * stands there; the lwIP send that follows it is left out. */
void shim_pack_frame(uint32_t sample, float t,
                     const uint16_t (*ch)[ARGUS_MAX_CHANNELS],
                     const uint32_t (*power)[ARGUS_MAX_CHANNELS],
                     uint8_t (*out)[sizeof(argus_frame_packet_t)])
{
    argus_frame_packet_t pkt = {0};
    pkt.magic = ARGUS_FRAME_MAGIC;
    pkt.sample = sample;
    pkt.t = t;
    pkt.version = ARGUS_FRAME_VERSION;
    pkt.channel_count = ARGUS_MAX_CHANNELS;
    memcpy(pkt.channels, *ch, sizeof(pkt.channels));
    memcpy(pkt.power, *power, sizeof(pkt.power)); /* v3: sum / BIN_LEN */
    pkt.crc = crc16_ccitt((const uint8_t *)&pkt, offsetof(argus_frame_packet_t, crc));

    memcpy(*out, &pkt, sizeof(pkt));
}
