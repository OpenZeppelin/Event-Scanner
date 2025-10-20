use alloy::{eips::BlockNumberOrTag, providers::Provider};
use event_scanner::assert_next;

use crate::common::{increase, setup_scanner};

#[tokio::test]
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let setup = setup_scanner(Some(0.1), None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let provider = setup.provider;
    let mut stream = setup.stream;

    let start_block = provider.get_block_number().await?;

    let expected = &[
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
    ];

    tokio::spawn(async move {
        client
            .start_scanner(BlockNumberOrTag::Number(start_block), Some(BlockNumberOrTag::Latest))
            .await
    });

    assert_next!(stream, expected);
    assert_next!(stream, None);

    Ok(())
}
