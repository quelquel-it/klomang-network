use crate::core::consensus::ghostdag::GhostDag;
use crate::core::dag::Dag;
use crate::core::crypto::Hash;

impl GhostDag {
    pub fn get_ordering(&self, dag: &Dag) -> Vec<Hash> {
        let mut blocks: Vec<_> = dag.get_all_hashes().into_iter().collect();
        blocks.sort_by(|a, b| {
            let a_score = dag.get_block(a).map_or(0, |b| b.blue_score);
            let b_score = dag.get_block(b).map_or(0, |b| b.blue_score);
            a_score.cmp(&b_score).then(a.cmp(b))
        });
        blocks
    }
}
