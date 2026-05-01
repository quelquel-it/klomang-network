#[derive(Debug)]
pub struct TxStore {
    pub transaction_count: usize,
}

impl TxStore {
    pub fn new() -> Self {
        Self {
            transaction_count: 0,
        }
    }

    pub fn add_transaction(&mut self) {
        self.transaction_count = self.transaction_count.saturating_add(1);
    }
}
