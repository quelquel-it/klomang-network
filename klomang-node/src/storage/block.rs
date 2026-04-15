#[derive(Debug)]
pub struct BlockStore {
    pub block_count: usize,
}

impl BlockStore {
    pub fn new() -> Self {
        Self { block_count: 0 }
    }

    pub fn add_block(&mut self) {
        self.block_count = self.block_count.saturating_add(1);
    }
}
