use paste::paste;
use strum::FromRepr;

use crate::{
    clock::{
        clocks_ll::{regi2c_write, regi2c_write_mask},
        XtalClock,
    },
    peripherals::{EXTMEM, LP_AON, PCR, PMU, SPI0, SPI1},
    rtc_cntl::{RtcCalSel, RtcClock, RtcFastClock, RtcSlowClock},
};

const I2C_DIG_REG: u8 = 0x6d;
const I2C_DIG_REG_HOSTID: u8 = 0;

const I2C_ULP: u8 = 0x61;
const I2C_ULP_HOSTID: u8 = 0;

const I2C_DIG_REG_XPD_RTC_REG: u8 = 13;
const I2C_DIG_REG_XPD_RTC_REG_MSB: u8 = 2;
const I2C_DIG_REG_XPD_RTC_REG_LSB: u8 = 2;

const I2C_DIG_REG_XPD_DIG_REG: u8 = 13;
const I2C_DIG_REG_XPD_DIG_REG_MSB: u8 = 3;
const I2C_DIG_REG_XPD_DIG_REG_LSB: u8 = 3;

const I2C_ULP_IR_FORCE_XPD_CK: u8 = 0;
const I2C_ULP_IR_FORCE_XPD_CK_MSB: u8 = 2;
const I2C_ULP_IR_FORCE_XPD_CK_LSB: u8 = 2;

const I2C_DIG_REG_ENIF_RTC_DREG: u8 = 5;
const I2C_DIG_REG_ENIF_RTC_DREG_MSB: u8 = 7;
const I2C_DIG_REG_ENIF_RTC_DREG_LSB: u8 = 7;

const I2C_DIG_REG_ENIF_DIG_DREG: u8 = 7;
const I2C_DIG_REG_ENIF_DIG_DREG_MSB: u8 = 7;
const I2C_DIG_REG_ENIF_DIG_DREG_LSB: u8 = 7;

pub(crate) fn init() {
    let pmu = unsafe { &*PMU::ptr() };

    // SET_PERI_REG_MASK(PMU_RF_PWC_REG, PMU_PERIF_I2C_RSTB);
    // SET_PERI_REG_MASK(PMU_RF_PWC_REG, PMU_XPD_PERIF_I2C);
    //
    // REGI2C_WRITE_MASK(I2C_DIG_REG, I2C_DIG_REG_ENIF_RTC_DREG, 1);
    // REGI2C_WRITE_MASK(I2C_DIG_REG, I2C_DIG_REG_ENIF_DIG_DREG, 1);
    // REGI2C_WRITE_MASK(I2C_DIG_REG, I2C_DIG_REG_XPD_RTC_REG, 0);
    // REGI2C_WRITE_MASK(I2C_DIG_REG, I2C_DIG_REG_XPD_DIG_REG, 0);
    // REG_SET_FIELD(PMU_HP_ACTIVE_HP_REGULATOR0_REG,
    // PMU_HP_ACTIVE_HP_REGULATOR_DBIAS, 25);
    // REG_SET_FIELD(PMU_HP_SLEEP_LP_REGULATOR0_REG,
    // PMU_HP_SLEEP_LP_REGULATOR_DBIAS, 26);

    pmu.rf_pwc
        .modify(|_, w| w.perif_i2c_rstb().set_bit().xpd_perif_i2c().set_bit());

    unsafe {
        // crate::clock::clocks_ll::regi2c_write_mask(I2C_DIG_REG,
        // I2C_DIG_REG_ENIF_RTC_DREG, 1); use i2c macro from C6 clock
        regi2c_write_mask(
            I2C_DIG_REG,
            I2C_DIG_REG_HOSTID,
            I2C_DIG_REG_ENIF_RTC_DREG,
            I2C_DIG_REG_ENIF_RTC_DREG_MSB,
            I2C_DIG_REG_ENIF_RTC_DREG_LSB,
            1,
        );
        regi2c_write_mask(
            I2C_DIG_REG,
            I2C_DIG_REG_HOSTID,
            I2C_DIG_REG_ENIF_DIG_DREG,
            I2C_DIG_REG_ENIF_DIG_DREG_MSB,
            I2C_DIG_REG_ENIF_DIG_DREG_LSB,
            1,
        );

        regi2c_write_mask(
            I2C_DIG_REG,
            I2C_DIG_REG_HOSTID,
            I2C_DIG_REG_XPD_RTC_REG,
            I2C_DIG_REG_XPD_RTC_REG_MSB,
            I2C_DIG_REG_XPD_RTC_REG_LSB,
            0,
        );
        regi2c_write_mask(
            I2C_DIG_REG,
            I2C_DIG_REG_HOSTID,
            I2C_DIG_REG_XPD_DIG_REG,
            I2C_DIG_REG_XPD_DIG_REG_MSB,
            I2C_DIG_REG_XPD_DIG_REG_LSB,
            0,
        );

        pmu.hp_active_hp_regulator0
            .modify(|_, w| w.hp_active_hp_regulator_dbias().bits(25));
        pmu.hp_sleep_lp_regulator0
            .modify(|_, w| w.hp_sleep_lp_regulator_dbias().bits(26));
    }
}

