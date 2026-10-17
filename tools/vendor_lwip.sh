#!/usr/bin/env bash
# Vendors lwIP 2.2.0 + Xilinx port layer into safety_controller/src/lwip/.
#
# Why vendored: Vitis 2026.1's BSP indexer does not register lwip220 as a
# library for ps7_cortexa9, despite lwip220_v1_4 shipping a complete
# descriptor set that lists ps7_cortexa9 in supported_processors. The
# library never appears in .repo.yaml, so it cannot be added to the domain.
# Compiling the sources directly into the application sidesteps this.
#
# Source: AMD Vitis 2026.1, lwip220_v1_4.
set -euo pipefail

LW="${LW:-/tools/Xilinx/2026.1/Vitis/data/embeddedsw/ThirdParty/sw_services/lwip220_v1_4/src/lwip-2.2.0}"
DST="$(cd "$(dirname "$0")/.." && pwd)/safety_controller/src/lwip"

[ -d "$LW" ] || { echo "lwIP source not found: $LW" >&2; exit 1; }

rm -rf "$DST"
mkdir -p "$DST"/{src/core/ipv4,src/netif,src/include,port/netif,port/include}

CORE="init.c def.c inet_chksum.c ip.c mem.c memp.c netif.c pbuf.c raw.c stats.c sys.c timeouts.c udp.c"
for f in $CORE; do cp "$LW/src/core/$f" "$DST/src/core/"; done

CORE4="etharp.c icmp.c ip4.c ip4_addr.c ip4_frag.c"
for f in $CORE4; do cp "$LW/src/core/ipv4/$f" "$DST/src/core/ipv4/"; done

cp "$LW/src/netif/ethernet.c" "$DST/src/netif/"
cp -r "$LW/src/include/." "$DST/src/include/"

PORT="xadapter.c xpqueue.c xemacpsif.c xemacpsif_dma.c xemacpsif_hw.c xemacpsif_physpeed.c xemacpsif_hw.h xemac_ieee_reg.h"
for f in $PORT; do cp "$LW/contrib/ports/xilinx/netif/$f" "$DST/port/netif/"; done

cp "$LW/contrib/ports/xilinx/sys_arch_raw.c" "$DST/port/"
cp -r "$LW/contrib/ports/xilinx/include/arch"  "$DST/port/include/"
cp -r "$LW/contrib/ports/xilinx/include/netif" "$DST/port/include/"

echo "Vendored to $DST"
echo -n "  .c files: "; find "$DST" -name '*.c' | wc -l
