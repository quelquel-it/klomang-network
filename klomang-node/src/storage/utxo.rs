#[derive(Debug)]
pub struct UtxoStore {
    pub entry_count: usize,
}

impl UtxoStore {
    pub fn new() -> Self {
        Self { entry_count: 0 }
    }

    pub fn increment_entries(&mut self) {
        self.entry_count = self.entry_count.saturating_add(1);
    }
}