pub(crate) fn configure_clock() {
    assert!(matches!(
        RtcClock::get_xtal_freq(),
        XtalClock::RtcXtalFreq40M
    ));

    // RtcClock::set_fast_freq(RtcFastClock::RtcFastClock8m);

    // let cal_val = loop {
    //     RtcClock::set_slow_freq(RtcSlowClock::RtcSlowClockRtc);

    //     let res = RtcClock::calibrate(RtcCalSel::RtcCalRtcMux, 1024);
    //     if res != 0 {
    //         break res;
    //     }
    // };

    // unsafe {
    //     let lp_aon = &*LP_AON::ptr();
    //     lp_aon.store1.write(|w| w.bits(cal_val));
    // }
}

fn calibrate_ocode() {}

fn set_rtc_dig_dbias() {}

/// Perform clock control related initialization
// fn clock_control_init() {
//     let extmem = unsafe { &*EXTMEM::ptr() };
//     let spi_mem_0 = unsafe { &*SPI0::ptr() };
//     let spi_mem_1 = unsafe { &*SPI1::ptr() };

//     // Clear CMMU clock force on
//     extmem
//         .cache_mmu_power_ctrl
//         .modify(|_, w| w.cache_mmu_mem_force_on().clear_bit());

//     // Clear tag clock force on
//     extmem
//         .icache_tag_power_ctrl
//         .modify(|_, w| w.icache_tag_mem_force_on().clear_bit());

//     // Clear register clock force on
//     spi_mem_0.clock_gate.modify(|_, w| w.clk_en().clear_bit());
//     spi_mem_1.clock_gate.modify(|_, w| w.clk_en().clear_bit());
// }

/// Perform power control related initialization
// fn power_control_init() {
//     let rtc_cntl = unsafe { &*RTC_CNTL::ptr() };
//     let pcr = unsafe { &*PCR::ptr() };
//     rtc_cntl
//         .clk_conf
//         .modify(|_, w| w.ck8m_force_pu().clear_bit());

//     // Cancel XTAL force PU if no need to force power up
//     // Cannot cancel XTAL force PU if PLL is force power on
//     rtc_cntl
//         .options0
//         .modify(|_, w| w.xtl_force_pu().clear_bit());

//     // Force PD APLL
//     rtc_cntl.ana_conf.modify(|_, w| {
//         w.plla_force_pu()
//             .clear_bit()
//             .plla_force_pd()
//             .set_bit()
//             // Open SAR_I2C protect function to avoid SAR_I2C
//             // Reset when rtc_ldo is low.
//             .reset_por_force_pd()
//             .clear_bit()
//     });

//     // Cancel BBPLL force PU if setting no force power up
//     rtc_cntl.options0.modify(|_, w| {
//         w.bbpll_force_pu()
//             .clear_bit()
//             .bbpll_i2c_force_pu()
//             .clear_bit()
//             .bb_i2c_force_pu()
//             .clear_bit()
//     });
//     rtc_cntl.rtc_cntl.modify(|_, w| {
//         w.regulator_force_pu()
//             .clear_bit()
//             .dboost_force_pu()
//             .clear_bit()
//             .dboost_force_pd()
//             .set_bit()
//     });

//     // If this mask is enabled, all soc memories cannot enter power down mode.
//     // We should control soc memory power down mode from RTC,
//     // so we will not touch this register any more.
//     pcr
//         .mem_pd_mask
//         .modify(|_, w| w.lslp_mem_pd_mask().clear_bit());

//     rtc_sleep_pu();

