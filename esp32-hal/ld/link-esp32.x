
/* before memory.x to allow override */
ENTRY(Reset)

INCLUDE memory.x

/* after memory.x to allow override */
PROVIDE(__pre_init = DefaultPreInit);
PROVIDE(__zero_bss = default_mem_hook);
PROVIDE(__init_data = default_mem_hook);

INCLUDE exception.x

/* map generic regions to output sections */
INCLUDE "alias.x"

INCLUDE "text.x"
INCLUDE "rodata.x"
INCLUDE "rwdata.x"
INCLUDE "rwtext.x"
INCLUDE "rtc_fast.x"
INCLUDE "rtc_slow.x"

_heap_end = ABSOLUTE(ORIGIN(dram_seg))+LENGTH(dram_seg)-LENGTH(reserved_for_boot_seg) - 2*STACK_SIZE;

_stack_start_cpu1 = _heap_end;
_stack_end_cpu1 = _stack_start_cpu1 + STACK_SIZE;
_stack_start_cpu0 = _stack_end_cpu1;
_stack_end_cpu0 = _stack_start_cpu0 + STACK_SIZE;

EXTERN(DefaultHandler);

EXTERN(WIFI_EVENT); /* Force inclusion of WiFi libraries */

INCLUDE "device.x"