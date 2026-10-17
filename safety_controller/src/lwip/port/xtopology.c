/*
 *  Board topology table for the lwIP Xilinx port.
 *  
 *  TODO: document this file
 */
#include "netif/xtopology.h"
#include "xparameters.h"
#include "lwip/arch.h"
#include "xiltimer.h"

struct xtopology_t xtopology[] = {
    {
        .emac_baseaddr = XPAR_XEMACPS_0_BASEADDR,
        .emac_type     = xemac_type_emacps,
    },
};

int xtopology_n_emacs = 1;

/* Zynq-7020 A9 global timer runs at CPU_3x2x / 2 = 333.333 MHz.
   COUNTS_PER_SECOND is absent from xiltimer.h in 2026.1. */
#define ARGUS_TIMER_HZ 333333333u

u32_t sys_now(void)
{
    XTime t;
    XTime_GetTime(&t);
    return (u32_t)(t / (ARGUS_TIMER_HZ / 1000u));
}