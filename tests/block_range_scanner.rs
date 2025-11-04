use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    providers::{ProviderBuilder, ext::AnvilApi},
    rpc::types::anvil::ReorgOptions,
};
use alloy_node_bindings::Anvil;
use event_scanner::{
    ScannerError, ScannerStatus, assert_closed, assert_empty, assert_next,
    block_range_scanner::{BlockRangeScanner, Message},
    robust_provider::RobustProvider,
};
use tokio_stream::StreamExt;

#[tokio::test]
async fn live_mode_processes_all_blocks_respecting_block_confirmations() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint().as_str()).await?;
    let robust_provider = RobustProvider::new(provider);

    // --- Zero block confirmations -> stream immediately ---

    let client = BlockRangeScanner::new().connect(robust_provider.clone()).run()?;

    let mut stream = client.stream_live(0).await?;

    robust_provider.inner().anvil_mine(Some(5), None).await?;

    assert_next!(stream, 1..=1);
    assert_next!(stream, 2..=2);
    assert_next!(stream, 3..=3);
    assert_next!(stream, 4..=4);
    assert_next!(stream, 5..=5);
    let mut stream = assert_empty!(stream);

    robust_provider.inner().anvil_mine(Some(1), None).await?;

    assert_next!(stream, 6..=6);
    assert_empty!(stream);

    // --- 1 block confirmation  ---

    let mut stream = client.stream_live(1).await?;

    robust_provider.inner().anvil_mine(Some(5), None).await?;

    assert_next!(stream, 6..=6);
    assert_next!(stream, 7..=7);
    assert_next!(stream, 8..=8);
    assert_next!(stream, 9..=9);
    assert_next!(stream, 10..=10);
    let mut stream = assert_empty!(stream);

    robust_provider.inner().anvil_mine(Some(1), None).await?;

    assert_next!(stream, 11..=11);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn stream_from_latest_starts_at_tip_not_confirmed() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint().as_str()).await?;
    provider.anvil_mine(Some(20), None).await?;

    let block_confirmations = 5;

    let robust_provider = RobustProvider::new(provider);

    let client = BlockRangeScanner::new().connect(robust_provider.clone()).run()?;

    let stream = client.stream_from(BlockNumberOrTag::Latest, block_confirmations).await?;

    let stream = assert_empty!(stream);

    robust_provider.inner().anvil_mine(Some(4), None).await?;

    let mut stream = assert_empty!(stream);

    robust_provider.inner().anvil_mine(Some(1), None).await?;

    assert_next!(stream, 20..=20);
    let mut stream = assert_empty!(stream);

    robust_provider.inner().anvil_mine(Some(1), None).await?;

    assert_next!(stream, 21..=21);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "Flaky test, see: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
