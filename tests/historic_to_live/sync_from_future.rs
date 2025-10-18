use crate::common::{increase, setup_scanner};
use alloy::{eips::BlockNumberOrTag, providers::Provider};
use event_scanner::assert_next;
use tokio_stream::wrappers::ReceiverStream;

#[tokio::test]
async fn sync_from_future_block_waits_until_minted() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let provider = setup.provider;
    let client = setup.client;

    // Determine a start height in the future
    let latest = provider.get_block_number().await?;
    let future_start = latest + 3;

    // Start the scanner in sync mode from the future block
    tokio::spawn(
        async move { client.start_scanner(BlockNumberOrTag::from(future_start), None).await },
    );

    // Send 2 transactions that should not appear in the stream
    _ = increase(&contract).await?;
    _ = increase(&contract).await?;

    // Assert: no messages should be received before reaching the start height
    let inner = setup.stream.into_inner();
    assert!(inner.is_empty());
    let mut stream = ReceiverStream::new(inner);

    // Act: emit an event that will be mined in block == future_start
    let expected = &[increase(&contract).await?];

    // Assert: the first streamed message arrives and contains the expected event
    assert_next!(stream, expected);

    Ok(())
}
