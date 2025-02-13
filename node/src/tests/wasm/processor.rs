use std::sync::Arc;
use std::time::Duration;

use futures::lock::Mutex;
use wasm_bindgen_test::*;

use crate::prelude::rings_core::async_trait;
use crate::prelude::rings_core::dht::TStabilize;
use crate::prelude::rings_core::message::MessageCallback;
use crate::prelude::rings_core::transports::manager::TransportHandshake;
use crate::prelude::rings_core::transports::manager::TransportManager;
use crate::prelude::rings_core::types::ice_transport::IceTrickleScheme;
use crate::prelude::rings_core::utils;
use crate::prelude::web3::contract::tokens::Tokenizable;
use crate::prelude::web_sys::RtcIceConnectionState;
use crate::prelude::*;
use crate::processor;
use crate::processor::*;
use crate::tests::wasm::prepare_processor;

async fn listen(p: &Processor) {
    let h = p.swarm.clone();
    let s = Arc::clone(&p.stabilization);

    futures::join!(
        async {
            h.listen().await;
        },
        async {
            s.wait().await;
        }
    );
}

async fn close_all_transport(p: &Processor) {
    futures::future::join_all(p.swarm.get_transports().iter().map(|(_, t)| t.close())).await;
}

struct MsgCallbackStruct {
    msgs: Arc<Mutex<Vec<String>>>,
}

#[async_trait(?Send)]
impl MessageCallback for MsgCallbackStruct {
    async fn custom_message(
        &self,
        _ctx: &MessagePayload<Message>,
        msg: &CustomMessage,
    ) -> Vec<MessageHandlerEvent> {
        let text = processor::unpack_text_message(msg).unwrap();
        console_log!("msg received: {}", text);
        let mut msgs = self.msgs.try_lock().unwrap();
        msgs.push(text);
        vec![]
    }

    async fn builtin_message(&self, _ctx: &MessagePayload<Message>) -> Vec<MessageHandlerEvent> {
        vec![]
    }
}

async fn create_connection(p1: &Processor, p2: &Processor) {
    console_log!("create_offer");
    let (transport_1, offer) = p1.swarm.create_offer().await.unwrap();
    let pendings_1 = p1.swarm.pending_transports().await.unwrap();
    // deal if transport is pending
    assert_eq!(pendings_1.len(), 1);

    assert_eq!(
        pendings_1.get(0).unwrap().id.to_string(),
        transport_1.id.to_string()
    );

    console_log!("answer_offer");
    let (transport_2, answer) = p2.swarm.answer_offer(offer).await.unwrap();

    console_log!("accept_answer");
    let peer = p1.swarm.accept_answer(answer).await.unwrap();

    loop {
        console_log!("waiting for connection");
        utils::js_utils::window_sleep(1000).await.unwrap();

        console_log!(
            "transport_1 state: {:?}",
            transport_1.ice_connection_state().await.unwrap()
        );
        console_log!(
            "transport_2 state: {:?}",
            transport_2.ice_connection_state().await.unwrap()
        );

        let s1 = transport_1.get_stats().await.unwrap();
        let s2 = transport_2.get_stats().await.unwrap();

        console_log!("transport_1 stats: {:?}", s1);
        console_log!("transport_2 stats: {:?}", s2);

        if transport_1.is_connected().await && transport_2.is_connected().await {
            break;
        }
    }

    assert!(peer.1.id.eq(&transport_1.id), "transport not same");

    futures::try_join!(
        async {
            if transport_1.is_connected().await {
                return Ok(());
            }
            transport_1.wait_for_connected().await
        },
        async {
            if transport_2.is_connected().await {
                return Ok(());
            }
            transport_2.wait_for_connected().await
        }
    )
    .unwrap();
}

