use super::{cap_t, CapTag};


impl cap_t {

    #[inline]
    pub fn new_irq_control_cap() -> Self {
        let mut cap = cap_t::default();

        cap.words[0] = 0 | (CapTag::CapIrqControlCap as usize & 0x1fusize) << 59;
        cap.words[1] = 0;
        cap
    }
}

#[inline]
pub fn cap_irq_control_cap_new() -> cap_t {
    cap_t::new_irq_control_cap()
}