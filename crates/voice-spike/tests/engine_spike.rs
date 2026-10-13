//! The spike's acceptance: the loopback Opus-over-WebRTC exchange completes,
//! every frame arrives, and the audio survives sample-exact. This is the
//! evidence the §2 #4 engine decision is made on.

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn the_daemons_own_stack_carries_opus_losslessly_on_loopback() {
    let report = tokio::time::timeout(Duration::from_secs(60), voice_spike::engine_proof())
        .await
        .expect("the engine proof must finish inside 60s on loopback")
        .expect("the engine proof ran without an unexpected error");

    println!("{}", report.text);
    assert!(report.verdict_ok, "engine proof failed: {}", report.text);
}
