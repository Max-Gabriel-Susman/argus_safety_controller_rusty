#include "xil_printf.h"
#include "xparameters.h"
#include "xil_cache.h"
#include "sleep.h"
#include "xiltimer.h"
#include "netif/xadapter.h"
#include "lwip/init.h"
#include "lwip/timeouts.h"
#include "lwip/sys.h"
#include "argus_net.h"
#include "argus_wire.h"
#include "argus_acq.h"
#include "argus_replay.h"
#include "argus_console.h"

/* Bins between status lines: 100 x 50 ms = 5 s. */
#define STATUS_EVERY_BINS 100

/* Bounded wait for the first replay request to leave. lwIP queues packets
 * for an unresolved destination when ARP_QUEUEING is on, so this normally
 * succeeds first try; it only matters if queueing is disabled. Pumping the
 * stack between attempts is what lets ARP complete either way. */
#define REPLAY_SEND_ATTEMPTS 200
#define REPLAY_SEND_GAP_US   10000

/* A9 global timer, 333.333 MHz. Matches ARGUS_TIMER_HZ in xtopology.c;
 * the 0.1% truncation is irrelevant at microsecond resolution. */
#define ARGUS_TICKS_PER_US 333u

static struct netif server_netif;
static unsigned char mac_ethernet_address[] = {0x00,0x0a,0x35,0x00,0x01,0x02};

/* One buffer half in DDR, for the one-shot network check only. Streaming
 * fetches land in BRAM directly. */
static uint16_t g_fetch_buf[ARGUS_REPLAY_SAMPLES_PER_HALF * ARGUS_MAX_CHANNELS];

/* --- acquisition chain checks ------------------------------------------- */

/* Frame read under a hardware hold.
 *
 * This was a seqlock. It could never have worked: measured on this board,
 * a GP0 read costs 1083 ns and all 96 words come to 114 us, against a
 * 33.3 us sweep. The read is 3.4x too slow to fit between two bank swaps,
 * so every attempt tore and no number of retries would have changed that.
 *
 * The assembler freezes the read bank instead. CTRL.hold stops the bank
 * alternating and STATUS.held acknowledges that the freeze is in effect --
 * poll it rather than assuming the write took immediately, since one last
 * swap can land on the clock hold registers. FRAME_INDEX freezes alongside
 * and names the frame actually in the bank.
 *
 * The writer does not stall. It keeps filling its own bank and discards
 * what it completes, so a read costs roughly four frames. At one read
 * every five seconds that is four frames in a hundred and fifty thousand.
 *
 * CTRL is read back rather than rewritten from scratch: enable and
 * ext_mode are both live by the time the main loop calls this. */
#define ACQ_HOLD_SPIN_MAX 100000u

static int acq_read_frame(uint16_t *out, uint32_t *frame_index)
{
    uint32_t ctrl  = argus_acq_rd(ARGUS_ACQ_CTRL);
    uint32_t spins = 0;

    argus_acq_wr(ARGUS_ACQ_CTRL, ctrl | ARGUS_ACQ_CTRL_HOLD);

    while ((argus_acq_rd(ARGUS_ACQ_STATUS) & ARGUS_ACQ_STATUS_HELD) == 0u) {
        if (++spins > ACQ_HOLD_SPIN_MAX) {
            argus_acq_wr(ARGUS_ACQ_CTRL, ctrl);
            return -2;
        }
    }

    *frame_index = argus_acq_rd(ARGUS_ACQ_FRAME_INDEX);

    for (int n = 0; n < ARGUS_MAX_CHANNELS; n++) {
        out[n] = (uint16_t)argus_acq_rd(ARGUS_ACQ_FRAME_BASE + 4u * (uint32_t)n);
    }

    argus_acq_wr(ARGUS_ACQ_CTRL, ctrl);
    return 0;
}

/* One bin of features under a hold, the same shape as acq_read_frame():
 * set CTRL.feat_hold, poll STATUS.feat_held, read FEATURE_INDEX and the
 * 192 words, release. Counts come out as they are; the 48-bit sum of
 * squares is divided by the bin length so power is the mean square in
 * code^2 and fits 32 bits. 192 reads at ~1 us each is 0.2 ms against a
 * 50 ms bin, and a bin that completes under the hold is deferred rather
 * than lost, so this never costs data. */
