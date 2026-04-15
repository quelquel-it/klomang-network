#[derive(Debug)]
pub struct DagStore {
    pub edge_count: usize,
}

impl DagStore {
    pub fn new() -> Self {
        Self { edge_count: 0 }
    }

    pub fn add_edge(&mut self) {
        self.edge_count = self.edge_count.saturating_add(1);
    }
}
