use alloy::{primitives::U256, providers::ext::AnvilApi};
use std::time::Duration;
use tokio::time::sleep;

use crate::common::{TestCounter, setup_sync_from_latest_scanner};
use event_scanner::{Notification, assert_empty, assert_event_sequence_final, assert_next};

#[tokio::test]
async fn happy_path_no_duplicates() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: produce 6 events total
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Ask for the latest 3, then live
    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) },
            TestCounter::CountIncreased { newCount: U256::from(6) },
        ]
    );
    let mut stream = assert_empty!(stream);

    // Live phase: emit three more, should arrive in order without duplicating latest
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Assert `SwitchingToLive` after emitting live events, because the test finishes the "latest
    // events" phase before new events are emitted, thus the "live" phase actually starts from a
    // future block.
    assert_next!(stream, Notification::SwitchingToLive);
    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(7) },
            TestCounter::CountIncreased { newCount: U256::from(8) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn fewer_historical_then_continues_live() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 5, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: only 2 available
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    // Latest phase returns all available
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
        ]
    );
    let mut stream = assert_empty!(stream);

    // Live: two more arrive
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Assert `SwitchingToLive` after emitting live events, because the test finishes the "latest
    // events" phase before new events are emitted, thus the "live" phase actually starts from a
    // future block.
    assert_next!(stream, Notification::SwitchingToLive);
    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(3) },
            TestCounter::CountIncreased { newCount: U256::from(4) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn exact_historical_count_then_live() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 4, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: produce exactly 4 events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
            TestCounter::CountIncreased { newCount: U256::from(4) },
        ]
    );
    let mut stream = assert_empty!(stream);

    // give scanner time to subscribe to live events
    sleep(Duration::from_millis(10)).await;
    // Live continues
    contract.increase().send().await?.watch().await?;

    // Assert `SwitchingToLive` after emitting live events, because the test finishes the "latest
    // events" phase before new events are emitted, thus the "live" phase actually starts from a
    // future block.
    assert_next!(stream, Notification::SwitchingToLive);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn no_historical_only_live_streams() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 5, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    // Latest is empty
    assert_next!(stream, Notification::NoPastLogsFound);
    let mut stream = assert_empty!(stream);

    // give scanner time to set up live subscription
    sleep(Duration::from_millis(10)).await;
    // Live events arrive
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Assert `SwitchingToLive` after emitting live events, because the test finishes the "latest
    // events" phase before new events are emitted, thus the "live" phase actually starts from a
    // future block.
    assert_next!(stream, Notification::SwitchingToLive);
    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn block_gaps_do_not_affect_number_of_events_streamed() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: emit 3, mine 1 empty block to form a clear boundary
    contract.increase().send().await?.watch().await?;

    provider.primary().anvil_mine(Some(1), None).await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    provider.primary().anvil_mine(Some(1), None).await?;

    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    let mut stream = assert_empty!(stream);

    // give scanner time to start
    sleep(Duration::from_millis(10)).await;
    // Immediately produce a new live event in a new block
    contract.increase().send().await?.watch().await?;

    // Assert `SwitchingToLive` after emitting live events, because the test finishes the "latest
    // events" phase before new events are emitted, thus the "live" phase actually starts from a
    // future block.
    assert_next!(stream, Notification::SwitchingToLive);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn waiting_on_live_logs_arriving() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: emit 3
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_empty!(stream);

    // `Notification::SwitchingToLive` arrives only on first live block received

    Ok(())
}
