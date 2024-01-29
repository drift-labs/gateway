//! Websocket server

use std::ops::Neg;

use drift_sdk::{
    async_utils::retry_policy::{self},
    constants::ProgramData,
    event_subscriber::{DriftEvent, EventSubscriber, PubsubClient, RpcClient},
    types::{MarketType, Order, OrderType, PositionDirection},
    Pubkey, Wallet,
};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::types::{get_market_decimals, Market, PRICE_DECIMALS};

/// Start the websocket server
pub async fn start_ws_server(
    listen_address: &str,
    ws_endpoint: String,
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
                ws_endpoint.clone(),
                wallet.clone(),
                program_data,
            ));
        }
    });
}

async fn accept_connection(
    stream: TcpStream,
    ws_endpoint: String,
    wallet: Wallet,
    program_data: &'static ProgramData,
) {
    let addr = stream.peer_addr().expect("peer address");
    let ws_stream = accept_async(stream).await.expect("Ws handshake");
    info!("accepted Ws connection: {}", addr);

    let (mut ws_out, mut ws_in) = ws_stream.split();
    let (message_tx, mut message_rx) = tokio::sync::mpsc::channel::<Message>(32);
    let mut stream_handle: Option<JoinHandle<()>> = None;

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
                            // TODO: support subscriptions for individual channels and/or markets
                            if stream_handle.is_some() {
                                // no double subs
                                return;
                            }
                            debug!("subscribing to events for: {}", request.sub_account_id);

                            let sub_account_address =
                                wallet.sub_account(request.sub_account_id as u16);
                            let mut event_stream = EventSubscriber::subscribe(
                                PubsubClient::new(ws_endpoint.as_str())
                                    .await
                                    .expect("ws connect"),
                                RpcClient::new(ws_endpoint.replace("ws", "http")),
                                sub_account_address,
                                retry_policy::exponential_backoff(3),
                            );

                            let join_handle = tokio::spawn({
                                let sub_account_id = request.sub_account_id;
                                let message_tx = message_tx.clone();
                                async move {
                                    while let Some(ref update) = event_stream.next().await {
                                        let (channel, data) = map_drift_event(
                                            program_data,
                                            update,
                                            sub_account_address,
                                        );
                                        message_tx
                                            .send(Message::text(
                                                serde_json::to_string(&WsEvent {
                                                    data,
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

                            stream_handle = Some(join_handle);
                        }
                        Method::Unsubscribe => {
                            debug!("unsubscribing: {}", request.sub_account_id);
                            // TODO: support ending by channel, this ends all channels
                            if let Some(task) = stream_handle.take() {
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

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum AccountEvent {
    #[serde(rename_all = "camelCase")]
    Fill {
        side: Side,
        fee: Decimal,
        amount: Decimal,
        price: Decimal,
        order_id: u32,
        ts: u64,
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    OrderCreate {
        order: OrderWithDecimals,
        ts: u64,
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    OrderCancel {
        order_id: u32,
        ts: u64,
        signature: String,
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
}

impl AccountEvent {
    fn fill(
        side: PositionDirection,
        fee: i64,
        base_amount: u64,
        quote_amount: u64,
        order_id: u32,
        ts: u64,
        decimals: u32,
        signature: &String,
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
            fee: Decimal::new(fee, PRICE_DECIMALS).normalize(),
            order_id,
            amount: base_amount.normalize(),
            ts,
            signature: signature.to_string(),
        }
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Side {
    Buy,
    Sell,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OrderWithDecimals {
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
    #[serde(serialize_with = "ser_order_type")]
    pub order_type: OrderType,
    /// Whether market is spot or perp
    #[serde(serialize_with = "ser_market_type")]
    pub market_type: MarketType,
    /// User generated order id. Can make it easier to place/cancel orders
    pub user_order_id: u8,
    #[serde(serialize_with = "ser_position_direction")]
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

fn ser_position_direction<S>(x: &PositionDirection, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(match x {
        PositionDirection::Long => "buy",
        PositionDirection::Short => "sell",
    })
}

fn ser_market_type<S>(x: &MarketType, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(match x {
        MarketType::Perp => "perp",
        MarketType::Spot => "spot",
    })
}

// TODO: lookup market decimals...
/// Map drift-program events into gateway friendly types
fn map_drift_event(
    program_data: &ProgramData,
    event: &DriftEvent,
    sub_account_address: Pubkey,
) -> (Channel, AccountEvent) {
    match event {
        DriftEvent::OrderFill {
            maker,
            maker_fee,
            maker_order_id,
            maker_side,
            taker: _,
            taker_fee,
            taker_order_id,
            taker_side,
            base_asset_amount_filled,
            quote_asset_amount_filled,
            oracle_price: _,
            market_index,
            market_type,
            signature,
            ts,
        } => {
            let decimals =
                get_market_decimals(program_data, Market::new(*market_index, *market_type));
            let fill = if *maker == Some(sub_account_address) {
                AccountEvent::fill(
                    maker_side.unwrap(),
                    *maker_fee,
                    *base_asset_amount_filled,
                    *quote_asset_amount_filled,
                    *maker_order_id,
                    *ts,
                    decimals,
                    signature,
                )
            } else {
                AccountEvent::fill(
                    taker_side.unwrap(),
                    (*taker_fee) as i64,
                    *base_asset_amount_filled,
                    *quote_asset_amount_filled,
                    *taker_order_id,
                    *ts,
                    decimals,
                    signature,
                )
            };

            (Channel::Fills, fill)
        }
        DriftEvent::OrderCancel {
            taker: _,
            maker,
            taker_order_id,
            maker_order_id,
            signature,
            ts,
        } => {
            let order_id = if *maker == Some(sub_account_address) {
                maker_order_id
            } else {
                taker_order_id
            };
            (
                Channel::Orders,
                AccountEvent::OrderCancel {
                    order_id: *order_id,
                    ts: *ts,
                    signature: signature.to_string(),
                },
            )
        }
        DriftEvent::OrderCancelMissing {
            order_id,
            user_order_id,
            signature,
        } => (
            Channel::Orders,
            AccountEvent::OrderCancelMissing {
                user_order_id: *user_order_id,
                order_id: *order_id,
                signature: signature.to_string(),
            },
        ),
        DriftEvent::OrderExpire {
            order_id,
            fee,
            ts,
            signature,
        } => (
            Channel::Orders,
            AccountEvent::OrderExpire {
                order_id: *order_id,
                fee: Decimal::new((*fee as i64).neg(), PRICE_DECIMALS),
                ts: *ts,
                signature: signature.to_string(),
            },
        ),
        DriftEvent::OrderCreate {
            order,
            ts,
            signature,
        } => {
            let decimals = get_market_decimals(
                program_data,
                Market::new(order.market_index, order.market_type),
            );
            (
                Channel::Orders,
                AccountEvent::OrderCreate {
                    order: OrderWithDecimals::from_order(*order, decimals),
                    ts: *ts,
                    signature: signature.to_string(),
                },
            )
        }
    }
}
