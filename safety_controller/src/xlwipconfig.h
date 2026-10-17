#ifndef __XLWIPCONFIG_H_
#define __XLWIPCONFIG_H_

/* Only GEM is present on Zynq-7000. The lwIP port tests these with
   #ifdef, so unused controllers must be left undefined, not set to 0. */
#define XLWIP_CONFIG_INCLUDE_GEM        1
#define XLWIP_CONFIG_N_TX_DESC          64
#define XLWIP_CONFIG_N_RX_DESC          64

#endif
