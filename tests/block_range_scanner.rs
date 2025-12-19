use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    providers::{Provider, ProviderBuilder, ext::AnvilApi},
    rpc::types::anvil::ReorgOptions,
};
use alloy_node_bindings::Anvil;
use event_scanner::{
    BlockRangeScannerBuilder, Notification, ScannerError, assert_closed, assert_empty, assert_next,
    assert_range_coverage,
};

#[tokio::test]
async fn live_mode_processes_all_blocks_respecting_block_confirmations() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    // --- Zero block confirmations -> stream immediately ---

    let mut brs = BlockRangeScannerBuilder::new().connect(provider.clone()).await?;

    let mut stream = brs.stream_live(0).await?;

    provider.anvil_mine(Some(5), None).await?;

    assert_range_coverage!(stream, 1..=5);
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(1), None).await?;

    assert_next!(stream, 6..=6);
    assert_empty!(stream);

    // --- 1 block confirmation  ---

    let mut stream = brs.stream_live(1).await?;

    provider.anvil_mine(Some(5), None).await?;

    assert_range_coverage!(stream, 6..=10);
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(1), None).await?;

    assert_next!(stream, 11..=11);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn live_with_block_confirmations_always_emits_genesis_block() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;
    let mut brs = BlockRangeScannerBuilder::new().connect(provider.clone()).await?;

    let mut stream = brs.stream_live(3).await?;

    provider.anvil_mine(Some(1), None).await?;
    assert_next!(stream, 0..=0);
    let stream = assert_empty!(stream);

    provider.anvil_mine(Some(2), None).await?;
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(5), None).await?;
    assert_range_coverage!(stream, 1..=5);
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(1), None).await?;
    assert_next!(stream, 6..=6);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn stream_from_starts_at_latest_once_it_has_enough_confirmations() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    let brs = BlockRangeScannerBuilder::new().connect(provider.clone()).await?;

    provider.anvil_mine(Some(20), None).await?;

    let stream = brs.stream_from(BlockNumberOrTag::Latest, 5).await?;

    let stream = assert_empty!(stream);

    provider.anvil_mine(Some(4), None).await?;
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(1), None).await?;
    assert_next!(stream, Notification::SwitchingToLive);
    assert_next!(stream, 20..=20);
    let mut stream = assert_empty!(stream);

    provider.anvil_mine(Some(1), None).await?;
    assert_next!(stream, 21..=21);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn continuous_blocks_if_reorg_less_than_block_confirmation() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    let mut brs = BlockRangeScannerBuilder::new().connect(provider.clone()).await?;

    let mut stream = brs.stream_live(5).await?;

    // mine initial blocks
    provider.anvil_mine(Some(10), None).await?;

    // assert initial block ranges immediately to avoid Anvil race condition:
    //
    // when a reorg happens after anvil_mine, Anvil occasionally first streams a non-zero block
    // number, which makes it impossible to deterministically assert the first expected block range
    // streamed by the scanner
    assert_range_coverage!(stream, 0..=5);
    let mut stream = assert_empty!(stream);

    // reorg less blocks than the block_confirmation config
    provider.anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs: vec![] }).await?;
    // mint additional blocks so the scanner processes reorged blocks
    provider.anvil_mine(Some(5), None).await?;

    // no ReorgDetected should be emitted
    assert_range_coverage!(stream, 6..=10);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn shallow_block_confirmation_does_not_mitigate_reorg() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    let mut brs = BlockRangeScannerBuilder::new().connect(provider.clone()).await?;

    let mut stream = brs.stream_live(3).await?;

    // mine initial blocks
    provider.anvil_mine(Some(10), None).await?;

    // assert initial block ranges immediately to avoid Anvil race condition:
    //
    // when a reorg happens after anvil_mine, Anvil occasionally first streams a non-zero block
    // number, which makes it impossible to deterministically assert the first expected block range
    // streamed by the scanner
    assert_range_coverage!(stream, 0..=7);
    let mut stream = assert_empty!(stream);

    // reorg more blocks than the block_confirmation config
    let latest = provider.get_block_number().await?;
    provider.anvil_reorg(ReorgOptions { depth: 8, tx_block_pairs: vec![] }).await?;

    // mint 1 block to allow the scanner to process reorged blocks (previously streamed + the block
    // confirmed now)
    provider.anvil_mine(Some(1), None).await?;

    assert_next!(stream, Notification::ReorgDetected { common_ancestor: latest - 8 });
    assert_range_coverage!(stream, 3..=8);
    let mut stream = assert_empty!(stream);

    // mint additional blocks to allow the scanner to stream all of the pre-reorg blocks
    provider.anvil_mine(Some(3), None).await?;
    assert_range_coverage!(stream, 9..=10);

    Ok(())
}

