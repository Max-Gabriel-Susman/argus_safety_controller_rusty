
#ifndef ARGUS_NET_H
    #define ARGUS_NET_H

    #include <stdint.h>
    int argus_net_init(void);

    int argus_send_frame(uint32_t sample, float t, const uint16_t *ch,
                         const uint32_t *power);

#endif 