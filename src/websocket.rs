use std::collections::HashMap;

use drift_sdk::{
    event_subscriber::{DriftEvent, EventSubscriber, PubsubClient},
    Wallet,
};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    task::{AbortHandle, JoinHandle},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

/// Start the websocket server
pub async fn start_ws_server(listen_address: &str, ws_endpoint: String, wallet: Wallet) {
    // Create the event loop and TCP listener we'll accept connections on.
    let listener = TcpListener::bind(&listen_address)
        .await
        .expect("failed to bind");
    info!("Ws server listening at: ws://{}", listen_address);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(accept_connection(
                stream,
                ws_endpoint.clone(),
                wallet.clone(),
            ));
        }
    });
}

async fn accept_connection(stream: TcpStream, ws_endpoint: String, wallet: Wallet) {
    let addr = stream.peer_addr().expect("peer address");
    let ws_stream = accept_async(stream).await.expect("Ws handshake");
    info!("accepted Ws connection: {}", addr);

    let (mut ws_out, mut ws_in) = ws_stream.split();
    let (message_tx, mut message_rx) = tokio::sync::mpsc::channel::<Message>(32);
    let mut stream_map = HashMap::<u8, JoinHandle<()>>::default();

    // writes messages to the connection
    tokio::spawn(async move {
        while let Some(msg) = message_rx.recv().await {
            if msg.is_close() {
                let _ = ws_out.close().await;
                break;
            } else {
                ws_out.send(msg).await.expect("sent");
            }
        }
    });

    // watches incoming messages from the connection
    while let Some(Ok(msg)) = ws_in.next().await {
        match msg {
            Message::Text(ref request) => match serde_json::from_str::<'_, WsRequest>(request) {
                Ok(request) => {
                    match request.method {
                        Method::Subscribe => {
                            // TODO: support subscribing by channels
                            if stream_map.contains_key(&request.sub_account_id) {
                                // no double subs
                                return;
                            }
                            debug!("subscribing to events for: {}", request.sub_account_id);

                            let mut event_stream = EventSubscriber::subscribe(
                                PubsubClient::new(ws_endpoint.as_str())
                                    .await
                                    .expect("ws connect"),
                                wallet.sub_account(request.sub_account_id as u16),
                            );

                            let join_handle = tokio::spawn({
                                let sub_account_id = request.sub_account_id;
                                let message_tx = message_tx.clone();
                                async move {
                                    while let Some(ref update) = event_stream.next().await {
                                        // TODO: could be helper in sdk
                                        let channel = if let DriftEvent::OrderFill { .. } = update {
                                            Channel::Fills
                                        } else {
                                            Channel::Orders
                                        };

                                        message_tx
                                            .send(Message::text(
                                                serde_json::to_string(&WsEvent {
                                                    data: update,
                                                    channel,
                                                    sub_account_id,
                                                })
                                                .expect("serializes"),
                                            ))
                                            .await
                                            .expect("capacity");
                                    }
                                }
                            });

                            stream_map.insert(request.sub_account_id, join_handle);
                        }
                        Method::Unsubscribe => {
                            debug!("unsubscribing: {}", request.sub_account_id);
                            // TODO: support ending by channel, this ends all channels
                            if let Some(task) = stream_map.remove(&request.sub_account_id) {
                                task.abort();
                            }
                        }
                    }
                }
                Err(err) => {
                    message_tx
                        .try_send(Message::text(
                            json!({
                                "error": "bad request",
                                "reason": err.to_string(),
                            })
                            .to_string(),
                        ))
                        .expect("capacity");
                }
            },
            Message::Close(frame) => {
                let _ = message_tx.send(Message::Close(frame)).await;
                break;
            }
            // tokio-tungstenite handles ping/pong transparently
            _ => (),
        }
    }
    info!("closing Ws connection: {}", addr);
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum Method {
    Subscribe,
    Unsubscribe,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum Channel {
    Fills,
    Orders,
    All,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WsRequest {
    method: Method,
    channel: Channel,
    sub_account_id: u8,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WsEvent<T: Serialize> {
    data: T,
    channel: Channel,
    sub_account_id: u8,
}

#[derive(Default)]
struct SubscriptionInfo {
    abort_handle: Option<AbortHandle>,
    channel_mask: u8,
}

impl SubscriptionInfo {
    fn new(abort_handle: AbortHandle, channel: Channel) -> Self {
        let mut this = Self {
            abort_handle: Some(abort_handle),
            channel_mask: 0,
        };
        this.sub_channel(channel);
        this
    }
    fn end(self) {
        self.abort_handle.unwrap().abort();
    }
    fn unsubscribed(&self) -> bool {
        self.channel_mask == 0
    }
    fn has_channel(&self, c: Channel) -> bool {
        (self.channel_mask & 1 << (c as u8)) > 0
    }
    fn sub_channel(&mut self, c: Channel) {
        self.channel_mask |= 1 << (c as u8);
    }
    fn unsub_channel(&mut self, c: Channel) {
        self.channel_mask &= !(1 << (c as u8));
    }
}
