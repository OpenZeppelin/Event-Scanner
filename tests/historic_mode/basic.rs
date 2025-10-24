use alloy::eips::BlockNumberOrTag;
use event_scanner::assert_next;

use crate::common::{increase, setup_historic_scanner};

#[tokio::test]
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let setup = setup_historic_scanner(
        Some(0.1),
        None,
        BlockNumberOrTag::Earliest,
        BlockNumberOrTag::Latest,
    )
    .await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    let expected = &[
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
        increase(&contract).await?,
    ];

    tokio::spawn(async move { scanner.start().await });

    assert_next!(stream, expected);
    assert_next!(stream, None);

    Ok(())
}
