use klomang_core::core::consensus::economic_constants;

fn main() {
    println!("Testing Economic Policy Edge Cases");
    println!("===================================");
    
    // Test 1: Decimal division edge case
    let total_reward = 3u128; // Odd number that causes decimal when divided by 5
    let miner_share = (total_reward * 80) / 100;
    let fullnode_share = total_reward - miner_share;
    
    println!("Test 1: Total reward = {} Nano-SLUG", total_reward);
    println!("Miner (80%): {} Nano-SLUG", miner_share);
    println!("Full Node (20%): {} Nano-SLUG", fullnode_share);
    println!("Sum: {} Nano-SLUG", miner_share + fullnode_share);
    println!("Lost: {} Nano-SLUG", total_reward - (miner_share + fullnode_share));
    println!();
    
    // Test 2: Very small reward
    let small_reward = 1u128;
    let miner_small = (small_reward * 80) / 100;
    let fullnode_small = small_reward - miner_small;
    
    println!("Test 2: Total reward = {} Nano-SLUG", small_reward);
    println!("Miner (80%): {} Nano-SLUG", miner_small);
    println!("Full Node (20%): {} Nano-SLUG", fullnode_small);
    println!("Sum: {} Nano-SLUG", miner_small + fullnode_small);
    println!("Lost: {} Nano-SLUG", small_reward - (miner_small + fullnode_small));
    println!();
    
    // Test 3: Large reward
    let large_reward = 1000000000000u128; // 1 trillion
    let miner_large = (large_reward * 80) / 100;
    let fullnode_large = large_reward - miner_large;
    
    println!("Test 3: Total reward = {} Nano-SLUG", large_reward);
    println!("Miner (80%): {} Nano-SLUG", miner_large);
    println!("Full Node (20%): {} Nano-SLUG", fullnode_large);
    println!("Sum: {} Nano-SLUG", miner_large + fullnode_large);
    println!("Lost: {} Nano-SLUG", large_reward - (miner_large + fullnode_large));
    println!();
    
    // Test 4: Check constants
    println!("Economic Constants:");
    println!("MAX_GLOBAL_SUPPLY: {}", economic_constants::MAX_GLOBAL_SUPPLY_NANO_SLUG);
    println!("MINER_PERCENT: {}", economic_constants::MINER_REWARD_PERCENT);
    println!("FULLNODE_PERCENT: {}", economic_constants::FULLNODE_REWARD_PERCENT);
    println!("BURN_ADDRESS: {:?}", economic_constants::BURN_ADDRESS);
}
