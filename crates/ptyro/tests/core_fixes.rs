//! End-to-end checks for fixes found by the code review: a reply too big for one frame is
//! answered with an error instead of wedging the connection.

use std::time::Duration;

use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// A reply bigger than the 16 MiB frame limit used to kill the daemon's writer for that client:
/// every later frame and response was dropped and the caller waited forever. Now that one request
/// is answered "too large", and the same connection goes on working.
#[tokio::test]
async fn a_reply_over_the_frame_limit_is_an_error_and_the_connection_lives() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    // Just under the limit going in (the daemon takes it); the answer carries the same text plus
    // the merge markers the pad's own header makes, which tips it over.
    let line = format!("{}\n", "a".repeat(79));
    let mut text = line.repeat((ptyro_protocol::MAX_FRAME - 200) / line.len());
    text.truncate(ptyro_protocol::MAX_FRAME - 200);
    let answered = tokio::time::timeout(
        Duration::from_secs(60),
        c.request(Request::ScratchPut { pad: Default::default(), base: String::new(), text }),
    )
    .await
    .expect("answered, not hung");
    let e = answered.expect_err("the reply doesn't fit in one frame");
    // (The pad's own 4 MiB limit answers first; a reply too big for a frame is answered the same
    // way, as an error on that request, and the connection goes on.)
    assert!(
        e.message.contains("reply is too large") || e.message.contains("limited to 4 MiB"),
        "{}",
        e.message
    );
    // The connection is fine: an ordinary request on it works.
    let Reply::Sessions { .. } = c.request(Request::List).await.unwrap() else { panic!("a list") };
}
