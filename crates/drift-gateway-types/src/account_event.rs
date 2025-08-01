use drift_rs::types::{MarketType, Order, OrderType, PositionDirection};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::PRICE_DECIMALS;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum AccountEvent {
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
    #[serde(rename_all = "camelCase")]
    Swap {
        amount_in: Decimal,
        amount_out: Decimal,
        market_in: u16,
        market_out: u16,
        ts: u64,
        tx_idx: usize,
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    Trigger { order_id: u32, oracle_price: u64 },
}

impl AccountEvent {
    pub fn fill(
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
pub enum Side {
    Buy,
    Sell,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrderWithDecimals {
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
    pub fn from_order(value: Order, decimals: u32) -> Self {
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
