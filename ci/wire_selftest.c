/* ci/wire_selftest.c
 *
 * Standalone ABI check for argus_wire.h
 *
 * Lives outside safety_controller/src on purpose: that directory
 * is collected by file(GLOB) into the firmware build, and a second
 * main() there breaks the link. CI compiles this file with the
 * -Isafety_controller/src, so the quoted include still resolves to
 * the vendored header; checking the copy the firmware actually
 * uses, not the one in argus_core.
 *
 * Run under both the host compiler and arm-none-eabi-gcc. The
 * static_asserts in the header fire at compile time under each; the
 * runtime checks below only execute on the host, which is enough,
 * since a packing difference between the two toolchains shows up as
 * a compile failure on the target.
 *
 * argus_core has an equivalent gtest, but it compiles the header as
 * c++. static_assert is an assert.h macro before C23, and struct
 * layout rules differ subtly between the languages, so a C-only
 * check is not redundant.
 *  */
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>

#include "argus_wire.h"

static int failures;

static void check_size(const char *what, size_t actual, size_t expected)
{
    if (actual != expected) {
        printf("FAIL %-34s %zu, expected %zu\n", what, actual, expected);
        failures++;
    }
}

static void check_u16(const char * what, uint16_t actual, uint16_t expected)
{
    if (actual != expected) {
        printf("FAIL %-34s 0x%04X, expected 0x%04X\n", what, actual, expected);
        failures++;
    }
}

int main(void)
{
    /* primitive assumptions */
    check_size("sizeof(float)", sizeof(float), 4);
    check_size("sizeof(uint16_t)", sizeof(uint16_t), 2);

    /* telemetry frame */
    check_size("sizeof(argus_frame_packet_t)", sizeof(argus_frame_packet_t), 594);
    check_size("frame.magic", offsetof(argus_frame_packet_t, magic), 0);
    check_size("frame.sample", offsetof(argus_frame_packet_t, sample), 4);
    check_size("frame.t", offsetof(argus_frame_packet_t, t), 8);
    check_size("frame.version", offsetof(argus_frame_packet_t, version), 12);
    check_size("frame.reserved", offsetof(argus_frame_packet_t, reserved), 13);
    check_size("frame.channel_count", offsetof(argus_frame_packet_t, channel_count), 14);
    check_size("frame.channels", offsetof(argus_frame_packet_t, channels), 16);
    check_size("frame.power", offsetof(argus_frame_packet_t, power), 208);
    check_size("frame.crc", offsetof(argus_frame_packet_t, crc), 592);

    /* replay request */
    check_size("sizeof(argus_replay_request_t)", sizeof(argus_replay_request_t), 22);
    check_size("req.magic", offsetof(argus_replay_request_t, magic), 0);
    check_size("req.version", offsetof(argus_replay_request_t, version), 4);
    check_size("req.type", offsetof(argus_replay_request_t, type), 6);
    check_size("req.seq", offsetof(argus_replay_request_t, seq), 8);
    check_size("req.sample_offset", offsetof(argus_replay_request_t, sample_offset), 12);
    check_size("req.sample_count", offsetof(argus_replay_request_t, sample_count), 16);
    check_size("req.flags", offsetof(argus_replay_request_t, flags), 18);
    check_size("req.crc", offsetof(argus_replay_request_t, crc), 20);

    /* replay chunk header */
    check_size("sizeof(argus_replay_chunk_hdr_t)", sizeof(argus_replay_chunk_hdr_t), 28);
    check_size("chunk.magic", offsetof(argus_replay_chunk_hdr_t, magic), 0);
    check_size("chunk.version", offsetof(argus_replay_chunk_hdr_t, version), 4);
    check_size("chunk.type", offsetof(argus_replay_chunk_hdr_t, type), 6);
    check_size("chunk.seq", offsetof(argus_replay_chunk_hdr_t, seq), 8);
    check_size("chunk.sample_offset", offsetof(argus_replay_chunk_hdr_t, sample_offset), 12);
    check_size("chunk.sample_count", offsetof(argus_replay_chunk_hdr_t, sample_count), 16);
    check_size("chunk.chunk_index", offsetof(argus_replay_chunk_hdr_t, chunk_index), 18);
    check_size("chunk.chunk_total", offsetof(argus_replay_chunk_hdr_t, chunk_total), 20);
    check_size("chunk.channel_count", offsetof(argus_replay_chunk_hdr_t, channel_count), 22);
    check_size("chunk.payload_len", offsetof(argus_replay_chunk_hdr_t, payload_len), 24);
    check_size("chunk.crc", offsetof(argus_replay_chunk_hdr_t, crc), 26);

    /* CRC */
    {
        /* Standard check value for CRC-16/CCITT-FALSE. */
        static const uint8_t check[] = {'1', '2', '3', '4', '5', '6', '7', '8', '9'};
        uint16_t split;

        check_u16("crc16_ccitt(\"123456789\")", crc16_ccitt(check, sizeof(check)), 0x29B1u);

        /* The replay path CRCs a header and a payload that are not
         * contiguous. If the resumable variant ever diverges from
         * the one-shot, the PS silently rejects every chunk. */
        split = crc16_ccitt_update(0xFFFFu, check, 4);
        split = crc16_ccitt_update(split, check +  4, 5);
        check_u16("crc16_ccitt_update split", split, crc16_ccitt(check, sizeof(check)));
    }

    /* datagram sizing */
    {
        const size_t payload = (size_t)ARGUS_REPLAY_SAMPLES_PER_CHUNK * ARGUS_MAX_CHANNELS * sizeof(uint16_t);
        if (payload > ARGUS_REPLAY_MAX_PAYLOAD) {
            printf("FAIL chunk payload %zu exceeds ARGUS_REPLAY_MAX_PAYLOAD %u\n",
           payload, (unsigned)ARGUS_REPLAY_MAX_PAYLOAD);
           failures++;

           /* 1500 MTU - 20 IP - 8 UDP. Exceeding this fragments
            * every chunk, which lwIP on the PS will not reassemble
            * by default. */
           if (sizeof(argus_replay_chunk_hdr_t) + payload > 1472u)
           {
            printf("FAIL chunk datagram %zu exceeds 1472\n",
                sizeof(argus_replay_chunk_hdr_t) + payload);
            failures++;
           }
        }

        if (failures != 0) {
            printf("wire_selftest: %d failure(s)\n", failures);
            return 1;
        }

        printf("wire_selftest: OK frame=%zu req=%zu chunk_hdr=%zu channels=%d\n",
            sizeof(argus_frame_packet_t),
            sizeof(argus_replay_request_t),
            sizeof(argus_replay_chunk_hdr_t),
            ARGUS_MAX_CHANNELS);
        return 0;
    }
}


const size_t argus_selftest_frame_size   = sizeof(argus_frame_packet_t);
const size_t argus_selftest_request_size = sizeof(argus_replay_request_t);
const size_t argus_selftest_chunk_size   = sizeof(argus_replay_chunk_hdr_t);