static int acq_read_features(uint16_t *counts, uint32_t *power, uint32_t *bin_index)
{
    uint32_t ctrl  = argus_acq_rd(ARGUS_ACQ_CTRL);
    uint32_t spins = 0;

    argus_acq_wr(ARGUS_ACQ_CTRL, ctrl | ARGUS_ACQ_CTRL_FEAT_HOLD);

    while ((argus_acq_rd(ARGUS_ACQ_STATUS) & ARGUS_ACQ_STATUS_FEAT_HELD) == 0u) {
        if (++spins > ACQ_HOLD_SPIN_MAX) {
            argus_acq_wr(ARGUS_ACQ_CTRL, ctrl);
            return -2;
        }
    }

    *bin_index = argus_acq_rd(ARGUS_ACQ_FEATURE_INDEX);

    for (int n = 0; n < ARGUS_MAX_CHANNELS; n++) {
        uint32_t lo = argus_acq_rd(ARGUS_ACQ_FEATURE_LO(n));
        uint32_t hi = argus_acq_rd(ARGUS_ACQ_FEATURE_HI(n));
        counts[n] = ARGUS_ACQ_FEAT_COUNT(hi);
        power[n]  = (uint32_t)(ARGUS_ACQ_FEAT_SUM(hi, lo) / ARGUS_ACQ_BIN_LEN);
    }

    argus_acq_wr(ARGUS_ACQ_CTRL, ctrl);
    return 0;
}

/* How long a frame read costs, for the record.
 *
 * Kept after the hold went in because it is the number that justifies the
 * hold existing, and because a regression here -- a slower interconnect
 * after a block design change, say -- is otherwise invisible now that the
 * read cannot fail.
 *
 * Two figures because they isolate different causes. A FRAME word comes
 * from the assembler's registered read port and a FRAME_INDEX word does
 * not, so the difference between them is that port; the absolute size of
 * either is the GP0 round trip plus whatever the -O0 loop costs on top. */
static void acq_timing_report(void)
{
    XTime t0, t1;
    volatile uint32_t sink = 0;

    XTime_GetTime(&t0);
    for (int n = 0; n < ARGUS_MAX_CHANNELS; n++) {
        sink += argus_acq_rd(ARGUS_ACQ_FRAME_INDEX);
    }
    XTime_GetTime(&t1);
    uint32_t reg_ns = (uint32_t)(((t1 - t0) * 1000u)
                     / (ARGUS_TICKS_PER_US * ARGUS_MAX_CHANNELS));

    XTime_GetTime(&t0);
    for (int n = 0; n < ARGUS_MAX_CHANNELS; n++) {
        sink += argus_acq_rd(ARGUS_ACQ_FRAME_BASE + 4u * (uint32_t)n);
    }
    XTime_GetTime(&t1);
    uint32_t frame_us = (uint32_t)((t1 - t0) / ARGUS_TICKS_PER_US);

    (void)sink;

    xil_printf("acq timing: reg %u ns/read, 96-word frame read %u us,"
               " sweep 33 us (read is held)\r\n",
               (unsigned)reg_ns, (unsigned)frame_us);
}

/* Every word must carry its own chip and channel, and the whole frame must
 * share one sample index. That holds for both sources: the chips' built-in
 * IDENT pattern and the relay's synthetic one use the same layout.
 *
 * With the built-in source the index is the chips' sweep counter, which
 * must agree with the assembler's frame counter modulo 256. Frames dropped
 * during a hold do not break that: FRAME_INDEX carries the sweep number of
 * the frame in the bank, not a count of frames published. With the
 * external source the index is the dataset sample number and the
 * cross-check does not apply. */
static void acq_frame_check(int ext)
{
    uint16_t frame[ARGUS_MAX_CHANNELS];
    uint32_t fi;

    if (acq_read_frame(frame, &fi) != 0) {
        argus_console_printf("acq frame: hold never took effect\r\n");
        return;
    }

    uint8_t idx = (uint8_t)(frame[0] & 0xFFu);
    int bad = 0;
    for (int n = 0; n < ARGUS_MAX_CHANNELS; n++) {
        uint16_t want = (uint16_t)(((n / ARGUS_ACQ_CH_PER_CHIP) << 14)
                                 | ((n % ARGUS_ACQ_CH_PER_CHIP) << 8)
                                 | idx);
        if (frame[n] != want) {
            bad++;
        }
    }

    if (ext) {
        argus_console_printf("acq frame %u: [0]=%04x [%d]=%04x idx=%02x (ext) bad=%d\r\n",
                             (unsigned)fi, frame[0],
                             ARGUS_MAX_CHANNELS - 1, frame[ARGUS_MAX_CHANNELS - 1],
                             idx, bad);
    } else {
        argus_console_printf("acq frame %u: [0]=%04x [%d]=%04x idx=%02x expect=%02x bad=%d\r\n",
                             (unsigned)fi, frame[0],
                             ARGUS_MAX_CHANNELS - 1, frame[ARGUS_MAX_CHANNELS - 1],
                             idx, (unsigned)((fi - 1u) & 0xFFu), bad);
    }
}

