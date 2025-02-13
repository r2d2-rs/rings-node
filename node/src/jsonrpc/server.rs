#![warn(missing_docs)]
//! A jsonrpc-server of rings-node
/// [JSON-RPC]: https://www.jsonrpc.org/specification
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

#[cfg(feature = "browser")]
use futures::channel::mpsc::Receiver;
use futures::future::join_all;
#[cfg(feature = "browser")]
use futures::lock::Mutex;
use serde_json::Value;
#[cfg(feature = "node")]
use tokio::sync::broadcast::Receiver;
#[cfg(feature = "node")]
use tokio::sync::Mutex;

use crate::backend::types::BackendMessage;
use crate::backend::MessageType;
use crate::error::Error as ServerError;
use crate::prelude::jsonrpc_core::Error;
use crate::prelude::jsonrpc_core::ErrorCode;
use crate::prelude::jsonrpc_core::Params;
use crate::prelude::jsonrpc_core::Result;
use crate::prelude::rings_core::dht::Did;
use crate::prelude::rings_core::message::Decoder;
use crate::prelude::rings_core::message::Encoded;
use crate::prelude::rings_core::message::Encoder;
use crate::prelude::rings_core::message::Message;
use crate::prelude::rings_core::message::MessagePayload;
use crate::prelude::rings_core::prelude::vnode::VirtualNode;
use crate::prelude::rings_core::transports::manager::TransportHandshake;
use crate::prelude::rings_core::transports::manager::TransportManager;
use crate::prelude::rings_core::types::ice_transport::IceTransportInterface;
use crate::prelude::rings_core::utils::from_rtc_ice_connection_state;
use crate::prelude::rings_rpc;
use crate::prelude::rings_rpc::response;
use crate::prelude::rings_rpc::response::Peer;
use crate::prelude::rings_rpc::types::HttpRequest;
use crate::processor;
use crate::processor::Processor;
use crate::seed::Seed;

/// RpcMeta basic info struct
/// * processor: contain `swarm` instance and `stabilization` instance.
/// * is_auth: is_auth set true after verify.
#[derive(Clone)]
pub struct RpcMeta {
    processor: Arc<Processor>,
    #[allow(dead_code)]
    pub(crate) receiver: Option<Arc<Mutex<Receiver<BackendMessage>>>>,
    /// if is_auth set to true, rpc server of *native node* will check signature from
    /// HEAD['X-SIGNATURE']
    is_auth: bool,
}

impl RpcMeta {
    fn require_authed(&self) -> Result<()> {
        if !self.is_auth {
            return Err(Error::from(ServerError::NoPermission));
        }
        Ok(())
    }
}

impl From<(Arc<Processor>, Arc<Mutex<Receiver<BackendMessage>>>, bool)> for RpcMeta {
    fn from(
        (processor, receiver, is_auth): (
            Arc<Processor>,
            Arc<Mutex<Receiver<BackendMessage>>>,
            bool,
        ),
    ) -> Self {
        Self {
            processor,
            receiver: Some(receiver),
            is_auth,
        }
    }
}

impl From<(Arc<Processor>, bool)> for RpcMeta {
    fn from((processor, is_auth): (Arc<Processor>, bool)) -> Self {
        Self {
            processor,
            receiver: None,
            is_auth,
        }
    }
}

impl From<Arc<Processor>> for RpcMeta {
    fn from(processor: Arc<Processor>) -> Self {
        Self {
            processor,
            receiver: None,
            is_auth: true,
        }
    }
}

pub(crate) async fn node_info(_: Params, meta: RpcMeta) -> Result<Value> {
    let node_info = meta
        .processor
        .get_node_info()
        .await
        .map_err(|_| Error::new(ErrorCode::InternalError))?;
    serde_json::to_value(node_info).map_err(|_| Error::new(ErrorCode::ParseError))
}

/// Connect Peer VIA http
pub(crate) async fn connect_peer_via_http(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let p: Vec<String> = params.parse()?;
    let peer_url = p
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let peer = meta
        .processor
        .connect_peer_via_http(peer_url)
        .await
        .map_err(Error::from)?;
    Ok(Value::String(peer.transport.id.to_string()))
}

/// Connect Peer with seed
pub(crate) async fn connect_with_seed(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let p: Vec<Seed> = params.parse()?;
    let seed = p
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let mut connected_addresses: HashSet<Did> = HashSet::from_iter(meta.processor.swarm.get_dids());
    connected_addresses.insert(meta.processor.swarm.did());

    let tasks = seed
        .peers
        .iter()
        .filter(|&x| !connected_addresses.contains(&x.did))
        .map(|x| meta.processor.connect_peer_via_http(&x.endpoint));

    let results = join_all(tasks).await;

    let first_err = results.into_iter().find(|x| x.is_err());
    if let Some(err) = first_err {
        err.map_err(Error::from)?;
    }

    Ok(Value::Null)
}

