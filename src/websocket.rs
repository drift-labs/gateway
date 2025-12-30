//! Websocket server

use std::{collections::HashMap, ops::Neg, sync::Arc};

use drift_gateway_types::{
    AccountEvent, FundingPaymentEvent, OrderCancelEvent, OrderCancelMissingEvent, OrderCreateEvent,
    OrderExpireEvent, OrderWithDecimals, SwapEvent,
};
use drift_rs::{
    constants::ProgramData,
    event_subscriber::{DriftEvent, EventSubscriber, PubsubClient},
    Pubkey, Wallet,
};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::{
    types::{get_market_decimals, Channel, Market, Method, WsEvent, WsRequest, PRICE_DECIMALS},
    LOG_TARGET,
};

/// Start the websocket server
pub async fn start_ws_server(
    listen_address: &str,
    ws_client: Arc<PubsubClient>,
    wallet: Arc<Wallet>,
    program_data: &'static ProgramData,
) {
    // Create the event loop and TCP listener we'll accept connections on.
    let listener = TcpListener::bind(&listen_address)
        .await
        .expect("failed to bind");
    info!("Ws server listening at: ws://{}", listen_address);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(accept_connection(
                stream,
                Arc::clone(&ws_client),
                Arc::clone(&wallet),
                program_data,
            ));
        }
    });
}