//     rtc_cntl.dig_pwc.modify(|_, w| {
//         w.dg_wrap_force_pu()
//             .clear_bit()
//             .wifi_force_pu()
//             .clear_bit()
//             .bt_force_pu()
//             .clear_bit()
//             .cpu_top_force_pu()
//             .clear_bit()
//             .dg_peri_force_pu()
//             .clear_bit()
//     });
//     rtc_cntl.dig_iso.modify(|_, w| {
//         w.dg_wrap_force_noiso()
//             .clear_bit()
//             .wifi_force_noiso()
//             .clear_bit()
//             .bt_force_noiso()
//             .clear_bit()
//             .cpu_top_force_noiso()
//             .clear_bit()
//             .dg_peri_force_noiso()
//             .clear_bit()
//     });

//     // Cancel digital PADS force no iso
//     system
//         .cpu_per_conf
//         .modify(|_, w| w.cpu_wait_mode_force_on().clear_bit());

//     // If SYSTEM_CPU_WAIT_MODE_FORCE_ON == 0,
//     // the CPU clock will be closed when CPU enter WAITI mode.
//     rtc_cntl.dig_iso.modify(|_, w| {
//         w.dg_pad_force_unhold()
//             .clear_bit()
//             .dg_pad_force_noiso()
//             .clear_bit()
//     });
// }

/// Configure whether certain peripherals are powered down in deep sleep
// fn rtc_sleep_pu() {
//     let rtc_cntl = unsafe { &*RTC_CNTL::ptr() };
//     let apb_ctrl = unsafe { &*APB_CTRL::ptr() };

//     rtc_cntl.dig_pwc.modify(|_, w| {
//         w.lslp_mem_force_pu()
//             .clear_bit()
//             .fastmem_force_lpu()
//             .clear_bit()
//     });

//     apb_ctrl.front_end_mem_pd.modify(|_, w| {
//         w.dc_mem_force_pu()
//             .clear_bit()
//             .pbus_mem_force_pu()
//             .clear_bit()
//             .agc_mem_force_pu()
//             .clear_bit()
//     });
//     apb_ctrl
//         .mem_power_up
//         .modify(|_, w| unsafe { w.sram_power_up().bits(0u8).rom_power_up().bits(0u8) });
// }

// Terminology:
//
// CPU Reset:    Reset CPU core only, once reset done, CPU will execute from
//               reset vector
// Core Reset:   Reset the whole digital system except RTC sub-system
// System Reset: Reset the whole digital system, including RTC sub-system
// Chip Reset:   Reset the whole chip, including the analog part

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRepr)]
pub enum SocResetReason {
    /// Power on reset
    ///
    /// In ESP-IDF this value (0x01) can *also* be `ChipBrownOut` or
    /// `ChipSuperWdt`, however that is not really compatible with Rust-style
    /// enums.
    ChipPowerOn   = 0x01,
    /// Software resets the digital core by RTC_CNTL_SW_SYS_RST
    CoreSw        = 0x03,
    /// Deep sleep reset the digital core
    CoreDeepSleep = 0x05,
    /// SDIO Core reset
    CoreSDIO      = 0x06,
    /// Main watch dog 0 resets digital core
    CoreMwdt0     = 0x07,
    /// Main watch dog 1 resets digital core
    CoreMwdt1     = 0x08,
    /// RTC watch dog resets digital core
    CoreRtcWdt    = 0x09,
    /// Main watch dog 0 resets CPU 0
    Cpu0Mwdt0     = 0x0B,
    /// Software resets CPU 0 by RTC_CNTL_SW_PROCPU_RST
    Cpu0Sw        = 0x0C,
    /// RTC watch dog resets CPU 0
    Cpu0RtcWdt    = 0x0D,
    /// VDD voltage is not stable and resets the digital core
    SysBrownOut   = 0x0F,
    /// RTC watch dog resets digital core and rtc module
    SysRtcWdt     = 0x10,
    /// Main watch dog 1 resets CPU 0
    Cpu0Mwdt1     = 0x11,
    /// Super watch dog resets the digital core and rtc module
    SysSuperWdt   = 0x12,
    /// eFuse CRC error resets the digital core
    CoreEfuseCrc  = 0x14,
    /// USB UART resets the digital core
    CoreUsbUart   = 0x15,
    /// USB JTAG resets the digital core
    CoreUsbJtag   = 0x16,
    /// JTAG resets CPU
    Cpu0JtagCpu   = 0x18,
}