//! Wifi register offsets in the 0x04808000 page, and their power-on values
//! (GBATEK, "DS Wifi I/O Map").

pub(super) const W_ID: u32 = 0x000;
pub(super) const W_MODE_RST: u32 = 0x004;
pub(super) const W_MODE_WEP: u32 = 0x006;
pub(super) const W_TXSTATCNT: u32 = 0x008;
pub(super) const W_X_00A: u32 = 0x00A;
pub(super) const W_IF: u32 = 0x010;
pub(super) const W_IE: u32 = 0x012;
pub(super) const W_MACADDR: u32 = 0x018;
pub(super) const W_BSSID: u32 = 0x020;
pub(super) const W_AID_LOW: u32 = 0x028;
pub(super) const W_AID_FULL: u32 = 0x02A;
pub(super) const W_TX_RETRYLIMIT: u32 = 0x02C;
pub(super) const W_INTERNAL_02E: u32 = 0x02E;
pub(super) const W_RXCNT: u32 = 0x030;
pub(super) const W_WEP_CNT: u32 = 0x032;
pub(super) const W_INTERNAL_034: u32 = 0x034;
pub(super) const W_POWER_US: u32 = 0x036;
pub(super) const W_POWER_TX: u32 = 0x038;
pub(super) const W_POWERSTATE: u32 = 0x03C;
pub(super) const W_POWERFORCE: u32 = 0x040;
pub(super) const W_RANDOM: u32 = 0x044;
pub(super) const W_POWER_UNKNOWN: u32 = 0x048;
pub(super) const W_RXBUF_BEGIN: u32 = 0x050;
pub(super) const W_RXBUF_END: u32 = 0x052;
pub(super) const W_RXBUF_WRCSR: u32 = 0x054;
pub(super) const W_RXBUF_WR_ADDR: u32 = 0x056;
pub(super) const W_RXBUF_RD_ADDR: u32 = 0x058;
pub(super) const W_RXBUF_READCSR: u32 = 0x05A;
pub(super) const W_RXBUF_COUNT: u32 = 0x05C;
pub(super) const W_RXBUF_RD_DATA: u32 = 0x060;
pub(super) const W_RXBUF_GAP: u32 = 0x062;
pub(super) const W_RXBUF_GAPDISP: u32 = 0x064;
pub(super) const W_TXBUF_WR_ADDR: u32 = 0x068;
pub(super) const W_TXBUF_COUNT: u32 = 0x06C;
pub(super) const W_TXBUF_WR_DATA: u32 = 0x070;
pub(super) const W_TXBUF_GAP: u32 = 0x074;
pub(super) const W_TXBUF_GAPDISP: u32 = 0x076;
pub(super) const W_TXBUF_BEACON: u32 = 0x080;
pub(super) const W_TXBUF_TIM: u32 = 0x084;
pub(super) const W_LISTENCOUNT: u32 = 0x088;
pub(super) const W_BEACONINT: u32 = 0x08C;
pub(super) const W_LISTENINT: u32 = 0x08E;
pub(super) const W_TXBUF_CMD: u32 = 0x090;
pub(super) const W_TXBUF_REPLY1: u32 = 0x094;
pub(super) const W_TXBUF_REPLY2: u32 = 0x098;
pub(super) const W_INTERNAL_09C: u32 = 0x09C;
pub(super) const W_TXBUF_LOC1: u32 = 0x0A0;
pub(super) const W_TXBUF_LOC2: u32 = 0x0A4;
pub(super) const W_TXBUF_LOC3: u32 = 0x0A8;
pub(super) const W_TXREQ_RESET: u32 = 0x0AC;
pub(super) const W_TXREQ_SET: u32 = 0x0AE;
pub(super) const W_TXREQ_READ: u32 = 0x0B0;
pub(super) const W_TXBUF_RESET: u32 = 0x0B4;
pub(super) const W_TXBUSY: u32 = 0x0B6;
pub(super) const W_TXSTAT: u32 = 0x0B8;
pub(super) const W_PREAMBLE: u32 = 0x0BC;
pub(super) const W_CMD_TOTALTIME: u32 = 0x0C0;
pub(super) const W_CMD_REPLYTIME: u32 = 0x0C4;
pub(super) const W_RXFILTER: u32 = 0x0D0;
pub(super) const W_CONFIG_0D4: u32 = 0x0D4;
pub(super) const W_CONFIG_0D8: u32 = 0x0D8;
pub(super) const W_RX_LEN_CROP: u32 = 0x0DA;
pub(super) const W_RXFILTER2: u32 = 0x0E0;
pub(super) const W_US_COUNTCNT: u32 = 0x0E8;
pub(super) const W_US_COMPARECNT: u32 = 0x0EA;
pub(super) const W_CONFIG_0EC: u32 = 0x0EC;
pub(super) const W_CMD_COUNTCNT: u32 = 0x0EE;
pub(super) const W_US_COMPARE: u32 = 0x0F0;
pub(super) const W_US_COUNT: u32 = 0x0F8;
pub(super) const W_CONTENTFREE: u32 = 0x10C;
pub(super) const W_PRE_BEACON: u32 = 0x110;
pub(super) const W_CMD_COUNT: u32 = 0x118;
pub(super) const W_BEACON_COUNT: u32 = 0x11C;
pub(super) const W_POST_BEACON: u32 = 0x134;
pub(super) const W_BB_CNT: u32 = 0x158;
pub(super) const W_BB_WRITE: u32 = 0x15A;
pub(super) const W_BB_READ: u32 = 0x15C;
pub(super) const W_BB_BUSY: u32 = 0x15E;
pub(super) const W_BB_MODE: u32 = 0x160;
pub(super) const W_BB_POWER: u32 = 0x168;
pub(super) const W_RF_DATA2: u32 = 0x17C;
pub(super) const W_RF_DATA1: u32 = 0x17E;
pub(super) const W_RF_BUSY: u32 = 0x180;
pub(super) const W_RF_CNT: u32 = 0x184;
pub(super) const W_TX_HDR_CNT: u32 = 0x194;
pub(super) const W_INTERNAL_198: u32 = 0x198;
pub(super) const W_RF_PINS: u32 = 0x19C;
pub(super) const W_X_1A2: u32 = 0x1A2;
pub(super) const W_X_1A4: u32 = 0x1A4;
pub(super) const W_RXSTAT_INC_IF: u32 = 0x1A8;
pub(super) const W_RXSTAT_OVF_IF: u32 = 0x1AC;
pub(super) const W_RXSTAT: u32 = 0x1B0;
pub(super) const W_RXSTAT_END: u32 = 0x1BE;
pub(super) const W_RX_COUNT: u32 = 0x1C4;
pub(super) const W_CMD_STAT: u32 = 0x1D0;
pub(super) const W_CMD_STAT_END: u32 = 0x1DE;
pub(super) const W_TX_SEQNO: u32 = 0x210;
pub(super) const W_RF_STATUS: u32 = 0x214;
pub(super) const W_IF_SET: u32 = 0x21C;
pub(super) const W_INTERNAL_224: u32 = 0x224;
pub(super) const W_INTERNAL_230: u32 = 0x230;
pub(super) const W_RXTX_ADDR: u32 = 0x268;
pub(super) const W_INTERNAL_278: u32 = 0x278;
pub(super) const W_INTERNAL_27C: u32 = 0x27C;

