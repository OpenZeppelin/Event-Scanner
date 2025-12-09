use alloy::{primitives::address, providers::ProviderBuilder, sol, sol_types::SolEvent};
use tokio::time::Instant;
use tokio_stream::StreamExt;

use event_scanner::{EventFilter, EventScannerBuilder, ScannerMessage};

sol! {
    struct BlockParams {
        // the max number of transactions in this block. Note that if there are not enough
        // transactions in calldata or blobs, the block will contains as many transactions as
        // possible.
        uint16 numTransactions;
        // The time difference (in seconds) between the timestamp of this block and
        // the timestamp of the parent block in the same batch. For the first block in a batch,
        // there is not parent block in the same batch, so the time shift should be 0.
        uint8 timeShift;
        // Signals sent on L1 and need to sync to this L2 block.
        bytes32[] signalSlots;
    }

    struct BatchMetadata {
        bytes32 infoHash;
        address proposer;
        uint64 batchId;
        uint64 proposedAt; // Used by node/client
    }

    struct BaseFeeConfig {
        uint8 adjustmentQuotient;
        uint8 sharingPctg;
        uint32 gasIssuancePerSecond;
        uint64 minGasExcess;
        uint32 maxGasIssuancePerBlock;
    }

    struct BatchInfo {
        bytes32 txsHash;
        // Data to build L2 blocks
        BlockParams[] blocks;
        bytes32[] blobHashes;
        bytes32 extraData;
        address coinbase;
        uint64 proposedIn; // Used by node/client
        uint64 blobCreatedIn;
        uint32 blobByteOffset;
        uint32 blobByteSize;
        uint32 gasLimit;
        uint64 lastBlockId;
        uint64 lastBlockTimestamp;
        // Data for the L2 anchor transaction, shared by all blocks in the batch
        uint64 anchorBlockId;
        // corresponds to the `_anchorStateRoot` parameter in the anchor transaction.
        // The batch's validity proof shall verify the integrity of these two values.
        bytes32 anchorBlockHash;
        BaseFeeConfig baseFeeConfig;
    }

    event BatchProposed(BatchInfo info, BatchMetadata meta, bytes txList);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .connect("https://mainnet.infura.io/v3/7f91ecc67a254027b28f302a4e813fab")
        .await?;

    let mut scanner = EventScannerBuilder::historic()
        .from_block(23_303_089)
        .to_block(23_932_937)
        .max_block_range(10000)
        .connect(provider)
        .await?;

    let filter = EventFilter::new()
        .event(BatchProposed::SIGNATURE)
        .contract_address(address!("0x06a9Ab27c7e2255df1815E6CC0168d7755Feb19a"));
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    let start = Instant::now();

    let mut log_count = 0;
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(ScannerMessage::Data(logs)) => {
                println!("count: {:?}", logs.len());
                log_count += logs.len();
            }
            Ok(ScannerMessage::Notification(notification)) => {
                panic!("notification: {:?}", notification)
            }
            Err(e) => {
                panic!("{:?}", e);
            }
        }
    }

    let elapsed = start.elapsed();

    // 3. Print the elapsed time using the debug formatter
    println!("Time elapsed: {:.2?}", elapsed);
    println!("Log count: {log_count}");

    Ok(())
}