/* Runs before the network stack on purpose: an AXI read against
 * unprogrammed PL hangs the A9 with no timeout, and if that happens it
 * should be the first thing after the banner, not buried under lwIP output.
 * Three lines prove the 125 MHz clock, reset release, the init sequence,
 * the SPI sweep, the assembler, and the AXI path.
 *
 * The ID is a fabric revision. A mismatch almost always means the bitstream
 * predates the firmware -- RTL rebuilt, Vivado not run, or the XSA not
 * re-read into the platform -- and every later check would then be poking
 * registers the fabric does not have. Say so here, once, on the first
 * line, where it cannot be mistaken for anything else. */
static void acq_smoke_test(void)
{
    uint32_t id = argus_acq_rd(ARGUS_ACQ_ID);

    if (id == ARGUS_ACQ_ID_EXPECT) {
        xil_printf("acq id=%08x\r\n", (unsigned)id);
    } else {
        xil_printf("acq id=%08x  EXPECTED %08x -- stale bitstream?"
                   " Rebuild in Vivado, re-read the XSA, rebuild the platform.\r\n",
                   (unsigned)id, (unsigned)ARGUS_ACQ_ID_EXPECT);
    }

    argus_acq_wr(ARGUS_ACQ_CTRL, ARGUS_ACQ_CTRL_ENABLE);
    usleep(50000);

    /* Measured with hold clear, so this is the true sweep rate. */
    uint32_t f0 = argus_acq_rd(ARGUS_ACQ_FRAME_INDEX);
    usleep(1000000);
    uint32_t f1 = argus_acq_rd(ARGUS_ACQ_FRAME_INDEX);

    xil_printf("acq status=%08x frames/s=%u\r\n",
               (unsigned)argus_acq_rd(ARGUS_ACQ_STATUS),
               (unsigned)(f1 - f0));

    acq_timing_report();
    acq_frame_check(0);
    argus_console_flush();

    /* The feature bank: one bin is 50 ms, so wait for the first swap and
     * read it. Counts are zero until the 1.09 s warm-up has passed; power
     * is live from bin 0. */
    {
        uint16_t counts[ARGUS_MAX_CHANNELS];
        uint32_t power[ARGUS_MAX_CHANNELS];
        uint32_t bin;

        for (int i = 0; i < 100 && argus_acq_rd(ARGUS_ACQ_FEATURE_INDEX) == 0u; i++) {
            usleep(1000);
        }

        if (acq_read_features(counts, power, &bin) != 0) {
            xil_printf("acq features: hold never took effect\r\n");
        } else {
            xil_printf("acq features: bin %u  ch0 count %u power %u  ch14 count %u power %u"
                       "  dropped %u\r\n",
                       (unsigned)bin, counts[0], (unsigned)power[0],
                       counts[14], (unsigned)power[14],
                       (unsigned)argus_acq_rd(ARGUS_ACQ_FEAT_DROPPED));
        }
    }
}

/* --- main ----------------------------------------------------------------- */

