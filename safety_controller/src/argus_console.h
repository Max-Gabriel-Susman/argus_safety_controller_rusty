/**
 * @file argus_console.h
 * @brief Non-blocking console output for the main loop.
 *
 * xil_printf waits on the UART: at 115200 8N1 a character takes about
 * 87 us, and the main loop has about 2.1 ms of slack per replay half, so
 * any line longer than about 24 characters costs the stream an underrun.
 * Here a message is formatted into a RAM ring at once and
 * argus_console_service() moves bytes into the 64-byte transmit FIFO only
 * while it has room, so no call ever waits on the wire.
 *
 * @warning Main-context only: none of these functions is safe from an
 *          interrupt.
 */

#ifndef ARGUS_CONSOLE_H
#define ARGUS_CONSOLE_H

#include <stdint.h>

/** Size of the RAM ring in bytes. Must be a power of two, so indices wrap
 *  with a mask; argus_console.c refuses to compile otherwise. */
#define ARGUS_CONSOLE_RING_BYTES 4096u

/** Size of the formatting buffer. A message of this many characters or
 *  more is dropped and counted, so the longest that can be queued is one
 *  less. */
#define ARGUS_CONSOLE_MSG_MAX 320u

/**
 * Formats into the ring.
 *
 * If the whole message does not fit, none of it is queued and the drop
 * counter (argus_console_drops()) goes up.
 *
 * @param fmt  printf-style format string.
 * @param ...  Arguments for @p fmt.
 */
void argus_console_printf(const char *fmt, ...);

/**
 * Writes queued bytes to the UART while its transmit FIFO has room.
 *
 * Call once per main-loop pass.
 */
void argus_console_service(void);

/**
 * Blocks until the ring is empty.
 *
 * For use before the main loop only, to keep buffered lines in order with
 * xil_printf output that follows.
 */
void argus_console_flush(void);

/**
 * Messages dropped because they did not fit, since boot.
 *
 * A message is dropped whole, either because the ring lacks room for it or
 * because it reaches #ARGUS_CONSOLE_MSG_MAX characters.
 *
 * @return Number of messages dropped since boot.
 */
uint32_t argus_console_drops(void);

/**
 * Longest message queued since the last call, in bytes; resets it.
 *
 * @return Length of the longest message queued since the previous call,
 *         or 0 if none was.
 */
uint32_t argus_console_take_max_msg(void);

#endif /* ARGUS_CONSOLE_H */