async fn continuous_blocks_if_reorg_less_than_block_confirmation() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint().as_str()).await?;

    let block_confirmations = 5;

    let robust_provider = RobustProvider::new(provider.clone());

    let client = BlockRangeScanner::new().connect(robust_provider).run()?;

    let mut receiver = client.stream_live(block_confirmations).await?;

    provider.anvil_mine(Some(10), None).await?;

    provider
        .anvil_reorg(ReorgOptions { depth: block_confirmations - 1, tx_block_pairs: vec![] })
        .await?;

    provider.anvil_mine(Some(20), None).await?;

    let mut block_range_start = 0;

    let end_loop = 20;
    let mut i = 0;
    while let Some(Message::Data(range)) = receiver.next().await {
        if block_range_start == 0 {
            block_range_start = *range.start();
        }

        assert_eq!(block_range_start, *range.start());
        assert!(range.end() >= range.start());
        block_range_start = *range.end() + 1;
        i += 1;
        if i == end_loop {
            break;
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Flaky test, see: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
async fn shallow_block_confirmation_does_not_mitigate_reorg() -> anyhow::Result<()> {
    let anvil = Anvil::new().block_time(1).try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint().as_str()).await?;

    let block_confirmations = 3;

    let robust_provider = RobustProvider::new(provider.clone());

    let client = BlockRangeScanner::new().connect(robust_provider).run()?;

    let mut receiver = client.stream_live(block_confirmations).await?;

    provider.anvil_mine(Some(10), None).await?;

    provider
        .anvil_reorg(ReorgOptions { depth: block_confirmations + 5, tx_block_pairs: vec![] })
        .await?;

    provider.anvil_mine(Some(30), None).await?;
    receiver.close();

    let mut block_range_start = 0;

    let mut block_num = vec![];
    let mut reorg_detected = false;
    while let Some(msg) = receiver.next().await {
        match msg {
            Message::Data(range) => {
                if block_range_start == 0 {
                    block_range_start = *range.start();
                }
                block_num.push(range);
                if block_num.len() == 15 {
                    break;
                }
            }
            Message::Status(ScannerStatus::ReorgDetected) => {
                reorg_detected = true;
            }
            _ => {
                break;
            }
        }
    }
    assert!(reorg_detected, "Reorg should have been detected");

    // Generally check that there is a reorg in the range i.e.
    //                                                        REORG
    // [0..=0, 1..=1, 2..=2, 3..=3, 4..=4, 5..=5, 6..=6, 7..=7, 3..=3, 4..=4, 5..=5, 6..=6,
    // 7..=7, 8..=8, 9..=9] (Less flaky to assert this way)
    let mut found_reorg_pattern = false;
    for window in block_num.windows(2) {
        if window[1].start() < window[0].end() {
            found_reorg_pattern = true;
            break;
        }
    }
    assert!(found_reorg_pattern,);

    Ok(())
}

#[tokio::test]
#[ignore = "too flaky, un-ignore once a full local node is used: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
async fn historical_emits_correction_range_when_reorg_below_end() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(120), None).await?;

    let end_num = 110;

    let robust_provider = RobustProvider::new(provider.clone());

    let client = BlockRangeScanner::new().max_block_range(30).connect(robust_provider).run()?;

    let mut stream = client
        .stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Number(end_num))
        .await?;

    let depth = 15;
    _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;
    _ = provider.anvil_mine(Some(20), None).await;

    assert_next!(stream, 0..=29);
    assert_next!(stream, 30..=59);
    assert_next!(stream, 60..=89);
    assert_next!(stream, 90..=110);
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, 105..=110);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "too flaky, un-ignore once a full local node is used: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
async fn historical_emits_correction_range_when_end_num_reorgs() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;
    let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

    provider.anvil_mine(Some(120), None).await?;

    let end_num = 120;

    let robust_provider = RobustProvider::new(provider.clone());
    let client = BlockRangeScanner::new().max_block_range(30).connect(robust_provider).run()?;

    let mut stream = client
        .stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Number(end_num))
        .await?;

    let pre_reorg_mine = 20;
    _ = provider.anvil_mine(Some(pre_reorg_mine), None).await;
    let depth = pre_reorg_mine + 1;
    _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;
    _ = provider.anvil_mine(Some(20), None).await;

    assert_next!(stream, 0..=29);
    assert_next!(stream, 30..=59);
    assert_next!(stream, 60..=89);
    assert_next!(stream, 90..=120);
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, 120..=120);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn historic_mode_respects_blocks_read_per_epoch() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(100), None).await?;

    let robust_provider = RobustProvider::new(provider);
    let client =
        BlockRangeScanner::new().max_block_range(5).connect(robust_provider.clone()).run()?;

    // ranges where each batch is of max blocks per epoch size
    let mut stream = client.stream_historical(0, 19).await?;
    assert_next!(stream, 0..=4);
    assert_next!(stream, 5..=9);
    assert_next!(stream, 10..=14);
    assert_next!(stream, 15..=19);
    assert_closed!(stream);

    // ranges where last batch is smaller than blocks per epoch
    let mut stream = client.stream_historical(93, 99).await?;
    assert_next!(stream, 93..=97);
    assert_next!(stream, 98..=99);
    assert_closed!(stream);

    // range where blocks per epoch is larger than the number of blocks in the range
    let mut stream = client.stream_historical(3, 5).await?;
    assert_next!(stream, 3..=5);
    assert_closed!(stream);

    // single item range
    let mut stream = client.stream_historical(3, 3).await?;
    assert_next!(stream, 3..=3);
    assert_closed!(stream);

    // range where blocks per epoch is larger than the number of blocks on chain
    let client = BlockRangeScanner::new().max_block_range(200).connect(robust_provider).run()?;

    let mut stream = client.stream_historical(0, 20).await?;
    assert_next!(stream, 0..=20);
    assert_closed!(stream);

    let mut stream = client.stream_historical(0, 99).await?;
    assert_next!(stream, 0..=99);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn historic_mode_normalises_start_and_end_block() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
    provider.anvil_mine(Some(11), None).await?;

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(5).connect(robust_provider).run()?;

    let mut stream = client.stream_historical(10, 0).await?;
    assert_next!(stream, 0..=4);
    assert_next!(stream, 5..=9);
    assert_next!(stream, 10..=10);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_single_batch_when_epoch_larger_than_range() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(150), None).await?;

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(100).connect(robust_provider).run()?;

    let mut stream = client.rewind(100, 150).await?;

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

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(5).connect(robust_provider).run()?;

    let mut stream = client.rewind(0, 14).await?;

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

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(4).connect(robust_provider).run()?;

    let mut stream = client.rewind(3, 12).await?;

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

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(5).connect(robust_provider).run()?;

    let mut stream = client.rewind(7, 7).await?;

    assert_next!(stream, 7..=7);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn rewind_epoch_of_one_sends_each_block_in_reverse_order() -> anyhow::Result<()> {
    let anvil = Anvil::new().try_spawn()?;

    let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

    provider.anvil_mine(Some(15), None).await?;

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(1).connect(robust_provider).run()?;

    let mut stream = client.rewind(5, 8).await?;

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

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(7).connect(robust_provider).run()?;

    let mut stream = client.rewind(BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

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

    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(5).connect(robust_provider).run()?;

    let mut stream = client.rewind(15, 3).await?;

    assert_next!(stream, 11..=15);
    assert_next!(stream, 6..=10);
    assert_next!(stream, 3..=5);
    assert_closed!(stream);

    let mut stream = client.rewind(3, 15).await?;

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
    let robust_provider = RobustProvider::new(provider);
    let client = BlockRangeScanner::new().max_block_range(5).connect(robust_provider).run()?;

    let stream = client.rewind(0, 999).await;

    assert!(matches!(
        stream,
        Err(ScannerError::BlockNotFound(BlockId::Number(BlockNumberOrTag::Number(999))))
    ));

    Ok(())
}
