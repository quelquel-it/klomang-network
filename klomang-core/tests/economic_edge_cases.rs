#[cfg(test)]
mod tests {
    use klomang_core::core::consensus::economic_constants;
    
    #[test]
    fn test_decimal_division_edge_cases() {
        // Test 1: Odd number causing decimal division
        let total_reward = 3u128;
        let miner_share = (total_reward * 80) / 100; // 2.4 -> 2 (truncated)
        let fullnode_share = total_reward - miner_share; // 3 - 2 = 1
        
        assert_eq!(miner_share, 2);
        assert_eq!(fullnode_share, 1);
        assert_eq!(miner_share + fullnode_share, 3);
        println!("Test 1 PASSED: No Nano-SLUG lost with total=3");
        
        // Test 2: Very small reward
        let small_reward = 1u128;
        let miner_small = (small_reward * 80) / 100; // 0.8 -> 0
        let fullnode_small = small_reward - miner_small; // 1 - 0 = 1
        
        assert_eq!(miner_small, 0);
        assert_eq!(fullnode_small, 1);
        assert_eq!(miner_small + fullnode_small, 1);
        println!("Test 2 PASSED: No Nano-SLUG lost with total=1");
        
        // Test 3: Large number
        let large_reward = 1000000000000u128;
        let miner_large = (large_reward * 80) / 100;
        let fullnode_large = large_reward - miner_large;
        
        assert_eq!(miner_large, 800000000000);
        assert_eq!(fullnode_large, 200000000000);
        assert_eq!(miner_large + fullnode_large, large_reward);
        println!("Test 3 PASSED: No Nano-SLUG lost with large total");
    }
    
    #[test]
    fn test_economic_constants_validation() {
        assert_eq!(economic_constants::MAX_GLOBAL_SUPPLY_NANO_SLUG, 600_000_000_000_000_000);
        assert_eq!(economic_constants::MINER_REWARD_PERCENT, 80);
        assert_eq!(economic_constants::FULLNODE_REWARD_PERCENT, 20);
        assert_eq!(economic_constants::BURN_ADDRESS, [0u8; 32]);
        const _: () = assert!(economic_constants::NO_BURN_ENFORCEMENT_ACTIVE);
        println!("Economic constants validation PASSED");
    }
}
