//! Websocket server

use std::{collections::HashMap, ops::Neg, sync::Arc};

use drift_rs::{
    constants::ProgramData,
    event_subscriber::{DriftEvent, EventSubscriber, PubsubClient},
    types::{MarketType, Order, OrderType, PositionDirection},
    Pubkey, Wallet,
};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::{
    types::{get_market_decimals, Market, PRICE_DECIMALS},
    LOG_TARGET,
};

/// Start the websocket server
pub async fn start_ws_server(
    listen_address: &str,
    ws_client: Arc<PubsubClient>,
    wallet: Wallet,
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
                wallet.clone(),
                program_data,
            ));
        }
    });
}

async fn accept_connection(
    stream: TcpStream,
    ws_client: Arc<PubsubClient>,
    wallet: Wallet,
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

                            let sub_account_address =
                                wallet.sub_account(request.sub_account_id as u16);
                            let mut event_stream = EventSubscriber::subscribe(
                                Arc::clone(&ws_client),
                                sub_account_address,
                            )
                            .await
                            .expect("ws connects");

                            let join_handle = tokio::spawn({
                                let subscription_map = Arc::clone(&subscriptions);
                                let sub_account_id = request.sub_account_id;
                                let message_tx = message_tx.clone();

                                async move {
                                    loop {
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
                                        warn!(target: LOG_TARGET, "event stream finished: {sub_account_id:?}, reconnecting...");
                                        subscription_map.lock().await.remove(&sub_account_id);
                                    }
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
    info!(target: LOG_TARGET, "closing Ws connection: {}", addr);
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum Method {
    Subscribe,
    Unsubscribe,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Channel {
    Fills,
    Orders,
    Funding,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WsRequest {
    method: Method,
    sub_account_id: u8,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WsEvent<T: Serialize> {
    data: T,
    channel: Channel,
    sub_account_id: u8,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AccountEvent {
    #[serde(rename_all = "camelCase")]
    Fill {
        side: Side,
        fee: Decimal,
        amount: Decimal,
        price: Decimal,
        oracle_price: Decimal,
        order_id: u32,
        market_index: u16,
        #[serde(
            serialize_with = "crate::types::ser_market_type",
            deserialize_with = "crate::types::de_market_type"
        )]
        market_type: MarketType,
        ts: u64,

        /// The index of the event in the transaction
        tx_idx: usize,
        signature: String,

        maker: Option<String>,
        maker_order_id: Option<u32>,
        maker_fee: Option<Decimal>,
        taker: Option<String>,
        taker_order_id: Option<u32>,
        taker_fee: Option<Decimal>,
    },
    #[serde(rename_all = "camelCase")]
    OrderCreate {
        order: OrderWithDecimals,
        ts: u64,
        signature: String,
        tx_idx: usize,
    },
    #[serde(rename_all = "camelCase")]
    OrderCancel {
        order_id: u32,
        ts: u64,
        signature: String,
        tx_idx: usize,
    },
    #[serde(rename_all = "camelCase")]
    OrderCancelMissing {
        user_order_id: u8,
        order_id: u32,
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    OrderExpire {
        order_id: u32,
        fee: Decimal,
        ts: u64,
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    FundingPayment {
        amount: Decimal,
        market_index: u16,
        ts: u64,
        signature: String,
        tx_idx: usize,
    },
}

impl AccountEvent {
    fn fill(
        side: PositionDirection,
        fee: i64,
        base_amount: u64,
        quote_amount: u64,
        oracle_price: i64,
        order_id: u32,
        ts: u64,
        decimals: u32,
        signature: &String,
        tx_idx: usize,
        market_index: u16,
        market_type: MarketType,
        maker: Option<String>,
        maker_order_id: Option<u32>,
        maker_fee: Option<i64>,
        taker: Option<String>,
        taker_order_id: Option<u32>,
        taker_fee: Option<i64>,
    ) -> Self {
        let base_amount = Decimal::new(base_amount as i64, decimals);
        let price = Decimal::new(quote_amount as i64, PRICE_DECIMALS) / base_amount;
        AccountEvent::Fill {
            side: if let PositionDirection::Long = side {
                Side::Buy
            } else {
                Side::Sell
            },
            price: price.normalize(),
            oracle_price: Decimal::new(oracle_price, PRICE_DECIMALS).normalize(),
            fee: Decimal::new(fee, PRICE_DECIMALS).normalize(),
            order_id,
            amount: base_amount.normalize(),
            ts,
            signature: signature.to_string(),
            market_index,
            market_type,
            tx_idx,
            maker,
            maker_order_id,
            maker_fee: maker_fee.map(|x| Decimal::new(x, PRICE_DECIMALS)),
            taker,
            taker_order_id,
            taker_fee: taker_fee.map(|x| Decimal::new(x, PRICE_DECIMALS)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Side {
    Buy,
    Sell,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrderWithDecimals {
    /// The slot the order was placed
    pub slot: u64,
    /// The limit price for the order (can be 0 for market orders)
    /// For orders with an auction, this price isn't used until the auction is complete
    pub price: Decimal,
    /// The size of the order
    pub amount: Decimal,
    /// The amount of the order filled
    pub filled: Decimal,
    /// At what price the order will be triggered. Only relevant for trigger orders
    pub trigger_price: Decimal,
    /// The start price for the auction. Only relevant for market/oracle orders
    pub auction_start_price: Decimal,
    /// The end price for the auction. Only relevant for market/oracle orders
    pub auction_end_price: Decimal,
    /// The time when the order will expire
    pub max_ts: i64,
    /// If set, the order limit price is the oracle price + this offset
    pub oracle_price_offset: Decimal,
    /// The id for the order. Each users has their own order id space
    pub order_id: u32,
    /// The perp/spot market index
    pub market_index: u16,
    /// The type of order
    #[serde(serialize_with = "ser_order_type", deserialize_with = "de_order_type")]
    pub order_type: OrderType,
    /// Whether market is spot or perp
    #[serde(
        serialize_with = "crate::types::ser_market_type",
        deserialize_with = "crate::types::de_market_type"
    )]
    pub market_type: MarketType,
    /// User generated order id. Can make it easier to place/cancel orders
    pub user_order_id: u8,
    #[serde(
        serialize_with = "ser_position_direction",
        deserialize_with = "de_position_direction"
    )]
    pub direction: PositionDirection,
    /// Whether the order is allowed to only reduce position size
    pub reduce_only: bool,
    /// Whether the order must be a maker
    pub post_only: bool,
    /// Whether the order must be canceled the same slot it is placed
    pub immediate_or_cancel: bool,
    /// How many slots the auction lasts
    pub auction_duration: u8,
}

impl OrderWithDecimals {
    fn from_order(value: Order, decimals: u32) -> Self {
        Self {
            slot: value.slot,
            price: Decimal::new(value.price as i64, PRICE_DECIMALS).normalize(),
            amount: Decimal::new(value.base_asset_amount as i64, decimals).normalize(),
            filled: Decimal::new(value.base_asset_amount_filled as i64, decimals).normalize(),
            trigger_price: Decimal::new(value.trigger_price as i64, PRICE_DECIMALS).normalize(),
            auction_start_price: Decimal::new(value.auction_start_price, PRICE_DECIMALS)
                .normalize(),
            auction_end_price: Decimal::new(value.auction_end_price, PRICE_DECIMALS).normalize(),
            oracle_price_offset: Decimal::new(value.oracle_price_offset as i64, PRICE_DECIMALS)
                .normalize(),
            max_ts: value.max_ts,
            order_id: value.order_id,
            market_index: value.market_index,
            order_type: value.order_type,
            market_type: value.market_type,
            user_order_id: value.user_order_id,
            direction: value.direction,
            reduce_only: value.reduce_only,
            post_only: value.post_only,
            immediate_or_cancel: value.immediate_or_cancel,
            auction_duration: value.auction_duration,
        }
    }
}

fn ser_order_type<S>(x: &OrderType, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(match x {
        OrderType::Limit => "limit",
        OrderType::Market => "market",
        OrderType::Oracle => "oracle",
        OrderType::TriggerLimit => "triggerLimit",
        OrderType::TriggerMarket => "triggerMarket",
    })
}

fn de_order_type<'de, D>(deserializer: D) -> Result<OrderType, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "limit" => Ok(OrderType::Limit),
        "market" => Ok(OrderType::Market),
        "oracle" => Ok(OrderType::Oracle),
        "triggerLimit" => Ok(OrderType::TriggerLimit),
        "triggerMarket" => Ok(OrderType::TriggerMarket),
        _ => Err(serde::de::Error::custom(format!(
            "unknown order type: {}",
            s
        ))),
    }
}

fn ser_position_direction<S>(x: &PositionDirection, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(match x {
        PositionDirection::Long => "buy",
        PositionDirection::Short => "sell",
    })
}

fn de_position_direction<'de, D>(deserializer: D) -> Result<PositionDirection, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "buy" => Ok(PositionDirection::Long),
        "sell" => Ok(PositionDirection::Short),
        _ => Err(serde::de::Error::custom(format!(
            "unknown position direction: {}",
            s
        ))),
    }
}

/// Map drift-program events into gateway friendly types for events to the specific UserAccount
pub(crate) fn map_drift_event_for_account(
    program_data: &ProgramData,
    event: &DriftEvent,
    sub_account_address: Pubkey,
) -> (Channel, Option<AccountEvent>) {
    match event {
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
                Some(AccountEvent::OrderCancel {
                    order_id: *order_id,
                    ts: *ts,
                    signature: signature.to_string(),
                    tx_idx: *tx_idx,
                }),
            )
        }
        DriftEvent::OrderCancelMissing {
            order_id,
            user_order_id,
            signature,
        } => (
            Channel::Orders,
            Some(AccountEvent::OrderCancelMissing {
                user_order_id: *user_order_id,
                order_id: *order_id,
                signature: signature.to_string(),
            }),
        ),
        DriftEvent::OrderExpire {
            order_id,
            fee,
            ts,
            signature,
            ..
        } => (
            Channel::Orders,
            Some(AccountEvent::OrderExpire {
                order_id: *order_id,
                fee: Decimal::new((*fee as i64).neg(), PRICE_DECIMALS),
                ts: *ts,
                signature: signature.to_string(),
            }),
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
                Some(AccountEvent::OrderCreate {
                    order: OrderWithDecimals::from_order(*order, decimals),
                    ts: *ts,
                    signature: signature.to_string(),
                    tx_idx: *tx_idx,
                }),
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
            Some(AccountEvent::FundingPayment {
                amount: Decimal::new(*amount, PRICE_DECIMALS).normalize(),
                market_index: *market_index,
                ts: *ts,
                signature: signature.to_string(),
                tx_idx: *tx_idx,
            }),
        ),
    }
}
