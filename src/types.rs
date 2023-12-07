//! Defines types for:
//! - gateway request/responses
//! - wrappers for presenting drift program types with less implementation detail
//!
use drift_sdk::{
    constants::{PerpMarketConfig, SpotMarketConfig},
    types::{self as sdk_types, PositionDirection, PostOnlyParam},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize, Deserialize)]
pub struct Order {
    #[serde(
        rename = "type",
        serialize_with = "order_type_ser",
        deserialize_with = "order_type_de"
    )]
    order_type: sdk_types::OrderType,
    market_id: u16,
    amount: i64,
    price: u64,
    post_only: bool,
    reduce_only: bool,
    immediate_or_cancel: bool,
}

impl From<sdk_types::Order> for Order {
    fn from(value: sdk_types::Order) -> Self {
        Order {
            market_id: value.market_index,
            price: value.price,
            amount: value.base_asset_amount as i64
                * match value.direction {
                    PositionDirection::Long => 1,
                    PositionDirection::Short => -1,
                },
            immediate_or_cancel: value.immediate_or_cancel,
            reduce_only: value.reduce_only,
            order_type: value.order_type,
            post_only: value.post_only,
        }
    }
}

fn order_type_ser<S>(order_type: &sdk_types::OrderType, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let s = match order_type {
        sdk_types::OrderType::Limit => "limit",
        sdk_types::OrderType::Market => "market",
        sdk_types::OrderType::TriggerLimit => "trigger_limit",
        sdk_types::OrderType::TriggerMarket => "trigger_market",
        sdk_types::OrderType::Oracle => "oracle",
    };
    serializer.serialize_str(s)
}

fn order_type_de<'de, D>(deserializer: D) -> Result<sdk_types::OrderType, D::Error>
where
    D: Deserializer<'de>,
{
    let order_type = Deserialize::deserialize(deserializer)?;
    match order_type {
        "limit" => Ok(sdk_types::OrderType::Limit),
        "market" => Ok(sdk_types::OrderType::Market),
        "trigger_limit" => Ok(sdk_types::OrderType::TriggerLimit),
        "trigger_market" => Ok(sdk_types::OrderType::TriggerMarket),
        "oracle" => Ok(sdk_types::OrderType::Oracle),
        _ => Err(serde::de::Error::custom("invalid order type")),
    }
}

#[derive(Serialize, Deserialize)]
pub struct SpotPosition {
    amount: u64,
    #[serde(rename = "type")]
    balance_type: String, // deposit or borrow
    market_id: u16,
}

impl From<sdk_types::SpotPosition> for SpotPosition {
    fn from(value: sdk_types::SpotPosition) -> Self {
        Self {
            amount: value.scaled_balance,
            market_id: value.market_index,
            balance_type: value.balance_type.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct PerpPosition {
    amount: i64,
    average_entry: u64,
    market_id: u16,
}

impl From<sdk_types::PerpPosition> for PerpPosition {
    fn from(value: sdk_types::PerpPosition) -> Self {
        Self {
            amount: value.base_asset_amount,
            market_id: value.market_index,
            average_entry: (value.quote_entry_amount.abs() / value.base_asset_amount.abs()) as u64,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct PlaceOrdersRequest {
    orders: Vec<PlaceOrder>,
}

#[derive(Serialize, Deserialize)]
pub struct PlaceOrder {
    market_id: u16,
    amount: i64,
    price: u64,
    #[serde(
        rename = "type",
        serialize_with = "order_type_ser",
        deserialize_with = "order_type_de"
    )]
    order_type: sdk_types::OrderType,
    post_only: bool,
    reduce_only: bool,
    immediate_or_cancel: bool,
}

#[derive(Serialize, Deserialize)]
pub struct GetOrdersResponse {
    pub orders: Vec<Order>,
}

#[derive(Serialize, Deserialize)]
pub struct GetPositionsResponse {
    pub spot: Vec<SpotPosition>,
    pub perp: Vec<PerpPosition>,
}

#[derive(Serialize)]
pub struct MarketInfo {
    market_id: u16,
    symbol: &'static str,
    precision: u8,
}

impl From<SpotMarketConfig<'static>> for MarketInfo {
    fn from(value: SpotMarketConfig<'static>) -> Self {
        Self {
            market_id: value.market_index,
            symbol: value.symbol,
            precision: value.precision_exp,
        }
    }
}

impl From<PerpMarketConfig<'static>> for MarketInfo {
    fn from(value: PerpMarketConfig<'static>) -> Self {
        Self {
            market_id: value.market_index,
            symbol: value.symbol,
            precision: 6, // i.e. USDC decimals
        }
    }
}

#[derive(Serialize)]
pub struct AllMarketsResponse {
    pub spot: Vec<MarketInfo>,
    pub perp: Vec<MarketInfo>,
}
