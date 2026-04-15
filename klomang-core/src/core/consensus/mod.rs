pub mod ghostdag;
pub mod ordering;
pub mod emission;
pub mod reward;
pub mod economic_constants;

pub use ghostdag::GhostDag;
pub use emission::{block_reward, total_emitted, capped_reward, max_supply};
pub use reward::{
    calculate_fees, calculate_accepted_fees, block_total_reward,
    validate_coinbase_reward,
};
pub use economic_constants::{
    MAX_GLOBAL_SUPPLY_NANO_SLUG,
    MINER_REWARD_PERCENT,
    FULLNODE_REWARD_PERCENT,
    BURN_ADDRESS,
    NO_BURN_ENFORCEMENT_ACTIVE,
    GAS_COLLECTION_POLICY,
    verify_non_burn_address,
    verify_all_non_burn_recipients,
    validate_miner_share,
    validate_fullnode_share,
};