/// Handle Connect with DID
pub(crate) async fn connect_with_did(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let p: Vec<String> = params.parse()?;
    let address_str = p
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    meta.processor
        .connect_with_did(
            Did::from_str(address_str).map_err(|_| Error::new(ErrorCode::InvalidParams))?,
            true,
        )
        .await
        .map_err(Error::from)?;
    Ok(Value::Null)
}

/// Handle create offer
pub(crate) async fn create_offer(_params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let (_, offer_payload) = meta
        .processor
        .swarm
        .create_offer()
        .await
        .map_err(ServerError::CreateOffer)
        .map_err(Error::from)?;

    let encoded = offer_payload
        .encode()
        .map_err(|_| ServerError::EncodeError)?;
    serde_json::to_value(encoded)
        .map_err(ServerError::SerdeJsonError)
        .map_err(Error::from)
}

/// Handle Answer Offer
pub(crate) async fn answer_offer(params: Params, meta: RpcMeta) -> Result<Value> {
    let p: Vec<String> = params.parse()?;
    let offer_payload_str = p
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let encoded: Encoded = <Encoded as From<&str>>::from(offer_payload_str);
    let offer_payload =
        MessagePayload::<Message>::from_encoded(&encoded).map_err(|_| ServerError::DecodeError)?;

    let (_, answer_payload) = meta
        .processor
        .swarm
        .answer_offer(offer_payload)
        .await
        .map_err(ServerError::AnswerOffer)
        .map_err(Error::from)?;

    tracing::debug!("connect_peer_via_ice response: {:?}", answer_payload);
    let encoded = answer_payload
        .encode()
        .map_err(|_| ServerError::EncodeError)?;
    serde_json::to_value(encoded)
        .map_err(ServerError::SerdeJsonError)
        .map_err(Error::from)
}

/// Handle accept answer
pub(crate) async fn accept_answer(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;

    let p: Vec<String> = params.parse()?;
    let answer_payload_str = p
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let encoded: Encoded = <Encoded as From<&str>>::from(answer_payload_str);
    let answer_payload =
        MessagePayload::<Message>::from_encoded(&encoded).map_err(|_| ServerError::DecodeError)?;
    let p: processor::Peer = meta
        .processor
        .swarm
        .accept_answer(answer_payload)
        .await
        .map_err(ServerError::AcceptAnswer)
        .map_err(Error::from)?
        .into();

    let state = p.transport.ice_connection_state().await;
    let r: Peer = p.into_response_peer(state.map(from_rtc_ice_connection_state));
    r.to_json_obj()
        .map_err(|_| ServerError::EncodeError)
        .map_err(Error::from)
}

/// Handle list peers
pub(crate) async fn list_peers(_params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let peers = meta.processor.list_peers().await?;
    let states_async = peers
        .iter()
        .map(|x| x.transport.ice_connection_state())
        .collect::<Vec<_>>();
    let states = futures::future::join_all(states_async).await;
    let r: Vec<Peer> = peers
        .iter()
        .zip(states.iter())
        .map(|(x, y)| x.into_response_peer(y.map(from_rtc_ice_connection_state)))
        .collect::<Vec<_>>();
    serde_json::to_value(r).map_err(|_| Error::from(ServerError::EncodeError))
}

/// Handle close connection
pub(crate) async fn close_connection(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<String> = params.parse()?;
    let did = params
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let did = Did::from_str(did).map_err(|_| Error::from(ServerError::InvalidDid))?;
    meta.processor.disconnect(did).await?;
    Ok(serde_json::json!({}))
}

/// Handle list pendings
pub(crate) async fn list_pendings(_params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let transports = meta.processor.list_pendings().await?;
    let states_async = transports
        .iter()
        .map(|x| x.ice_connection_state())
        .collect::<Vec<_>>();
    let states = futures::future::join_all(states_async).await;
    let r: Vec<response::TransportInfo> = transports
        .iter()
        .zip(states.iter())
        .map(|(x, y)| response::TransportInfo::from((x, y.map(from_rtc_ice_connection_state))))
        .collect::<Vec<_>>();
    serde_json::to_value(r).map_err(|_| Error::from(ServerError::EncodeError))
}

/// Handle close pending transport
pub(crate) async fn close_pending_transport(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<String> = params.parse()?;
    let transport_id = params
        .first()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    meta.processor
        .close_pending_transport(transport_id.as_str())
        .await?;
    Ok(serde_json::json!({}))
}

