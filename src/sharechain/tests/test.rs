#[cfg(test)]
mod tests {
    use minotari_app_grpc::tari_rpc::{Block, BlockHeader, ProofOfWork, SubmitBlockRequest};
    use tari_common::configuration::Network;
    use tari_common_types::tari_address::{TariAddress, TariAddressFeatures};
    use tari_crypto::{keys::PublicKey as CryptoPubKey, ristretto::RistrettoPublicKey};

    use crate::{
        sharechain::{
            block::{Block as ShareChainBlock, BlockBuilder},
            ShareChain,
        },
        InMemoryShareChain,
    };

    fn new_random_address() -> TariAddress {
        let mut rng = rand::thread_rng();
        let (_, pk) = RistrettoPublicKey::random_keypair(&mut rng);
        TariAddress::new_single_address(pk, Network::LocalNet, TariAddressFeatures::INTERACTIVE)
    }

    fn new_sharechain_blocks(n: u64) -> Vec<ShareChainBlock> {
        let mut blocks = Vec::new();
        for i in 1..n + 1 {
            let block = BlockBuilder::new()
                .with_height(i)
                .with_miner_wallet_address(new_random_address())
                .build();
            blocks.push(block);
        }
        blocks
    }

    async fn generate_block_request(payment_address: String) -> SubmitBlockRequest {
        SubmitBlockRequest {
            block: Some(Block {
                header: Some(BlockHeader {
                    hash: [0; 32].to_vec(),
                    version: 0,
                    height: 1,
                    prev_hash: [0; 32].to_vec(),
                    timestamp: 1720167829,
                    output_mr: [0; 32].to_vec(),
                    kernel_mr: [0; 32].to_vec(),
                    input_mr: [0; 32].to_vec(),
                    total_kernel_offset: [0; 32].to_vec(),
                    nonce: 119018423820796913,
                    pow: Some(ProofOfWork {
                        pow_algo: 1,
                        pow_data: Vec::new(),
                    }),
                    kernel_mmr_size: 795,
                    output_mmr_size: 804,
                    total_script_offset: [0; 32].to_vec(),
                    validator_node_mr: [0; 32].to_vec(),
                    validator_node_size: 0,
                }),
                body: None,
            }),
            wallet_payment_address: payment_address,
        }
    }

    #[tokio::test]
    async fn submit_blocks_nominal_case() {
        let chain = InMemoryShareChain::default();

        let block_1 = BlockBuilder::new().with_height(1).build();
        let op = chain.submit_block(&block_1).await;
        assert!(op.is_ok());

        let tip = chain.tip_height().await;
        assert!(tip.is_ok());
        assert_eq!(tip.unwrap(), 1);

        let block_2 = BlockBuilder::new().with_height(2).build();
        let block_3 = BlockBuilder::new().with_height(3).build();
        let blocks = vec![block_2.clone(), block_3.clone()];
        let op = chain.submit_blocks(blocks.clone(), false).await;
        assert!(op.is_ok());

        let tip = chain.tip_height().await;
        assert!(tip.is_ok());
        assert_eq!(tip.unwrap(), 3);

        // only block with height greater than 1
        let blocks = chain.blocks(1).await;
        assert!(blocks.is_ok());
        let blocks = blocks.unwrap();
        assert_eq!(blocks.clone().len(), 2);
        assert_eq!(blocks[0], block_2);
        assert_eq!(blocks[1], block_3);
    }

    #[tokio::test]
    async fn generate_shares_nominal_case() {
        let chain = InMemoryShareChain::default();

        let blocks = new_sharechain_blocks(3);
        let op = chain.submit_blocks(blocks.clone(), false).await;
        assert!(op.is_ok());

        // every miner has obtained 1% of 100 shares (= 1)
        let shares = chain.generate_shares(100).await;
        assert_eq!(shares.len(), 3);
        for share in shares {
            assert_eq!(share.value, 1);
        }
        let chain = InMemoryShareChain::default();
        let _ = chain.submit_blocks(blocks, false).await;

        // every miner has obtained 2% of 100 shares (= 2)
        let shares = chain.generate_shares(200).await;
        assert_eq!(shares.len(), 3);
        for share in shares {
            assert_eq!(share.value, 2);
        }
    }

    #[tokio::test]
    async fn new_block_nominal_case() {
        let chain = InMemoryShareChain::default();

        let req = generate_block_request(new_random_address().to_hex()).await;
        let op = chain.new_block(&req).await;
        assert!(op.is_ok());

        let block = op.unwrap();
        assert!(block.height() == 1);
    }

    #[tokio::test]
    async fn new_block_error_no_block() {
        let chain = InMemoryShareChain::default();

        let req = SubmitBlockRequest {
            block: None,
            wallet_payment_address: new_random_address().to_hex(),
        };
        let op = chain.new_block(&req).await;

        assert!(op.is_err());
        assert_eq!(
            op.err().unwrap().to_string(),
            "gRPC Block conversion error: Missing field: block"
        );
    }

    #[tokio::test]
    async fn new_block_error_invalid_address() {
        let chain = InMemoryShareChain::default();

        // not in hex format, error
        let req = generate_block_request(new_random_address().to_string()).await;
        let op = chain.new_block(&req).await;

        assert!(op.is_err());
        assert_eq!(
            op.err().unwrap().to_string(),
            "Tari address error: Cannot recover public key"
        );
    }
}
