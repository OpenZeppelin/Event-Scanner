use alloy::primitives::U256;
use event_scanner::assert_event_sequence_final;

use crate::common::{LiveScannerSetup, TestCounter::CountIncreased, setup_live_scanner};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let LiveScannerSetup { contract, provider: _p, scanner, mut stream, anvil: _a } =
        setup_live_scanner(None, None, 0).await?;

    scanner.start().await?;

    tokio::spawn(async move {
        for _ in 0..100 {
            contract
                .increase()
                .send()
                .await
                .expect("should send")
                .watch()
                .await
                .expect("should confirm");
        }
    });

    let expected =
        (1..=100).map(|n| CountIncreased { newCount: U256::from(n) }).collect::<Vec<_>>();
    assert_event_sequence_final!(stream, expected);

    Ok(())
}