/// Handle send message
pub(crate) async fn send_raw_message(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: serde_json::Map<String, Value> = params.parse()?;
    let destination = params
        .get("destination")
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let text = params
        .get("text")
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let tx_id = meta
        .processor
        .send_message(destination, text.as_bytes())
        .await?;
    Ok(
        serde_json::to_value(rings_rpc::response::SendMessageResponse::from(
            tx_id.to_string(),
        ))
        .unwrap(),
    )
}

/// send custom message to specifice destination
/// * Params
///   - destination:  destination did
///   - message_type: u16
///   - data: base64 of [u8]
pub(crate) async fn send_custom_message(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let destination = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let message_type: u16 = params
        .get(1)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_u64()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .try_into()
        .map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    let data = params
        .get(2)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let data = base64::decode(data).map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    let msg: BackendMessage = BackendMessage::from((message_type, data.as_ref()));
    let msg: Vec<u8> = msg.into();
    let tx_id = meta.processor.send_message(destination, &msg).await?;

    Ok(
        serde_json::to_value(rings_rpc::response::SendMessageResponse::from(
            tx_id.to_string(),
        ))
        .unwrap(),
    )
}

pub(crate) async fn send_simple_text_message(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let destination = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let text = params
        .get(1)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let msg: BackendMessage =
        BackendMessage::from((MessageType::SimpleText.into(), text.as_bytes()));
    let msg: Vec<u8> = msg.into();
    // TODO chunk message flag
    let tx_id = meta.processor.send_message(destination, &msg).await?;

    Ok(
        serde_json::to_value(rings_rpc::response::SendMessageResponse::from(
            tx_id.to_string(),
        ))
        .unwrap(),
    )
}

/// handle send http request message
pub(crate) async fn send_http_request_message(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let destination = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let p2 = params
        .get(1)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .to_owned();
    let http_request: HttpRequest =
        serde_json::from_value(p2).map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    let msg: BackendMessage = (MessageType::HttpRequest, &http_request).try_into()?;
    let msg: Vec<u8> = msg.into();
    // TODO chunk message flag
    let tx_id = meta.processor.send_message(destination, &msg).await?;

    Ok(
        serde_json::to_value(rings_rpc::response::SendMessageResponse::from(
            tx_id.to_string(),
        ))
        .unwrap(),
    )
}

pub(crate) async fn publish_message_to_topic(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let topic = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let data = params
        .get(1)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .to_string()
        .encode()
        .map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    meta.processor.storage_append_data(topic, data).await?;

    Ok(serde_json::json!({}))
}

pub(crate) async fn fetch_messages_of_topic(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let topic = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    let index = params
        .get(1)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_i64()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let vid = VirtualNode::gen_did(topic).map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    meta.processor.storage_fetch(vid).await?;
    let result = meta.processor.storage_check_cache(vid).await;

    if let Some(vnode) = result {
        let messages = vnode
            .data
            .iter()
            .skip(index as usize)
            .map(|v| v.decode())
            .filter_map(|v| v.ok())
            .collect::<Vec<String>>();
        Ok(serde_json::json!(messages))
    } else {
        Ok(serde_json::json!(Vec::<String>::new()))
    }
}

pub(crate) async fn register_service(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let name = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;
    meta.processor.register_service(name).await?;
    Ok(serde_json::json!({}))
}

pub(crate) async fn lookup_service(params: Params, meta: RpcMeta) -> Result<Value> {
    meta.require_authed()?;
    let params: Vec<serde_json::Value> = params.parse()?;
    let name = params
        .get(0)
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidParams))?;

    let rid = VirtualNode::gen_did(name).map_err(|_| Error::new(ErrorCode::InvalidParams))?;

    meta.processor.storage_fetch(rid).await?;
    let result = meta.processor.storage_check_cache(rid).await;

    if let Some(vnode) = result {
        let dids = vnode
            .data
            .iter()
            .map(|v| v.decode())
            .filter_map(|v| v.ok())
            .collect::<Vec<String>>();
        Ok(serde_json::json!(dids))
    } else {
        Ok(serde_json::json!(Vec::<String>::new()))
    }
}

#[cfg(feature = "node")]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jsonrpc_core::types::params::Params;

    use super::*;
    use crate::prelude::*;
    use crate::tests::native::prepare_processor;

    async fn new_rnd_meta() -> RpcMeta {
        let (processor, _) = prepare_processor(None).await;
        Arc::new(processor).into()
    }

    #[tokio::test]
    async fn test_maually_handshake() {
        let meta1 = new_rnd_meta().await;
        let meta2 = new_rnd_meta().await;
        let offer = create_offer(Params::None, meta1.clone()).await.unwrap();
        let answer = answer_offer(Params::Array(vec![offer]), meta2)
            .await
            .unwrap();
        accept_answer(Params::Array(vec![answer]), meta1)
            .await
            .unwrap();
    }
}
