/**
 * @file argus_console.c
 * @brief Ring buffer behind argus_console.h, which documents the API.
 */

#include <stdarg.h>
#include <stdio.h>
#include <stdint.h>

#include "bspconfig.h"       /* STDOUT_BASEADDRESS */
#include "xuartps_hw.h"

#include "argus_console.h"

/* Power of two, so indices wrap with a mask. */
#if (ARGUS_CONSOLE_RING_BYTES & (ARGUS_CONSOLE_RING_BYTES - 1u)) != 0
#error "ARGUS_CONSOLE_RING_BYTES must be a power of two"
#endif

static char     g_ring[ARGUS_CONSOLE_RING_BYTES]; /**< bytes queued for the UART */
static uint32_t g_head;     /**< next byte written; free-running */
static uint32_t g_tail;     /**< next byte sent;    free-running */
static uint32_t g_drops;    /**< messages dropped since boot */
static uint32_t g_max_msg;  /**< longest message since the last take */

void argus_console_printf(const char *fmt, ...)
{
    char msg[ARGUS_CONSOLE_MSG_MAX];
    va_list ap;
    int n;

    va_start(ap, fmt);
    n = vsnprintf(msg, sizeof msg, fmt, ap);
    va_end(ap);

    if (n < 0 || (uint32_t)n >= sizeof msg
        || (uint32_t)n > ARGUS_CONSOLE_RING_BYTES - (g_head - g_tail)) {
        g_drops++;
        return;
    }

    for (int i = 0; i < n; i++) {
        g_ring[(g_head + (uint32_t)i) & (ARGUS_CONSOLE_RING_BYTES - 1u)] = msg[i];
    }
    g_head += (uint32_t)n;

    if ((uint32_t)n > g_max_msg) {
        g_max_msg = (uint32_t)n;
    }
}

void argus_console_service(void)
{
    while (g_tail != g_head && !XUartPs_IsTransmitFull(STDOUT_BASEADDRESS)) {
        XUartPs_WriteReg(STDOUT_BASEADDRESS, XUARTPS_FIFO_OFFSET,
                         (uint8_t)g_ring[g_tail & (ARGUS_CONSOLE_RING_BYTES - 1u)]);
        g_tail++;
    }
}

void argus_console_flush(void)
{
    while (g_tail != g_head) {
        argus_console_service();
    }
}

uint32_t argus_console_drops(void)
{
    return g_drops;
}

uint32_t argus_console_take_max_msg(void)
{
    uint32_t m = g_max_msg;

    g_max_msg = 0;
    return m;
}