#[wasm_bindgen_test]
async fn test_processor_handshake_and_msg() {
    let msgs1: Arc<Mutex<Vec<String>>> = Default::default();
    let msgs2: Arc<Mutex<Vec<String>>> = Default::default();
    let callback1 = Box::new(MsgCallbackStruct {
        msgs: msgs1.clone(),
    });
    let callback2 = Box::new(MsgCallbackStruct {
        msgs: msgs2.clone(),
    });

    let p1 = prepare_processor(Some(callback1)).await;
    let p2 = prepare_processor(Some(callback2)).await;

    let test_text1 = "test1";
    let test_text2 = "test2";
    let test_text3 = "test3";
    let test_text4 = "test4";
    let test_text5 = "test5";

    let p1_addr = p1.did().into_token().to_string();
    let p2_addr = p2.did().into_token().to_string();
    console_log!("p1_addr: {}", p1_addr);
    console_log!("p2_addr: {}", p2_addr);

    console_log!("listen");
    listen(&p1).await;
    listen(&p2).await;

    console_log!("processor_hs_connect_1_2");
    create_connection(&p1, &p2).await;

    fluvio_wasm_timer::Delay::new(Duration::from_secs(2))
        .await
        .unwrap();

    console_log!("processor_send_test_text_messages");
    p1.send_message(p2_addr.as_str(), test_text1.as_bytes())
        .await
        .unwrap();
    console_log!("send test_text1 done");

    p2.send_message(p1_addr.as_str(), test_text2.as_bytes())
        .await
        .unwrap();
    console_log!("send test_text2 done");

    p2.send_message(p1_addr.as_str(), test_text3.as_bytes())
        .await
        .unwrap();
    console_log!("send test_text3 done");

    p1.send_message(p2_addr.as_str(), test_text4.as_bytes())
        .await
        .unwrap();
    console_log!("send test_text4 done");

    p2.send_message(p1_addr.as_str(), test_text5.as_bytes())
        .await
        .unwrap();
    console_log!("send test_text5 done");

    fluvio_wasm_timer::Delay::new(Duration::from_secs(4))
        .await
        .unwrap();

    console_log!("check received");

    let mut msgs1 = msgs1.try_lock().unwrap().as_slice().to_vec();
    msgs1.sort();
    let mut msgs2 = msgs2.try_lock().unwrap().as_slice().to_vec();
    msgs2.sort();

    let mut expect1 = vec![
        test_text2.to_owned(),
        test_text3.to_owned(),
        test_text5.to_owned(),
    ];
    expect1.sort();

    let mut expect2 = vec![test_text1.to_owned(), test_text4.to_owned()];
    expect2.sort();
    assert_eq!(msgs1, expect1);
    assert_eq!(msgs2, expect2);

    console_log!("processor_hs_close_all_transport");
    futures::join!(close_all_transport(&p1), close_all_transport(&p2),);
}

#[wasm_bindgen_test]
async fn test_processor_connect_with_did() {
    super::setup_log();
    let p1 = prepare_processor(None).await;
    console_log!("p1 address: {}", p1.did());
    let p2 = prepare_processor(None).await;
    console_log!("p2 address: {}", p2.did());
    let p3 = prepare_processor(None).await;
    console_log!("p3 address: {}", p3.did());

    p1.swarm.clone().listen().await;
    p2.swarm.clone().listen().await;
    p3.swarm.clone().listen().await;

    console_log!("processor_connect_p1_and_p2");
    create_connection(&p1, &p2).await;
    console_log!("processor_connect_p1_and_p2, done");

    console_log!("processor_connect_p2_and_p3");
    create_connection(&p2, &p3).await;
    console_log!("processor_connect_p2_and_p3, done");

    let p1_peers = p1.list_peers().await.unwrap();
    assert!(
        p1_peers
            .iter()
            .any(|p| p.did.to_string().eq(&p2.did().into_token().to_string())),
        "p2 not in p1's peer list"
    );

    fluvio_wasm_timer::Delay::new(Duration::from_secs(2))
        .await
        .unwrap();

    console_log!("connect p1 and p3");
    // p1 create connect with p3's address
    let peer3 = p1.connect_with_did(p3.did(), true).await.unwrap();
    console_log!("processor_p1_p3_conntected");
    fluvio_wasm_timer::Delay::new(Duration::from_millis(1000))
        .await
        .unwrap();
    console_log!("processor_detect_connection_state");
    assert_eq!(
        peer3.transport.ice_connection_state().await.unwrap(),
        RtcIceConnectionState::Connected
    );

    console_log!("check peers");
    let peers = p1.list_peers().await.unwrap();
    assert!(
        peers.iter().any(|p| p
            .did
            .to_string()
            .eq(p3.did().into_token().to_string().as_str())),
        "peer list dose NOT contains p3 address"
    );
    console_log!("processor_close_all_transport");
    futures::join!(
        close_all_transport(&p1),
        close_all_transport(&p2),
        close_all_transport(&p3),
    );
}
