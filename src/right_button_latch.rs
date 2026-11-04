#[derive(Default)]
pub struct RightButtonLatch(bool);
impl RightButtonLatch {
    pub fn press(&mut self, accepted: bool) { self.0 = accepted; }
    pub fn release(&mut self) -> bool { std::mem::take(&mut self.0) }
}

#[cfg(test)]
mod tests {
    use super::RightButtonLatch;

    #[test]
    fn accepted_press_swallows_exactly_one_release_even_after_cancel() {
        let mut latch = RightButtonLatch::default();
        latch.press(true);
        assert!(latch.release());
        assert!(!latch.release());
    }

    #[test]
    fn ordinary_or_rejected_press_is_not_swallowed() {
        let mut latch = RightButtonLatch::default();
        latch.press(false);
        assert!(!latch.release());
    }
}