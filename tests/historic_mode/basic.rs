use alloy::eips::BlockNumberOrTag;
use event_scanner::{assert_closed, assert_next};

use crate::common::{TestCounterExt, setup_historic_scanner};

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
        contract.increase_and_get_meta().await?,
        contract.increase_and_get_meta().await?,
        contract.increase_and_get_meta().await?,
        contract.increase_and_get_meta().await?,
        contract.increase_and_get_meta().await?,
    ];

    scanner.start().await?;

    assert_next!(stream, expected);
    assert_closed!(stream);

    Ok(())
}