/// Registers whose power-on value is not zero, from GBATEK's "[Init]"
/// column. W_ID depends on the console and is set separately.
pub(super) const POWER_ON: [(u32, u16); 37] = [
    (W_TX_RETRYLIMIT, 0x0707),
    (W_POWER_US, 0x0001),
    (W_POWER_TX, 0x0003),
    (W_POWERSTATE, 0x0200),
    (W_RXBUF_BEGIN, 0x4000),
    (W_RXBUF_END, 0x4800),
    (W_BEACONINT, 0x0064),
    (W_INTERNAL_09C, 0x0050),
    (W_TXREQ_READ, 0x0010),
    (W_PREAMBLE, 0x0001),
    (W_RXFILTER, 0x0401),
    (W_CONFIG_0D4, 0x0001),
    (W_CONFIG_0D8, 0x0004),
    (W_RX_LEN_CROP, 0x0602),
    (W_RXFILTER2, 0x0008),
    (W_CONFIG_0EC, 0x3F03),
    (W_CMD_COUNTCNT, 0x0001),
    (0x120, 0x0048),
    (0x122, 0x4840),
    (0x130, 0x0142),
    (0x132, 0x8064),
    (W_POST_BEACON, 0xFFFF),
    (0x142, 0x2443),
    (0x144, 0x0042),
    (0x146, 0x0016),
    (0x148, 0x0016),
    (0x14A, 0x0016),
    (0x14C, 0x162C),
    (0x150, 0x0204),
    (0x154, 0x0058),
    (W_BB_MODE, 0x0100),
    (W_BB_POWER, 0x800D),
    (W_RF_DATA2, 0x0800),
    (W_RF_DATA1, 0xC008),
    (W_RF_CNT, 0x0018),
    (W_RF_PINS, 0x0004),
    (W_X_1A2, 0x0001),
];

/// Registers the CPU cannot write.
pub(super) const READ_ONLY: [u32; 16] = [
    W_ID,
    W_RANDOM,
    W_RXBUF_WRCSR,
    W_TXREQ_READ,
    W_TXBUSY,
    W_TXSTAT,
    W_BB_READ,
    W_BB_BUSY,
    W_RF_BUSY,
    W_RF_PINS,
    W_RXSTAT_INC_IF,
    W_RXSTAT_OVF_IF,
    W_RX_COUNT,
    W_TX_SEQNO,
    W_RF_STATUS,
    W_RXTX_ADDR,
];