async fn accept_connection(
    stream: TcpStream,
    ws_client: Arc<PubsubClient>,
    wallet: Arc<Wallet>,
    program_data: &'static ProgramData,
) {
    let addr = stream.peer_addr().expect("peer address");
    let ws_stream = accept_async(stream).await.expect("Ws handshake");
    info!(target: LOG_TARGET, "accepted Ws connection: {}", addr);

    let (mut ws_out, mut ws_in) = ws_stream.split();
    let (message_tx, mut message_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let subscriptions = Arc::new(Mutex::new(HashMap::<u8, JoinHandle<()>>::default()));

    // writes messages to the connection
    tokio::spawn(async move {
        while let Some(msg) = message_rx.recv().await {
            if msg.is_close() {
                let _ = ws_out.send(msg).await;
                let _ = ws_out.close().await;
                debug!(target: LOG_TARGET, "closing Ws connection (send half): {}", addr);
                break;
            }
            ws_out.send(msg).await.expect("sent");
        }
    });

    // watches incoming messages from the connection
    while let Some(Ok(msg)) = ws_in.next().await {
        match msg {
            Message::Text(ref request) => match serde_json::from_str::<'_, WsRequest>(request) {
                Ok(request) => {
                    match request.method {
                        Method::Subscribe => {
                            // TODO: support subscriptions for individual channels and/or markets
                            let mut subscription_map = subscriptions.lock().await;
                            if subscription_map.contains_key(&request.sub_account_id) {
                                info!(target: LOG_TARGET, "subscription already exists for: {}", request.sub_account_id);
                                message_tx
                                    .send(Message::text(
                                        json!({
                                            "error": "bad request",
                                            "reason": "subscription already exists",
                                        })
                                        .to_string(),
                                    ))
                                    .await
                                    .unwrap();
                                continue;
                            }
                            info!(target: LOG_TARGET, "subscribing to events for: {}", request.sub_account_id);
                            let join_handle = tokio::spawn({
                                let ws_client_ref = Arc::clone(&ws_client);
                                let sub_account_address =
                                    wallet.sub_account(request.sub_account_id as u16);
                                let subscription_map = Arc::clone(&subscriptions);
                                let sub_account_id = request.sub_account_id;
                                let message_tx = message_tx.clone();

                                async move {
                                    loop {
                                        let mut event_stream = match EventSubscriber::subscribe(
                                            Arc::clone(&ws_client_ref),
                                            sub_account_address,
                                        )
                                        .await
                                        {
                                            Ok(stream) => stream,
                                            Err(err) => {
                                                log::error!(target: LOG_TARGET, "event subscribe failed: {sub_account_id:?}, {err:?}");
                                                break;
                                            }
                                        };

                                        debug!(target: LOG_TARGET, "event stream connected: {sub_account_id:?}");
                                        while let Some(ref update) = event_stream.next().await {
                                            let (channel, data) = map_drift_event_for_account(
                                                program_data,
                                                update,
                                                sub_account_address,
                                            );
                                            if data.is_none() {
                                                continue;
                                            }
                                            if message_tx
                                                .send(Message::text(
                                                    serde_json::to_string(&WsEvent {
                                                        data,
                                                        channel,
                                                        sub_account_id,
                                                    })
                                                    .expect("serializes"),
                                                ))
                                                .await
                                                .is_err()
                                            {
                                                warn!(target: LOG_TARGET, "failed sending Ws message: {}", addr);
                                                break;
                                            }
                                        }
                                    }
                                    warn!(target: LOG_TARGET, "event stream finished: {sub_account_id:?}");
                                    subscription_map.lock().await.remove(&sub_account_id);
                                    // the event subscription task has failed
                                    // close the Ws so client can handle resubscription
                                    let _ = message_tx.try_send(Message::Close(None));
                                }
                            });
                            subscription_map.insert(request.sub_account_id, join_handle);
                        }
                        Method::Unsubscribe => {
                            info!(target: LOG_TARGET, "unsubscribing events of: {}", request.sub_account_id);
                            // TODO: support ending by channel, this ends all channels
                            let mut subscription_map = subscriptions.lock().await;
                            if let Some(task) = subscription_map.remove(&request.sub_account_id) {
                                task.abort();
                            }
                        }
                    }
                }
                Err(err) => {
                    message_tx
                        .send(Message::text(
                            json!({
                                "error": "bad request",
                                "reason": err.to_string(),
                            })
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                }
            },
            Message::Close(frame) => {
                info!(target: LOG_TARGET, "received Ws close: {}", addr);
                let _ = message_tx.send(Message::Close(frame)).await;
                break;
            }
            // tokio-tungstenite handles ping/pong transparently
            _ => (),
        }
    }

    let subs = subscriptions.lock().await;
    for (_k, task) in subs.iter() {
        task.abort();
    }
    info!(target: LOG_TARGET, "closing Ws connection: {}", addr);
}

/// Map drift-program events into gateway friendly types for events to the specific UserAccount
pub(crate) fn map_drift_event_for_account(
    program_data: &ProgramData,
    event: &DriftEvent,
    sub_account_address: Pubkey,
) -> (Channel, Option<AccountEvent>) {
    match event {
        DriftEvent::OrderTrigger {
            user: _,
            order_id,
            oracle_price,
            amount: _,
        } => (
            Channel::Orders,
            Some(AccountEvent::Trigger {
                order_id: *order_id,
                oracle_price: *oracle_price,
            }),
        ),
        DriftEvent::OrderFill {
            maker,
            maker_fee,
            maker_order_id,
            maker_side,
            taker,
            taker_fee,
            taker_order_id,
            taker_side,
            base_asset_amount_filled,
            quote_asset_amount_filled,
            oracle_price,
            market_index,
            market_type,
            signature,
            tx_idx,
            ts,
            bit_flags: _,
        } => {
            let decimals =
                get_market_decimals(program_data, Market::new(*market_index, *market_type));
            let fill = if *maker == Some(sub_account_address) {
                Some(AccountEvent::fill(
                    maker_side.unwrap(),
                    *maker_fee,
                    *base_asset_amount_filled,
                    *quote_asset_amount_filled,
                    *oracle_price,
                    *maker_order_id,
                    *ts,
                    decimals,
                    signature,
                    *tx_idx,
                    *market_index,
                    *market_type,
                    (*maker).map(|x| x.to_string()),
                    Some(*maker_order_id),
                    Some(*maker_fee),
                    (*taker).map(|x| x.to_string()),
                    Some(*taker_order_id),
                    Some(*taker_fee as i64),
                ))
            } else if *taker == Some(sub_account_address) {
                Some(AccountEvent::fill(
                    taker_side.unwrap(),
                    (*taker_fee) as i64,
                    *base_asset_amount_filled,
                    *quote_asset_amount_filled,
                    *oracle_price,
                    *taker_order_id,
                    *ts,
                    decimals,
                    signature,
                    *tx_idx,
                    *market_index,
                    *market_type,
                    (*maker).map(|x| x.to_string()),
                    Some(*maker_order_id),
                    Some(*maker_fee),
                    (*taker).map(|x| x.to_string()),
                    Some(*taker_order_id),
                    Some(*taker_fee as i64),
                ))
            } else {
                None
            };

            (Channel::Fills, fill)
        }
        DriftEvent::OrderCancel {
            taker: _,
            maker,
            taker_order_id,
            maker_order_id,
            signature,
            tx_idx,
            ts,
        } => {
            let order_id = if *maker == Some(sub_account_address) {
                maker_order_id
            } else {
                taker_order_id
            };
            (
                Channel::Orders,
                Some(AccountEvent::OrderCancel(OrderCancelEvent {
                    order_id: *order_id,
                    ts: *ts,
                    signature: signature.clone(),
                    tx_idx: *tx_idx,
                })),
            )
        }
        DriftEvent::OrderCancelMissing {
            order_id,
            user_order_id,
            signature,
        } => (
            Channel::Orders,
            Some(AccountEvent::OrderCancelMissing(OrderCancelMissingEvent {
                user_order_id: *user_order_id,
                order_id: *order_id,
                signature: signature.clone(),
            })),
        ),
        DriftEvent::OrderExpire {
            order_id,
            fee,
            ts,
            signature,
            ..
        } => (
            Channel::Orders,
            Some(AccountEvent::OrderExpire(OrderExpireEvent {
                order_id: *order_id,
                fee: Decimal::new((*fee as i64).neg(), PRICE_DECIMALS),
                ts: *ts,
                signature: signature.to_string(),
            })),
        ),
        DriftEvent::OrderCreate {
            order,
            ts,
            signature,
            tx_idx,
            ..
        } => {
            let decimals = get_market_decimals(
                program_data,
                Market::new(order.market_index, order.market_type),
            );
            (
                Channel::Orders,
                Some(AccountEvent::OrderCreate(OrderCreateEvent {
                    order: OrderWithDecimals::from_order(*order, decimals),
                    ts: *ts,
                    signature: signature.clone(),
                    tx_idx: *tx_idx,
                })),
            )
        }
        DriftEvent::FundingPayment {
            amount,
            market_index,
            ts,
            tx_idx,
            signature,
            ..
        } => (
            Channel::Funding,
            Some(AccountEvent::FundingPayment(FundingPaymentEvent {
                amount: Decimal::new(*amount, PRICE_DECIMALS).normalize(),
                market_index: *market_index,
                ts: *ts,
                signature: signature.clone(),
                tx_idx: *tx_idx,
            })),
        ),
        DriftEvent::Swap {
            user: _,
            amount_in,
            amount_out,
            market_in,
            market_out,
            fee: _,
            ts,
            signature,
            tx_idx,
        } => {
            let decimals_in = get_market_decimals(program_data, Market::spot(*market_in));
            let decimals_out = get_market_decimals(program_data, Market::spot(*market_out));
            (
                Channel::Swap,
                Some(AccountEvent::Swap(SwapEvent {
                    amount_in: Decimal::new(*amount_in as i64, decimals_in),
                    amount_out: Decimal::new(*amount_out as i64, decimals_out),
                    market_in: *market_in,
                    market_out: *market_out,
                    ts: *ts,
                    tx_idx: *tx_idx,
                    signature: signature.clone(),
                })),
            )
        }
    }
}
