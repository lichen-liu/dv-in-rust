use super::core::State;
use crate::comm::scheduler_sequencer;
use crate::util::config::SequencerConfig;
use crate::util::tcp;
use futures::prelude::*;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_serde::formats::SymmetricalJson;
use tokio_serde::SymmetricallyFramed;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{debug, warn};

/// Main entrance for Sequencer
pub async fn main(conf: SequencerConfig) {
    let state = Arc::new(Mutex::new(State::new()));

    tcp::start_tcplistener(
        conf.to_addr(),
        |tcp_stream| {
            let state_cloned = state.clone();
            process_connection(tcp_stream, state_cloned)
        },
        conf.max_connection,
        "Sequencer",
        None
    )
    .await;
}

/// Process the `tcp_stream` for a single connection
///
/// Will process all messages sent via this `tcp_stream` on this tcp connection.
/// Once this tcp connection is closed, this function will return
async fn process_connection(mut tcp_stream: TcpStream, state: Arc<Mutex<State>>) {
    let peer_addr = tcp_stream.peer_addr().unwrap();
    let (tcp_read, tcp_write) = tcp_stream.split();

    // Delimit frames from bytes using a length header
    let length_delimited_read = FramedRead::new(tcp_read, LengthDelimitedCodec::new());
    let length_delimited_write = FramedWrite::new(tcp_write, LengthDelimitedCodec::new());

    // Deserialize/Serialize frames using JSON codec
    let serded_read = SymmetricallyFramed::new(
        length_delimited_read,
        SymmetricalJson::<scheduler_sequencer::Message>::default(),
    );
    let serded_write = SymmetricallyFramed::new(
        length_delimited_write,
        SymmetricalJson::<scheduler_sequencer::Message>::default(),
    );

    // Process a stream of incoming messages from a single tcp connection
    serded_read
        .and_then(move |msg| {
            let state_cloned = state.clone();
            async move {
                match msg {
                    scheduler_sequencer::Message::RequestTxVN(sqlbegintx) => {
                        debug!("<- [{}] RequestTxVN on {:?}", peer_addr, sqlbegintx);
                        let txvn = state_cloned.lock().await.assign_vn(sqlbegintx);
                        debug!("-> [{}] Reply {:?}", peer_addr, txvn);
                        Ok(scheduler_sequencer::Message::ReplyTxVN(txvn))
                    }
                    other => {
                        warn!("<- [{}] Unsupported message {:?}", peer_addr, other);
                        Ok(scheduler_sequencer::Message::Invalid)
                    }
                }
            }
        })
        .forward(serded_write)
        .map(|_| ())
        .await;
}
