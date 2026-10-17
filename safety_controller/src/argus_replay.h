/**
 * @file argus_replay.h
 * @brief PS-side replay: pulls dataset samples from the host relay over UDP
 *        and keeps the PL's ping-pong BRAM fed.
 *
 * Wraps argus_replay_client.h in lwIP raw callbacks. The client owns the
 * protocol; this file owns transport, the destination, and the streaming
 * state machine that decides which half to fill next.
 *
 * Two ways to use it, both after one call to argus_replay_init():
 *
 * - **One-shot:** argus_replay_start_fetch() into a caller-supplied
 *   buffer, then argus_replay_service() until argus_replay_is_done().
 *   main.c does this once at boot to prove the host-to-PS path before
 *   the PL depends on it.
 * - **Streaming:** argus_replay_stream_begin() primes both BRAM halves,
 *   switches the chain to external mode, then argus_replay_stream_service()
 *   refills whichever half the PL reports consumed. Runs forever from the
 *   main loop.
 */
#ifndef ARGUS_REPLAY_H
#define ARGUS_REPLAY_H

#include <stdint.h>

/* Local UDP port the PS binds for replay traffic. Not part of the wire
 * contract -- the relay answers whatever source address a request came from
 * -- but binding explicitly keeps captures readable. */
#define ARGUS_REPLAY_LOCAL_PORT 5011u

/* Creates the PCB, binds it, registers the receive callback, and initialises
 * the client. Returns 0 on success. */
int argus_replay_init(void);

/* --- one-shot -------------------------------------------------------------- */

/* Issues a request for one buffer half starting at `sample_offset` into
 * `dst`, which must hold ARGUS_REPLAY_SAMPLES_PER_HALF x ARGUS_MAX_CHANNELS
 * samples. Returns 0 if the request went out. */
int argus_replay_start_fetch(uint32_t sample_offset, uint16_t *dst);

/* Drive from the main loop. Handles retransmit deadlines. */
void argus_replay_service(void);

int argus_replay_is_done(void);
int argus_replay_succeeded(void);

/* Prints client counters over UART. */
void argus_replay_report(void);

/* --- streaming ------------------------------------------------------------- */

/* Starts the ping-pong. Fetches half 0, then half 1, then puts the chain in
 * external mode with a soft reset so playback starts at row 0 of half 0.
 * Non-blocking: progress happens in argus_replay_stream_service(). */
void argus_replay_stream_begin(uint32_t first_sample);

/* Drive from the main loop. Calls argus_replay_service() itself. */
void argus_replay_stream_service(void);

/* Main-loop accounting for the stream report: call once per iteration
 * with what xemacif_input() returned (packets delivered). */
void argus_replay_note_loop(int packets);

/* Streaming counters over UART: halves filled, underruns seen, retries,
 * and where the per-packet time goes. */
void argus_replay_stream_report(void);

#endif /* ARGUS_REPLAY_H */
