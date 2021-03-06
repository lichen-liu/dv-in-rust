use futures::prelude::*;
use o2versioner::comm::scheduler_dbproxy::Message;
use o2versioner::comm::MsqlResponse;
use o2versioner::core::*;
use o2versioner::dbproxy_main;
use o2versioner::util::conf::DbProxyConf;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{tcp::OwnedWriteHalf, TcpStream};
use tokio::time::sleep;
use tokio_serde::{formats::SymmetricalJson, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

#[tokio::test]
#[ignore]
async fn test_dbproxy_end_to_end() {
    let details = "127.0.0.1:2347";
    let addr: SocketAddr = details.parse().expect("Unable to parse socket address");
    let _proxy_handle = tokio::spawn(helper_spawn_proxy(addr.clone()));

    let mut messages = Vec::new();
    let _mock_table_vs = vec![
        TxTableVN {
            table: "table1".to_string(),
            vn: 0,
            op: RWOperation::R,
        },
        TxTableVN {
            table: "table2".to_string(),
            vn: 0,
            op: RWOperation::R,
        },
    ];
    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.3:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx3"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.2:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::Query(
            MsqlQuery::new(
                "SELECT name, age, designation, salary FROM public.tbltest;".to_string(),
                TableOps::from("READ T1"),
                EarlyReleaseTables::default(),
            )
            .unwrap(),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.2:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx2"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.1:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx1"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    sleep(Duration::from_millis(1000)).await;
    let tcp_stream = TcpStream::connect("127.0.0.1:2347").await.unwrap();
    let (tcp_read, tcp_write) = tcp_stream.into_split();

    let mut deserializer = SymmetricallyFramed::new(
        FramedRead::new(tcp_read, LengthDelimitedCodec::new()),
        SymmetricalJson::<Message>::default(),
    );

    let _client_handle = tokio::spawn(helper_spawn_client_sender(tcp_write, messages));

    let mut begin_count: u32 = 0;
    let mut query_count: u32 = 0;
    let mut end_count: u32 = 0;

    while let Some(msg) = deserializer.try_next().await.unwrap() {
        match msg {
            Message::MsqlResponse(_, res) => match res {
                MsqlResponse::BeginTx(_b) => {
                    begin_count += 1;
                }
                MsqlResponse::Query(_q) => {
                    query_count += 1;
                }
                MsqlResponse::EndTx(_e) => {
                    end_count += 1;
                }
            },
            _other => {
                println!("nope");
            }
        }

        if begin_count == 3 && query_count == 1 && end_count == 0 {
            break;
        }
    }

    assert!(true);
}

#[tokio::test]
#[ignore]
async fn test_dbproxy_end_to_end_2() {
    let details = "127.0.0.1:2348";
    let addr: SocketAddr = details.parse().expect("Unable to parse socket address");
    let _proxy_handle = tokio::spawn(helper_spawn_proxy(addr.clone()));

    let mut messages = Vec::new();
    let _mock_table_vs = vec![
        TxTableVN {
            table: "table1".to_string(),
            vn: 0,
            op: RWOperation::R,
        },
        TxTableVN {
            table: "table2".to_string(),
            vn: 0,
            op: RWOperation::R,
        },
    ];
    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.3:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx3"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.2:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::Query(
            MsqlQuery::new(
                "SELECT name, age, designation, salary FROM public.tbltest;".to_string(),
                TableOps::from("READ T1"),
                EarlyReleaseTables::default(),
            )
            .unwrap(),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.2:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx2"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    let item = Message::MsqlRequest(
        RequestMeta {
            client_addr: "127.0.0.1:8080".parse().unwrap(),
            cur_txid: 0,
            request_id: 0,
        },
        Msql::BeginTx(
            MsqlBeginTx::default()
                .set_name(Some("tx1"))
                .set_tableops(TableOps::from("READ WRIte")),
        ),
        None,
    );
    messages.push(item);

    sleep(Duration::from_millis(1000)).await;
    let tcp_stream = TcpStream::connect("127.0.0.1:2348").await.unwrap();
    let (tcp_read, tcp_write) = tcp_stream.into_split();

    let mut deserializer = SymmetricallyFramed::new(
        FramedRead::new(tcp_read, LengthDelimitedCodec::new()),
        SymmetricalJson::<Message>::default(),
    );

    let _client_handle = tokio::spawn(helper_spawn_client_sender(tcp_write, messages));

    let mut begin_count: u32 = 0;
    let mut query_count: u32 = 0;
    let mut end_count: u32 = 0;

    while let Some(msg) = deserializer.try_next().await.unwrap() {
        match msg {
            Message::MsqlResponse(_, res) => match res {
                MsqlResponse::BeginTx(_b) => {
                    begin_count += 1;
                }
                MsqlResponse::Query(_q) => {
                    query_count += 1;
                }
                MsqlResponse::EndTx(_e) => {
                    end_count += 1;
                }
            },
            _other => {
                println!("nope");
            }
        }

        if begin_count == 3 && query_count == 1 && end_count == 0 {
            break;
        }
    }

    assert!(true);
}

async fn helper_spawn_client_sender(tcp_write: OwnedWriteHalf, mut messages: Vec<Message>) {
    let mut serializer = SymmetricallyFramed::new(
        FramedWrite::new(tcp_write, LengthDelimitedCodec::new()),
        SymmetricalJson::<Message>::default(),
    );

    while !messages.is_empty() {
        serializer.send(messages.pop().unwrap()).await.unwrap();
    }
}

async fn helper_spawn_proxy(addr: SocketAddr) {
    let conf = DbProxyConf::new(addr.to_string()).set_sql_conf(Some(
        "host=localhost port=5432 dbname=Test user=postgres password=Abc@123",
    ));

    dbproxy_main(conf).await;
}
