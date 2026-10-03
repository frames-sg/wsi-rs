use super::*;

impl PositionedFile {
    pub(crate) fn poison_position_for_test(&self) {
        let _guard = self.position.lock().unwrap();
        panic!("poison the positional read fallback lock");
    }
}
