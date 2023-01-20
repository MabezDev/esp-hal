ets_printf = 0x40000028;
PROVIDE(esp_rom_printf = ets_printf);
PROVIDE(cache_invalidate_icache_all = 0x4000064c);
PROVIDE(cache_suspend_icache = 0x40000698);
PROVIDE(cache_resume_icache = 0x4000069c);
/* TODO PROVIDE(cache_ibus_mmu_set = 0x40000560); */
/* TODO PROVIDE(cache_dbus_mmu_set = 0x40000564); */
PROVIDE(ets_delay_us = 0x40000040);
PROVIDE(ets_update_cpu_frequency_rom = 0x40000048);
PROVIDE(rom_i2c_writeReg = 0x400012c0);
PROVIDE(rom_i2c_writeReg_Mask = 0x400012e8);
PROVIDE(rtc_get_reset_reason = 0x40000018);
