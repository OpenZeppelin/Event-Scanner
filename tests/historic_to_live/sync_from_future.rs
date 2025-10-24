use crate::common::{increase, setup_sync_scanner};
use event_scanner::assert_next;
use tokio_stream::wrappers::ReceiverStream;

#[tokio::test]
async fn sync_from_future_block_waits_until_minted() -> anyhow::Result<()> {
    let future_start_block = 4;
    let setup = setup_sync_scanner(None, None, future_start_block, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;

    // Start the scanner in sync mode from the future block
    scanner.start().await?;

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