#[tokio::test]
#[ignore = "historical has reorg logic, but it can be reliably tested once ack-channels are introduced: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn historical_emits_correction_range_when_reorg_below_end() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(120), None).await?;

    let end_num = 110;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(30).connect(provider.clone()).await?;

    let mut stream = brs
        .stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Number(end_num))
        .await?;

    assert_next!(stream, 0..=29);
    assert_next!(stream, 30..=56);
    assert_next!(stream, 57..=86);
    assert_next!(stream, 87..=110);
    let mut stream = assert_empty!(stream);

    let latest = provider.get_block_number().await?;
    let depth = 15;
    _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;

    assert_next!(stream, Notification::ReorgDetected { common_ancestor: latest - depth });
    assert_next!(stream, 106..=110);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "historical has reorg logic, but it can be reliably tested once ack-channels are introduced: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn historical_emits_correction_range_when_end_num_reorgs() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(120), None).await?;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(30).connect(provider.clone()).await?;

    let mut stream =
        brs.stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Latest).await?;

    assert_next!(stream, 0..=29);
    assert_next!(stream, 30..=56);
    assert_next!(stream, 57..=86);
    assert_next!(stream, 87..=116);
    assert_next!(stream, 117..=120);
    let mut stream = assert_empty!(stream);

    let latest = provider.get_block_number().await?;
    let depth = 1;
    _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;

    assert_next!(stream, Notification::ReorgDetected { common_ancestor: latest - depth });
    assert_next!(stream, 120..=120);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "enable when ack-channels are introduced: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn historical_reorg_occurring_immediately_after_finalized_processing_is_ignored()
-> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(11), None).await?;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(10).connect(provider.clone()).await?;

    let mut stream =
        brs.stream_historical(BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    assert_next!(stream, 0..=0);
    let mut stream = assert_empty!(stream);

    _ = provider.anvil_reorg(ReorgOptions { depth: 3, tx_block_pairs: vec![] }).await;

    // no Notification::ReorgDetected is streamed
    assert_next!(stream, 1..=11);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn historic_mode_respects_blocks_read_per_epoch() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(100), None).await?;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(5).connect(provider.clone()).await?;

    // ranges where each batch is of max blocks per epoch size
    let mut stream = brs.stream_historical(0, 19).await?;
    assert_next!(stream, 0..=4);
    assert_next!(stream, 5..=9);
    assert_next!(stream, 10..=14);
    assert_next!(stream, 15..=19);
    assert_closed!(stream);

    // ranges where last batch is smaller than blocks per epoch
    let mut stream = brs.stream_historical(93, 99).await?;
    assert_next!(stream, 93..=97);
    assert_next!(stream, 98..=99);
    assert_closed!(stream);

    // range where blocks per epoch is larger than the number of blocks in the range
    let mut stream = brs.stream_historical(3, 5).await?;
    assert_next!(stream, 3..=5);
    assert_closed!(stream);

    // single item range
    let mut stream = brs.stream_historical(3, 3).await?;
    assert_next!(stream, 3..=3);
    assert_closed!(stream);

    // range where blocks per epoch is larger than the number of blocks on chain
    let mut brs = BlockRangeScannerBuilder::new().max_block_range(200).connect(provider).await?;

    let mut stream = brs.stream_historical(0, 20).await?;
    assert_next!(stream, 0..=20);
    assert_closed!(stream);

    let mut stream = brs.stream_historical(0, 99).await?;
    assert_next!(stream, 0..=36);
    assert_next!(stream, 37..=99);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn historic_mode_normalises_start_and_end_block() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(11), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(5).connect(provider).await?;

    let mut stream = brs.stream_historical(10, 0).await?;
    assert_next!(stream, 0..=0);
    assert_next!(stream, 1..=5);
    assert_next!(stream, 6..=10);
    assert_closed!(stream);

    let mut stream = brs.stream_historical(0, 10).await?;
    assert_next!(stream, 0..=0);
    assert_next!(stream, 1..=5);
    assert_next!(stream, 6..=10);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_single_batch_when_epoch_larger_than_range() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(150), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(100).connect(provider).await?;

    let mut stream = brs.stream_rewind(100, 150).await?;

    // Range length is 51, epoch is 100 -> single batch [100..=150]
    assert_next!(stream, 100..=150);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_exact_multiple_of_epoch_creates_full_batches_in_reverse() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(15), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(5).connect(provider).await?;

    let mut stream = brs.stream_rewind(0, 14).await?;

    // 0..=14 with epoch 5 -> [10..=14, 5..=9, 0..=4]
    assert_next!(stream, 10..=14);
    assert_next!(stream, 5..=9);
    assert_next!(stream, 0..=4);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_with_remainder_trims_first_batch_to_stream_start() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(15), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(4).connect(provider).await?;

    let mut stream = brs.stream_rewind(3, 12).await?;

    // 3..=12 with epoch 4 -> ends: 12,8,4 -> batches: [9..=12, 5..=8, 3..=4]
    assert_next!(stream, 9..=12);
    assert_next!(stream, 5..=8);
    assert_next!(stream, 3..=4);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_single_block_range() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(15), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(5).connect(provider).await?;

    let mut stream = brs.stream_rewind(7, 7).await?;

    assert_next!(stream, 7..=7);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_epoch_of_one_sends_each_block_in_reverse_order() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(15), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(1).connect(provider).await?;

    let mut stream = brs.stream_rewind(5, 8).await?;

    // 5..=8 with epoch 1 -> [8..=8, 7..=7, 6..=6, 5..=5]
    assert_next!(stream, 8..=8);
    assert_next!(stream, 7..=7);
    assert_next!(stream, 6..=6);
    assert_next!(stream, 5..=5);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn command_rewind_defaults_latest_to_earliest_batches_correctly() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    // Mine 20 blocks, so the total number of blocks is 21 (including 0th block)
    provider.anvil_mine(Some(20), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(7).connect(provider).await?;

    let mut stream =
        brs.stream_rewind(BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    assert_next!(stream, 14..=20);
    assert_next!(stream, 7..=13);
    assert_next!(stream, 0..=6);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn command_rewind_handles_start_and_end_in_any_order() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    // Ensure blocks at 3 and 15 exist
    provider.anvil_mine(Some(16), None).await?;

    let mut brs = BlockRangeScannerBuilder::new().max_block_range(5).connect(provider).await?;

    let mut stream = brs.stream_rewind(15, 3).await?;

    assert_next!(stream, 11..=15);
    assert_next!(stream, 6..=10);
    assert_next!(stream, 3..=5);
    assert_closed!(stream);

    let mut stream = brs.stream_rewind(3, 15).await?;

    assert_next!(stream, 11..=15);
    assert_next!(stream, 6..=10);
    assert_next!(stream, 3..=5);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn command_rewind_propagates_block_not_found_error() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    // Do not mine up to 999 so start won't exist
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
    let mut brs = BlockRangeScannerBuilder::new().max_block_range(5).connect(provider).await?;

    let stream = brs.stream_rewind(0, 999).await;

    assert!(matches!(
        stream,
        Err(ScannerError::BlockNotFound(BlockId::Number(BlockNumberOrTag::Number(999))))
    ));

    Ok(())
}

#[tokio::test]
#[ignore = "stream_rewind reorg tests require ack-channels to reliably halt processing: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn rewind_reorg_emits_notification_and_rescans_affected_range() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(20), None).await?;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(5).connect(provider.clone()).await?;

    let mut stream = brs.stream_rewind(5, 20).await?;

    assert_next!(stream, 16..=20);
    assert_next!(stream, 11..=15);

    // NOTE: Pause scanner
    _ = provider.anvil_reorg(ReorgOptions { depth: 3, tx_block_pairs: vec![] }).await;

    assert_next!(stream, Notification::ReorgDetected { common_ancestor: 17 });
    // Rescan range from common_ancestor + 1 to tip
    assert_next!(stream, 18..=20);
    //Rewind continues from where it left off
    assert_next!(stream, 6..=10);
    assert_next!(stream, 5..=5);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "stream_rewind reorg tests require ack-channels to reliably halt processing: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn deep_rewind_reorg_streams_affected_range_in_chronologi() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(20), None).await?;

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(5).connect(provider.clone()).await?;

    let mut stream = brs.stream_rewind(5, 20).await?;

    assert_next!(stream, 16..=20);

    // TODO: Pause scanning
    // Deep reorg: >= max_block_range * 2
    _ = provider.anvil_reorg(ReorgOptions { depth: 10, tx_block_pairs: vec![] }).await;

    assert_next!(stream, Notification::ReorgDetected { common_ancestor: 10 });
    // Rescan range from common_ancestor (11) to tip (20)
    assert_next!(stream, 11..=15);
    assert_next!(stream, 16..=20);
    // Rewind continues from where it left off (batch_from = 15)
    assert_next!(stream, 10..=15);
    assert_next!(stream, 5..=9);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "stream_rewind reorg tests require ack-channels to reliably halt processing: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn rewind_skips_reorg_check_when_tip_below_finalized() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(100), None).await?;

    let finalized = provider.get_block_by_number(BlockNumberOrTag::Finalized).await?.unwrap();

    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(5).connect(provider.clone()).await?;

    // Rewind with tip < finalized
    let mut stream = brs.stream_rewind(0, finalized.header.number - 1).await?;

    assert_next!(stream, 31..=35);
    assert_next!(stream, 26..=30);
    assert_next!(stream, 21..=25);

    // TODO: pause scanning
    _ = provider.anvil_reorg(ReorgOptions { depth: 5, tx_block_pairs: vec![] }).await;

    // no ReorgDetected should be emitted
    assert_next!(stream, 16..=20);
    assert_next!(stream, 11..=15);
    assert_next!(stream, 6..=10);
    assert_next!(stream, 1..=5);
    assert_next!(stream, 0..=0);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "stream_rewind reorg tests require ack-channels to reliably halt processing: https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn rewind_skips_reorg_when_tip_is_at_finalized() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(100), None).await?;

    let finalized = provider.get_block_by_number(BlockNumberOrTag::Finalized).await?.unwrap();
    let mut brs =
        BlockRangeScannerBuilder::new().max_block_range(5).connect(provider.clone()).await?;

    // Rewind with tip == finalized
    let mut stream = brs.stream_rewind(0, finalized.header.number).await?;

    assert_next!(stream, 32..=36);
    assert_next!(stream, 27..=31);
    assert_next!(stream, 22..=26);

    // TODO: pause scanning
    _ = provider.anvil_reorg(ReorgOptions { depth: 5, tx_block_pairs: vec![] }).await;

    // no ReorgDetected should be emitted
    assert_next!(stream, 17..=21);
    assert_next!(stream, 12..=16);
    assert_next!(stream, 7..=11);
    assert_next!(stream, 2..=6);
    assert_next!(stream, 0..=1);
    assert_closed!(stream);

    Ok(())
}
