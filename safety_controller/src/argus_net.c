#include <string.h>
#include <stddef.h>
#include <stdint.h>

#include "lwip/udp.h"
#include "lwip/init.h"
#include "netif/xadapter.h"

#include "argus_wire.h"
#include "argus_net.h"

static struct udp_pcb *g_pcb;
static ip_addr_t g_dest;

int argus_net_init(void)
{
    // Create a protocol control block; a handle for a udp endpoint.
    g_pcb = udp_new(); 
    if (!g_pcb) {
        return -1;
    }
    IP4_ADDR(&g_dest, 192, 168, 1, 20); /* Orin */
    return 0;
}

int argus_send_frame(uint32_t sample, float t, const uint16_t *ch,
                     const uint32_t *power)
{
    argus_frame_packet_t pkt = {0};
    pkt.magic = ARGUS_FRAME_MAGIC;
    pkt.sample = sample;
    pkt.t = t;
    pkt.version = ARGUS_FRAME_VERSION;
    pkt.channel_count = ARGUS_MAX_CHANNELS;
    memcpy(pkt.channels, ch, sizeof(pkt.channels));
    memcpy(pkt.power, power, sizeof(pkt.power)); /* v3: sum / BIN_LEN */
    pkt.crc = crc16_ccitt((const uint8_t *)&pkt, offsetof(argus_frame_packet_t, crc));
    
    struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, sizeof(pkt), PBUF_RAM);
    if (!p) {
        return -1;
    }
    memcpy(p->payload, &pkt, sizeof(pkt));
    
    err_t err = udp_sendto(g_pcb, p, &g_dest, ARGUS_UDP_PORT);
    pbuf_free(p);
    return (err == ERR_OK) ? 0 : -1;
}