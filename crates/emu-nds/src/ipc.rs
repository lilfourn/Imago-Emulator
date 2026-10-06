//! Inter-processor communication: IPCSYNC and the two 16-word FIFOs
//! (GBATEK, "DS Inter Process Communication (IPC)").
//!
//! Each CPU owns one side of the link. `Side::Arm9` is the ARM9's view:
//! its send FIFO is the ARM7's receive FIFO, and so on. Methods return the
//! interrupts they raise on each CPU so the caller can route them.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::interrupt::irq;

const FIFO_DEPTH: usize = 16;

/// Which CPU is accessing the registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Side {
    Arm9,
    Arm7,
}

impl Side {
    const fn index(self) -> usize {
        match self {
            Self::Arm9 => 0,
            Self::Arm7 => 1,
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Arm9 => Self::Arm7,
            Self::Arm7 => Self::Arm9,
        }
    }
}

/// Interrupts to raise: `[arm9, arm7]` IF bits.
pub(crate) type Raised = [u32; 2];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Port {
    /// IPCSYNC bits 8–11: the value this CPU shows the other.
    sync_out: u8,
    /// IPCSYNC bit 14: interrupt this CPU when the other sends with bit 13.
    sync_irq: bool,
    /// IPCFIFOCNT bits this CPU controls: 2 send-empty IRQ, 10 receive
    /// not-empty IRQ, 14 error, 15 enable.
    control: u16,
    /// Words this CPU has sent and the other has not read.
    send: VecDeque<u32>,
    /// The last word read, returned again when the FIFO is empty.
    last_received: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Ipc {
    ports: [Port; 2],
}

impl Ipc {
    fn port(&self, side: Side) -> &Port {
        &self.ports[side.index()]
    }

    fn port_mut(&mut self, side: Side) -> &mut Port {
        &mut self.ports[side.index()]
    }

    fn raise(side: Side, bits: u32) -> Raised {
        let mut raised = [0; 2];
        raised[side.index()] = bits;
        raised
    }

    /// IPCSYNC as `side` reads it.
    pub(crate) fn read_sync(&self, side: Side) -> u16 {
        let input = u16::from(self.port(side.other()).sync_out);
        let output = u16::from(self.port(side).sync_out);
        input | output << 8 | u16::from(self.port(side).sync_irq) << 14
    }

    pub(crate) fn write_sync(&mut self, side: Side, value: u16) -> Raised {
        let port = self.port_mut(side);
        port.sync_out = ((value >> 8) & 0xF) as u8;
        port.sync_irq = value & (1 << 14) != 0;
        let notify = value & (1 << 13) != 0 && self.port(side.other()).sync_irq;
        if notify {
            Self::raise(side.other(), irq::IPC_SYNC)
        } else {
            [0; 2]
        }
    }

    /// IPCFIFOCNT as `side` reads it.
    pub(crate) fn read_control(&self, side: Side) -> u16 {
        let port = self.port(side);
        let send = &port.send;
        let receive = &self.port(side.other()).send;
        let status = u16::from(send.is_empty())
            | u16::from(send.len() == FIFO_DEPTH) << 1
            | u16::from(receive.is_empty()) << 8
            | u16::from(receive.len() == FIFO_DEPTH) << 9;
        (port.control & 0xC404) | status
    }

    pub(crate) fn write_control(&mut self, side: Side, value: u16) -> Raised {
        let mut raised = [0; 2];
        let port = self.port_mut(side);
        let was_send_irq = port.control & (1 << 2) != 0;
        let was_receive_irq = port.control & (1 << 10) != 0;
        if value & (1 << 3) != 0 {
            port.send.clear();
        }
        // Bit 14 (error) is acknowledged by writing one.
        let error = port.control & (1 << 14) & !value;
        port.control = (value & 0x8404) | error;
        let send_empty = port.send.is_empty();
        let receive_waiting = !self.port(side.other()).send.is_empty();
        let port = self.port(side);
        // Enabling an interrupt whose condition already holds fires it.
        if !was_send_irq && port.control & (1 << 2) != 0 && send_empty {
            raised[side.index()] |= irq::IPC_SEND_EMPTY;
        }
        if !was_receive_irq && port.control & (1 << 10) != 0 && receive_waiting {
            raised[side.index()] |= irq::IPC_RECEIVE_NOT_EMPTY;
        }
        raised
    }

    /// `side` writes IPCFIFOSEND.
    pub(crate) fn send(&mut self, side: Side, value: u32) -> Raised {
        let port = self.port_mut(side);
        if port.control & 0x8000 == 0 {
            return [0; 2];
        }
        if port.send.len() == FIFO_DEPTH {
            port.control |= 1 << 14;
            return [0; 2];
        }
        let was_empty = port.send.is_empty();
        port.send.push_back(value);
        let other = self.port(side.other());
        if was_empty && other.control & (1 << 10) != 0 {
            Self::raise(side.other(), irq::IPC_RECEIVE_NOT_EMPTY)
        } else {
            [0; 2]
        }
    }

    /// `side` reads IPCFIFORECV. Returns the word and the interrupts raised.
    pub(crate) fn receive(&mut self, side: Side) -> (u32, Raised) {
        if self.port(side).control & 0x8000 == 0 {
            return (
                self.port(side.other()).send.front().copied().unwrap_or(0),
                [0; 2],
            );
        }
        let Some(value) = self.port_mut(side.other()).send.pop_front() else {
            // Reading an empty FIFO is an error and returns the last word.
            let port = self.port_mut(side);
            port.control |= 1 << 14;
            return (port.last_received, [0; 2]);
        };
        self.port_mut(side).last_received = value;
        let sender = self.port(side.other());
        let raised = if sender.send.is_empty() && sender.control & (1 << 2) != 0 {
            Self::raise(side.other(), irq::IPC_SEND_EMPTY)
        } else {
            [0; 2]
        };
        (value, raised)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_shows_each_side_the_others_nibble() {
        let mut ipc = Ipc::default();
        ipc.write_sync(Side::Arm7, 0x4000); // ARM7 wants sync IRQs
        let raised = ipc.write_sync(Side::Arm9, 0x0A00 | 1 << 13);
        assert_eq!(raised, [0, irq::IPC_SYNC]);
        assert_eq!(ipc.read_sync(Side::Arm7) & 0xF, 0xA);
        assert_eq!(ipc.read_sync(Side::Arm9) & 0xF00, 0xA00);
    }

    #[test]
    fn words_cross_in_order_and_raise_fifo_interrupts() {
        let mut ipc = Ipc::default();
        ipc.write_control(Side::Arm9, 0x8000 | 1 << 2); // enable, send-empty IRQ
        ipc.write_control(Side::Arm7, 0x8000 | 1 << 10); // enable, receive IRQ
        assert_eq!(ipc.send(Side::Arm9, 1), [0, irq::IPC_RECEIVE_NOT_EMPTY]);
        assert_eq!(ipc.send(Side::Arm9, 2), [0, 0]);
        assert_eq!(ipc.read_control(Side::Arm7) & 0x100, 0, "receive not empty");
        assert_eq!(ipc.receive(Side::Arm7), (1, [0; 2]));
        assert_eq!(ipc.receive(Side::Arm7), (2, [irq::IPC_SEND_EMPTY, 0]));
        // Empty: error flag, last word again.
        assert_eq!(ipc.receive(Side::Arm7).0, 2);
        assert_ne!(ipc.read_control(Side::Arm7) & 1 << 14, 0);
    }

    #[test]
    fn a_full_fifo_refuses_words() {
        let mut ipc = Ipc::default();
        ipc.write_control(Side::Arm9, 0x8000);
        for word in 0..17 {
            ipc.send(Side::Arm9, word);
        }
        assert_eq!(ipc.read_control(Side::Arm9) & 0x4002, 0x4002);
        ipc.write_control(Side::Arm9, 0x8000 | 1 << 3 | 1 << 14); // clear, ack error
        assert_eq!(ipc.read_control(Side::Arm9) & 0x4003, 0x0001);
    }
}