int main(void)
{
    ip_addr_t ipaddr, netmask, gw;
    uint16_t counts[ARGUS_MAX_CHANNELS];
    uint32_t power[ARGUS_MAX_CHANNELS];
    uint32_t bin;
    uint32_t last_bin   = 0;
    uint32_t tx_count   = 0;
    uint32_t tx_skipped = 0;
    int attempt;

    /* Caches on. The D-cache was off through bring-up. With it on, the
     * replay BRAM aperture can be mapped write-back so a half's 28 KB lands
     * in cache and one flush writes it to the fabric in 32-byte bursts
     * (argus_replay.c); the rest of the firmware runs several times faster
     * as well. lwIP's Xilinx port maintains the cache over the GEM's DMA
     * buffers itself. The acq registers are Device memory and unaffected. */
    Xil_DCacheEnable();

    xil_printf("Initializing Argus Safety Controller...\r\n");

    acq_smoke_test();

    IP4_ADDR(&ipaddr,  192, 168, 1, 10);
    IP4_ADDR(&netmask, 255, 255, 255, 0);
    IP4_ADDR(&gw,      192, 168, 1, 1);

    lwip_init();

    if (!xemac_add(&server_netif, &ipaddr, &netmask, &gw,
                   mac_ethernet_address, XPAR_XEMACPS_0_BASEADDR)) {
        xil_printf("ERROR: xemac_add failed\r\n");
        return -1;
    }
    netif_set_default(&server_netif);
    netif_set_up(&server_netif);

    if (argus_net_init() != 0) {
        xil_printf("ERROR: argus_net_init failed\r\n");
        return -1;
    }

    if (argus_replay_init() != 0) {
        xil_printf("ERROR: argus_replay_init failed\r\n");
        return -1;
    }

    /* One-shot fetch into DDR: proves the host->PS path before anything
     * depends on it. */
    for (attempt = 0; attempt < REPLAY_SEND_ATTEMPTS; attempt++) {
        if (argus_replay_start_fetch(0, g_fetch_buf) == 0) {
            break;
        }
        xemacif_input(&server_netif);
        sys_check_timeouts();
        usleep(REPLAY_SEND_GAP_US);
    }

    if (attempt >= REPLAY_SEND_ATTEMPTS) {
        xil_printf("ERROR: replay fetch request never left the board\r\n");
    } else {
        while (!argus_replay_is_done()) {
            xemacif_input(&server_netif);
            sys_check_timeouts();
            argus_replay_service();
        }
        if (argus_replay_succeeded()) {
            xil_printf("replay ok: [0][0]=%04x [0][5]=%04x [1][0]=%04x [146][95]=%04x\r\n",
                g_fetch_buf[0], g_fetch_buf[5],
                g_fetch_buf[ARGUS_MAX_CHANNELS],
                g_fetch_buf[146 * ARGUS_MAX_CHANNELS + 95]);
        } else {
            xil_printf("replay FAILED\r\n");
        }
        argus_replay_report();
    }

    /* Streaming: prime both BRAM halves, switch the chips to the external
     * source, then keep the PL fed from the main loop. */
    argus_replay_stream_begin(0);
    xil_printf("stream: priming\r\n");

    xil_printf("Argus Safety Controller initialized. IP 192.168.1.10\r\n");

    /* Telemetry is now the codec's output: one NeuralFrame per 50 ms bin,
     * carrying each channel's threshold-crossing count -- the format
     * inference_node was trained on, so the decoder consumes it as is.
     * Spike-band power is read alongside and shown on the status line;
     * it goes on the wire when NeuralFrame gains a power field.
     *
     * Paced by the fabric, not the clock: FEATURE_INDEX advances once per
     * bin and the loop reads whenever it has. sample on the wire is the
     * bin number and t is the bin's time at the sweep rate. */
    while (1) {
        argus_console_service();
        argus_replay_note_loop(xemacif_input(&server_netif));   /* ARP too */
        sys_check_timeouts();
        argus_replay_stream_service();

        if (argus_acq_rd(ARGUS_ACQ_FEATURE_INDEX) == last_bin) {
            continue;
        }

        /* A hold that never took effect is a fabric problem, not a reason
         * to publish a torn bin. Skip, count, and let the status line show it. */
        if (acq_read_features(counts, power, &bin) != 0) {
            tx_skipped++;
            last_bin = argus_acq_rd(ARGUS_ACQ_FEATURE_INDEX);
            continue;
        }
        last_bin = bin;

        argus_send_frame(bin, (float)bin * ((float)ARGUS_ACQ_BIN_LEN / ARGUS_ACQ_SWEEP_HZ),
                         counts, power);
        tx_count++;

        if ((tx_count % 20) == 0) {
            argus_console_printf("tx %u bin %u skipped %u\r\n",
                                 (unsigned)tx_count, (unsigned)bin, (unsigned)tx_skipped);
        }
        if ((tx_count % STATUS_EVERY_BINS) == 0) {
            argus_replay_stream_report();
            argus_console_printf("feat: bin %u dropped %u  ch14 count %u power %u  ch75 count %u power %u\r\n",
                                 (unsigned)bin, (unsigned)argus_acq_rd(ARGUS_ACQ_FEAT_DROPPED),
                                 counts[14], (unsigned)power[14], counts[75], (unsigned)power[75]);
            acq_frame_check(argus_acq_rd(ARGUS_ACQ_CTRL) & ARGUS_ACQ_CTRL_EXT_MODE);
        }
    }
    return 0;
}
