//! Run with:
//!   SHARDD_API_KEY=sk_live_... cargo run --example quickstart
//!
//! Creates a credit event, reads it back via list_events + get_balances,
//! then retries the same write to demonstrate idempotency.

use shardd::{Client, CreateEventOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("SHARDD_API_KEY").expect("set SHARDD_API_KEY in your environment");
    let bucket = std::env::var("SHARDD_BUCKET").unwrap_or_else(|_| "demo".into());

    let client = Client::new(api_key)?;

    // 1. Credit 500 units to user:alice. Fresh UUID nonce — first-write
    //    path, server returns 201.
    let first = client
        .create_event(
            &bucket,
            "user:alice",
            500,
            CreateEventOptions {
                note: Some("sdk quickstart credit".into()),
                ..Default::default()
            },
        )
        .await?;
    println!(
        "credited: event={} balance={} deduplicated={}",
        first.event.event_id, first.balance, first.deduplicated
    );

    // 2. Same logical operation, same nonce — server returns the
    //    original event with deduplicated=true. Use this pattern for
    //    safe retries.
    let replay = client
        .create_event(
            &bucket,
            "user:alice",
            500,
            CreateEventOptions {
                note: Some("sdk quickstart credit".into()),
                idempotency_nonce: Some(first.event.idempotency_nonce.clone()),
                ..Default::default()
            },
        )
        .await?;
    println!(
        "retried:  event={} balance={} deduplicated={}",
        replay.event.event_id, replay.balance, replay.deduplicated
    );
    assert_eq!(first.event.event_id, replay.event.event_id);

    // 3. Read back the bucket.
    let balances = client.get_balances(&bucket).await?;
    for row in &balances.accounts {
        println!(
            "  {} = {} (available {})",
            row.account, row.balance, row.available_balance
        );
    }

    // 4. Inspect edge selection.
    let h = client.health(None).await?;
    println!(
        "pinned edge: {} (region {}, sync_gap {:?})",
        h.edge_id.as_deref().unwrap_or("?"),
        h.region.as_deref().unwrap_or("?"),
        h.sync_gap
    );

    Ok(())
}
