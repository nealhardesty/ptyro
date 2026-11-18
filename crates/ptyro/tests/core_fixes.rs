//! End-to-end checks for fixes found by the code review: a reply too big for one frame is
//! answered with an error instead of wedging the connection.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use ptyro_protocol::{
    ClientInfo, ClientKind, ClientMsg, Encoding, Outcome, PROTO, Reply, ServerMsg, decode, encode,
    framing,
};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// A reply bigger than the 16 MiB frame limit used to kill the daemon's writer for that client:
/// every later frame and response was dropped and the caller waited forever. Now that one request
/// is answered "too large", and the same connection goes on working.
#[tokio::test]
async fn a_reply_over_the_frame_limit_is_an_error_and_the_connection_lives() {
    let d = TempDaemon::start(BIN).unwrap();
    let mut opts = ptyro_client::ConnectOptions::new(d.paths.clone(), ClientKind::Test);
    opts.autospawn = false;
    let stream = ptyro_client::connect_raw(&opts).await.unwrap();
    let mut framed = tokio_util::codec::Framed::new(stream, framing());
    let hello = ClientMsg::Hello {
        proto: PROTO,
        client: ClientInfo {
            kind: ClientKind::Test,
            version: "x".into(),
            pid: 1,
            outer: None,
            token: None,
            via: None,
        },
        features: vec![],
    };
    framed.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();
    // A saved view the daemon takes, whose answer is four times its size: in JSON, `1e9` comes
    // back as `1000000000.0`. Sent as text, so this side doesn't grow it first.
    let n = (ptyro_protocol::MAX_FRAME / 2) / 4;
    let mut save =
        String::from(r#"{"t":"request","id":1,"req":{"t":"save_view","name":"big","view":["#);
    save.push_str(&vec!["1e9"; n].join(","));
    save.push_str("]}}");
    let mut ask = async |id: u32, req: tokio_util::bytes::Bytes| -> Outcome {
        framed.send(req).await.unwrap();
        loop {
            let bytes = tokio::time::timeout(Duration::from_secs(60), framed.next())
                .await
                .expect("answered, not hung")
                .unwrap()
                .unwrap();
            if let Ok((ServerMsg::Response { id: got, outcome }, _)) = decode::<ServerMsg>(&bytes)
                && got == id
            {
                return outcome;
            }
        }
    };
    let json = |id: u32, req: serde_json::Value| {
        encode(&serde_json::json!({ "t": "request", "id": id, "req": req }), Encoding::Json)
            .unwrap()
    };
    let saved = ask(1, save.into()).await;
    assert!(matches!(saved, Outcome::Ok { .. }), "the view was saved: {saved:?}");
    let load = json(2, serde_json::json!({ "t": "load_view", "name": "big" }));
    let Outcome::Err { error } = ask(2, load).await else {
        panic!("the reply doesn't fit in one frame")
    };
    assert!(error.message.contains("reply is too large"), "{}", error.message);
    // The connection is fine: an ordinary request on it works.
    let list = json(3, serde_json::json!({ "t": "list" }));
    let Outcome::Ok { reply: Reply::Sessions { .. } } = ask(3, list).await else {
        panic!("a list")
    };
}
