use super::admin_handler::*;
use super::core::*;
use super::dispatcher::*;
use super::logging::*;
use super::transceiver::*;
use crate::comm::MsqlResponse;
use crate::comm::{scheduler_api, scheduler_sequencer};
use crate::core::*;
use crate::util::conf::*;
use crate::util::executor::Executor;
use crate::util::tcp;
use bb8::Pool;
use futures::future::Either;
use futures::pin_mut;
use futures::prelude::*;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::signal;
use tokio::sync::oneshot;
use tokio::sync::Mutex;
use tokio_serde::formats::SymmetricalJson;
use tokio_serde::SymmetricallyFramed;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{error, field, info, info_span, instrument, trace, warn, Instrument, Span};
use unicase::UniCase;

/// Main entrance for the Scheduler
///
/// # Modes
/// 1. Unlimited, the server will keep running forever.
/// 2. Limit the total maximum number of input connections,
/// once reaching that limit, no new connections are accepted.
/// 3. Admin port, can send `kill`, `exit` or `quit` in raw bytes
/// to the admin port, which will then force to not accept any new
/// connections.
///
/// # Notes
/// Upon receiving CTRL-C signal, scheduler will shutdown with
/// possible *INCONSISTEN* state. Please use the above mentioned modes
/// to properly stop the scheduler
#[instrument(name = "scheduler", skip(conf))]
pub async fn main(conf: Conf) {
    // Create the main state
    let state = State::new(DbVNManager::from_iter(conf.to_dbproxy_addrs()), conf.clone());

    // The current task completes as soon as start_tcplistener finishes,
    // which happens when it reaches the max_conn_till_dropped if not None,
    // which is really depending on the incoming connections into Scheduler.
    // So the sequencer_socket_pool here does not require an explicit
    // max_lifetime being set.
    // Prepare sequencer pool
    let sequencer_socket_pool = Pool::builder()
        .max_size(conf.scheduler.sequencer_pool_size)
        .min_idle(Some(1))
        .build(tcp::TcpStreamConnectionManager::new(conf.sequencer.to_addr()).await)
        .await
        .unwrap();

    // Prepare transceiver
    let (transceiver_addrs, transceivers): (Vec<_>, Vec<_>) = conf
        .to_dbproxy_addrs()
        .into_iter()
        .map(|dbproxy_addr| {
            let (trscaddrs, trsc) = Transceiver::new(conf.scheduler.transceiver_queue_size, dbproxy_addr);
            ((dbproxy_addr, trscaddrs), trsc)
        })
        .unzip();

    // Prepare dispatcher
    let (dispatcher_addr, dispatcher) = Dispatcher::new(
        conf.scheduler.dispatcher_queue_size,
        state.share_dbvn_manager(),
        DbproxyManager::from_iter(transceiver_addrs),
    );

    // Launch transceiver as a new task
    let transceiver_handle = tokio::spawn(
        stream::iter(transceivers)
            .for_each_concurrent(None, |transceiver| Box::new(transceiver).run())
            .in_current_span(),
    );

    // Launch dispatcher as a new task
    let dispatcher_handle = tokio::spawn(Box::new(dispatcher).run().in_current_span());

    // Create a stop_signal channel if admin mode is turned on
    let (stop_tx, stop_rx) = if conf.scheduler.admin_addr.is_some() {
        let (tx, rx) = oneshot::channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Launch main handler as a new task
    let conf_clone = conf.scheduler.clone();
    let sequencer_socket_pool_clone = sequencer_socket_pool.clone();
    let state_clone = state.clone();
    let handler_handle = tokio::spawn(
        tcp::start_tcplistener(
            conf.scheduler.to_addr(),
            move |tcp_stream| {
                let sequencer_socket_pool = sequencer_socket_pool_clone.clone();
                let state_cloned = state_clone.clone();
                let conf = conf_clone.clone();
                // Connection/session specific storage
                // Note: this closure contains one copy of dispatcher_addr
                // Then, for each connection, a new dispatcher_addr is cloned
                let dispatcher_addr = Arc::new(dispatcher_addr.clone());
                async move {
                    let client_addr = tcp_stream.peer_addr().unwrap();
                    let conn_state =
                        ConnectionState::new(client_addr.clone(), state_cloned.share_client_record(client_addr).await);
                    process_connection(conf, tcp_stream, conn_state, sequencer_socket_pool, dispatcher_addr).await;
                }
            },
            conf.scheduler.max_connection,
            stop_rx,
        )
        .in_current_span(),
    );

    // Combine the dispatcher handle and main handler handle into a main_handle
    let main_handle = future::try_join3(transceiver_handle, dispatcher_handle, handler_handle);
    let ctrl_c_handle = signal::ctrl_c();

    pin_mut!(ctrl_c_handle);
    // Exit when either main_handle finishes or ctrl_c signal is received
    let main_handle_with_signal = future::select(main_handle, ctrl_c_handle).map(|res| match res {
        Either::Left((main_res, _)) => main_res
            .map(|_| info!("main_handle finished"))
            .map_err(|e| e.to_string()),
        Either::Right((ctrl_c_res, _)) => ctrl_c_res
            .map(|_| info!("Received CTRL_C, terminating main_handle"))
            .map_err(|e| e.to_string()),
    });

    // Allow scheduler to be terminated by admin
    if let Some(admin_addr) = &conf.scheduler.admin_addr {
        let admin_handle = tokio::spawn(
            admin(
                admin_addr.parse().unwrap(),
                stop_tx,
                sequencer_socket_pool,
                state.clone(),
            )
            .in_current_span(),
        );

        // main_handle can either run to finish or be the result
        // of the above stop_tx.send()
        main_handle_with_signal.await.unwrap();

        // At this point, we just want to cancel the admin_handle
        tokio::select! {
            _ = future::ready(()) => info!("Scheduler admin is terminated" ),
            _ = admin_handle => {}
        };
    } else {
        main_handle_with_signal.await.unwrap();
    }

    // Dump logging files
    state.dump_perf_log().await;

    info!("DIES");
}

#[instrument(name = "admin", skip(admin_addr, stop_tx, sequencer_socket_pool, state))]
async fn admin(
    admin_addr: SocketAddr,
    stop_tx: Option<oneshot::Sender<()>>,
    sequencer_socket_pool: Pool<tcp::TcpStreamConnectionManager>,
    state: State,
) {
    start_admin_tcplistener(admin_addr, move |msg| {
        let sequencer_socket_pool = sequencer_socket_pool.clone();
        let state = state.clone();
        async move {
            let cmd_registry: HashMap<_, Vec<_>> = vec![
                ("block_unblock", vec!["block", "unblock"]),
                ("kill", vec!["kill", "exit", "quit"]),
                ("perf", vec!["perf"]),
            ]
            .into_iter()
            .map(|(k, vs)| (k, vs.into_iter().map(|v| UniCase::new(String::from(v))).collect()))
            .collect();

            let command = UniCase::new(msg);

            if cmd_registry.get("block_unblock").unwrap().contains(&command) {
                let m = if command == UniCase::new("block") {
                    scheduler_sequencer::Message::RequestBlock
                } else {
                    scheduler_sequencer::Message::RequestUnblock
                };
                let reply = tcp::send_and_receive_single_as_json(&mut sequencer_socket_pool.get().await.unwrap(), m)
                    .map_err(|e| e.to_string())
                    .and_then(|res| match res {
                        scheduler_sequencer::Message::ReplyBlockUnblock(m) => future::ok(m),
                        _ => future::ok(String::from("Invalid response from Sequencer")),
                    })
                    .map_ok_or_else(|e| e, |m| m)
                    .await;
                (reply, true)
            } else if cmd_registry.get("kill").unwrap().contains(&command) {
                let reply = format!(
                    "Scheduler is going to Stop. {}",
                    tcp::send_and_receive_single_as_json(
                        &mut sequencer_socket_pool.get().await.unwrap(),
                        scheduler_sequencer::Message::RequestStop,
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|res| match res {
                        scheduler_sequencer::Message::ReplyStop => {
                            future::ok(String::from("Sequencer received Stop"))
                        }
                        _ => future::ok(String::from("Invalid response from Sequencer")),
                    })
                    .map_ok_or_else(|e| e, |m| m)
                    .await
                );
                (reply, false)
            } else if cmd_registry.get("perf").unwrap().contains(&command) {
                let location_dumped = state.dump_perf_log().await;
                (format!("Perf logging dumped to {:?}", location_dumped), true)
            } else {
                (
                    format!("Unknown command: {}. Available commands: {:?}", command, cmd_registry),
                    true,
                )
            }
        }
    })
    .await;

    stop_tx.unwrap().send(()).unwrap();

    info!("DIES");
}

/// Process the `tcp_stream` for a single connection
///
/// Will process all messages sent via this `tcp_stream` on this tcp connection.
/// Once this tcp connection is closed, this function will return
#[instrument(name="conn", skip(conf, socket, conn_state, sequencer_socket_pool, dispatcher_addr), fields(message=field::Empty))]
async fn process_connection(
    conf: SchedulerConf,
    mut socket: TcpStream,
    conn_state: ConnectionState,
    sequencer_socket_pool: Pool<tcp::TcpStreamConnectionManager>,
    dispatcher_addr: Arc<DispatcherAddr>,
) {
    let client_addr = socket.peer_addr().unwrap();

    Span::current().record("message", &&client_addr.to_string()[..]);

    let (tcp_read, tcp_write) = socket.split();

    // Delimit frames from bytes using a length header
    let delimited_read = FramedRead::new(tcp_read, LengthDelimitedCodec::new());
    let delimited_write = FramedWrite::new(tcp_write, LengthDelimitedCodec::new());

    // Deserialize/Serialize frames using JSON codec
    let serded_read = SymmetricallyFramed::new(delimited_read, SymmetricalJson::<scheduler_api::Message>::default());
    let serded_write = SymmetricallyFramed::new(delimited_write, SymmetricalJson::<scheduler_api::Message>::default());

    // Each individual connection communication is executed in blocking order,
    // the socket is dedicated for one session only, opposed to being shared for multiple sessions.
    // At any given point, there is at most one transaction.
    // Connection/session specific storage
    let conn_state = Arc::new(Mutex::new(conn_state));
    let conn_state_cloned = conn_state.clone();

    // Process a stream of incoming messages from a single tcp connection
    let dispatcher_addr_cloned = dispatcher_addr.clone();
    serded_read
        .and_then(move |msg| {
            let conf_cloned = conf.clone();
            let conn_state_cloned = conn_state_cloned.clone();
            let sequencer_socket_pool_cloned = sequencer_socket_pool.clone();
            let dispatcher_addr_cloned = dispatcher_addr_cloned.clone();
            trace!("<- {:?}", msg);

            async move {
                Ok(process_request(
                    conf_cloned,
                    msg,
                    conn_state_cloned,
                    sequencer_socket_pool_cloned,
                    dispatcher_addr_cloned,
                )
                .await)
            }
        })
        .inspect_err(|err| {
            warn!("Can not decode input bytes: {:?}", err);
        })
        .forward(serded_write)
        .map(|_| ())
        .await;

    let mut conn_state = Arc::try_unwrap(conn_state).unwrap().into_inner();

    if conn_state.current_txvn().is_some() {
        warn!(
            "Unclosed transaction. Aborting the transaction.. {:?}",
            conn_state.current_txvn()
        );

        let response = process_endtx(Msql::EndTx(MsqlEndTx::rollback()), &mut conn_state, &dispatcher_addr).await;
        warn!("Aborting unclosed transaction successfully. {:?}", response);
    }

    info!("Connection dropped. {:?}", conn_state);
}

#[instrument(name="request", skip(conf, msg, conn_state, sequencer_socket_pool, dispatcher_addr), fields(message=field::Empty, id=field::Empty, txid=field::Empty, cmd=field::Empty))]
async fn process_request(
    conf: SchedulerConf,
    msg: scheduler_api::Message,
    conn_state: Arc<Mutex<ConnectionState>>,
    sequencer_socket_pool: Pool<tcp::TcpStreamConnectionManager>,
    dispatcher_addr: Arc<DispatcherAddr>,
) -> scheduler_api::Message {
    // Not creating any critical session indeed, process_msql will always be executing in serial
    let mut conn_state_guard = conn_state.lock().await;
    Span::current().record("message", &msg.as_ref());

    let response = match msg {
        scheduler_api::Message::RequestMsql(msql) => {
            process_msql(
                conf,
                msql,
                &mut conn_state_guard,
                sequencer_socket_pool,
                dispatcher_addr,
            )
            .await
        }
        scheduler_api::Message::RequestMsqlText(msqltext) => match Msql::try_from(msqltext) {
            // Try to convert MsqlText to Msql first
            Ok(msql) => {
                process_msql(
                    conf,
                    msql,
                    &mut conn_state_guard,
                    sequencer_socket_pool,
                    dispatcher_addr,
                )
                .await
            }
            Err(e) => scheduler_api::Message::InvalidMsqlText(e.to_owned()),
        },
        scheduler_api::Message::RequestCrash(reason) => {
            error!("<- Soft Crash Request: {}", reason);
            error!("Connection state: {:?}", conn_state_guard);
            panic!("Received a soft crash request");
        }
        _ => scheduler_api::Message::InvalidRequest,
    };
    trace!("-> {:?}", response);

    response
    // conn_state_guard should be dropped here
}

async fn process_msql(
    conf: SchedulerConf,
    msql: Msql,
    conn_state: &mut ConnectionState,
    sequencer_socket_pool: Pool<tcp::TcpStreamConnectionManager>,
    dispatcher_addr: Arc<DispatcherAddr>,
) -> scheduler_api::Message {
    Span::current().record("cmd", &msql.as_ref());
    Span::current().record("txid", &conn_state.client_meta().current_txid());
    Span::current().record("id", &conn_state.current_request_id().await);

    // Start the RequestRecord
    let reqrecord = RequestRecord::start(&msql, conn_state.current_txvn());
    let msqlresponse = match msql {
        Msql::BeginTx(msqlbegintx) => process_begintx(msqlbegintx, conn_state, &sequencer_socket_pool).await,
        Msql::Query(mut query) => {
            if query.has_early_release() {
                if conf.disable_early_release || query.tableops().access_pattern().is_read_only() {
                    warn!(
                        "Removing early release annotation due to settings or ReadOnly query. {:?} {:?}",
                        query.tableops(),
                        query.early_release_tables()
                    );
                    query.drop_early_release();
                }
            }

            if conn_state.current_txvn().is_none()
                && (query.access_pattern().is_write_only() || conf.disable_single_read_optimization)
            {
                if query.access_pattern().is_read_only() {
                    info!("Unoptimized Single Read query");
                } else {
                    info!("Single Write query");
                }

                // Construct a new MsqlBeginTx
                let msqlbegintx = MsqlBeginTx::from(query.tableops().clone());
                process_begintx(msqlbegintx, conn_state, &sequencer_socket_pool).await;
                // Execute the query
                let resp = process_query(Msql::Query(query), conn_state, &dispatcher_addr).await;
                // Construct a new MsqlEndTx
                let msqlendtx = Msql::EndTx(MsqlEndTx::commit());
                process_endtx(msqlendtx, conn_state, &dispatcher_addr).await;
                resp
            } else {
                process_query(Msql::Query(query), conn_state, &dispatcher_addr).await
            }
        }
        Msql::EndTx(_) => process_endtx(msql, conn_state, &dispatcher_addr).await,
    };

    // Store the RequestRecord
    conn_state
        .push_request_record(reqrecord.finish(&msqlresponse, conn_state.current_txvn()))
        .await;

    scheduler_api::Message::Reply(msqlresponse)
}

/// Helper function to check the legality of the current `Msql` request,
/// this should be called after legalization
fn process_msql_legality(msql: &Msql, txvn_opt: &Option<TxVN>) -> Result<(), MsqlResponse> {
    match Legality::final_check(msql, txvn_opt) {
        Legality::Critical(e) => {
            warn!("{} {:?} {:?}", e, msql, txvn_opt);
            Err(MsqlResponse::err(e, msql))
        }
        Legality::Panic(e) => {
            error!("{} {:?} {:?}", e, msql, txvn_opt);
            panic!("{}", e);
        }
        _ => Ok(()),
    }
}

async fn process_begintx(
    msqlbegintx: MsqlBeginTx,
    conn_state: &mut ConnectionState,
    sequencer_socket_pool: &Pool<tcp::TcpStreamConnectionManager>,
) -> MsqlResponse {
    if let Err(msqlresponse) = process_msql_legality(&Msql::BeginTx(msqlbegintx.clone()), conn_state.current_txvn()) {
        return msqlresponse;
    }

    assert!(conn_state.current_txvn().is_none());

    let mut sequencer_socket = sequencer_socket_pool.get().await.unwrap();
    let msg = scheduler_sequencer::Message::RequestTxVN(conn_state.client_meta().clone(), msqlbegintx);

    tcp::send_and_receive_single_as_json(&mut sequencer_socket, msg)
        .map_err(|e| e.to_string())
        .and_then(|res| async {
            match res {
                scheduler_sequencer::Message::ReplyTxVN(txvn) => {
                    if let Some(txvn) = txvn {
                        let existing = conn_state.replace_txvn(Some(txvn));
                        assert!(existing.is_none());
                        Ok(())
                    } else {
                        Err(String::from("Can't get a TxVN from Sequencer"))
                    }
                }
                _ => Err(String::from("Invalid response from Sequencer")),
            }
        })
        .map_ok_or_else(|e| MsqlResponse::begintx_err(e), |_| MsqlResponse::begintx_ok())
        .instrument(info_span!("<->sequencer"))
        .await
}

async fn process_query(
    msql: Msql,
    conn_state: &mut ConnectionState,
    dispatcher_addr: &Arc<DispatcherAddr>,
) -> MsqlResponse {
    if let Err(msqlresponse) = process_msql_legality(&msql, conn_state.current_txvn()) {
        return msqlresponse;
    }

    if conn_state.current_txvn().is_none() {
        if msql.try_get_query().unwrap().access_pattern().is_read_only() {
            info!("Optimized Single Read query");
        } else {
            panic!("TxVN should not be None here");
        }
    }

    dispatcher_addr
        .request(DispatcherRequest::new(
            conn_state.client_meta().clone(),
            msql,
            conn_state.current_txvn().clone(),
            conn_state.current_request_id().await,
        ))
        .map_ok_or_else(
            |e| MsqlResponse::query_err(e),
            |res| {
                let DispatcherReply { msql_res, txvn_res } = res;
                conn_state.replace_txvn(txvn_res);
                msql_res
            },
        )
        .await
}

async fn process_endtx(
    msql: Msql,
    conn_state: &mut ConnectionState,
    dispatcher_addr: &Arc<DispatcherAddr>,
) -> MsqlResponse {
    if let Err(msqlresponse) = process_msql_legality(&msql, conn_state.current_txvn()) {
        return msqlresponse;
    }

    let txvn = conn_state.replace_txvn(None);
    assert!(txvn.is_some());

    dispatcher_addr
        .request(DispatcherRequest::new(
            conn_state.client_meta().clone(),
            msql,
            txvn,
            conn_state.current_request_id().await,
        ))
        .map_ok_or_else(
            |e| MsqlResponse::endtx_err(e),
            |res| {
                let DispatcherReply { msql_res, txvn_res } = res;
                let existing = conn_state.replace_txvn(txvn_res);
                assert!(existing.is_none());
                conn_state.client_meta_as_mut().transaction_finished();
                msql_res
            },
        )
        .await
}
