//! Defines types for:
//! - gateway request/responses
//! - wrappers for presenting drift program types with less implementation detail
//!
use drift_sdk::{
    constants::{PerpMarketConfig, SpotMarketConfig},
    types::{self as sdk_types, MarketType, OrderParams, PositionDirection, PostOnlyParam},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize, Deserialize)]
pub struct Order {
    #[serde(serialize_with = "order_type_ser", deserialize_with = "order_type_de")]
    order_type: sdk_types::OrderType,
    market_id: u16,
    #[serde(
        serialize_with = "market_type_ser",
        deserialize_with = "market_type_de"
    )]
    market_type: MarketType,
    amount: i64,
    price: u64,
    post_only: bool,
    reduce_only: bool,
    user_order_id: u8,
    immediate_or_cancel: bool,
}

impl From<sdk_types::Order> for Order {
    fn from(value: sdk_types::Order) -> Self {
        Order {
            market_id: value.market_index,
            market_type: value.market_type,
            price: value.price,
            amount: value.base_asset_amount as i64 * (1_i64 - (2 * value.direction as i64)),
            immediate_or_cancel: value.immediate_or_cancel,
            reduce_only: value.reduce_only,
            order_type: value.order_type,
            post_only: value.post_only,
            user_order_id: value.user_order_id,
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

pub type ModifyOrdersRequest = PlaceOrdersRequest;

#[derive(Serialize, Deserialize)]
pub struct PlaceOrdersRequest {
    pub orders: Vec<PlaceOrder>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceOrder {
    market_id: u16,
    #[serde(
        serialize_with = "market_type_ser",
        deserialize_with = "market_type_de"
    )]
    market_type: sdk_types::MarketType,
    amount: i64,
    price: u64,
    /// 0 indicates it is not set (according to program)
    #[serde(default)]
    pub user_order_id: u8,
    /// only used for modify orders
    pub order_id: Option<u32>,
    #[serde(serialize_with = "order_type_ser", deserialize_with = "order_type_de")]
    order_type: sdk_types::OrderType,
    #[serde(default)]
    post_only: bool,
    #[serde(default)]
    reduce_only: bool,
    #[serde(default)]
    immediate_or_cancel: bool,
}

fn market_type_ser<S>(market_type: &sdk_types::MarketType, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let s = match market_type {
        sdk_types::MarketType::Spot => "spot",
        sdk_types::MarketType::Perp => "perp",
    };
    serializer.serialize_str(s)
}

fn market_type_de<'de, D>(deserializer: D) -> Result<sdk_types::MarketType, D::Error>
where
    D: Deserializer<'de>,
{
    let market_type = Deserialize::deserialize(deserializer)?;
    match market_type {
        "spot" => Ok(sdk_types::MarketType::Spot),
        "perp" => Ok(sdk_types::MarketType::Perp),
        _ => Err(serde::de::Error::custom("invalid market type")),
    }
}

impl From<PlaceOrder> for sdk_types::OrderParams {
    fn from(value: PlaceOrder) -> Self {
        OrderParams {
            market_index: value.market_id,
            market_type: value.market_type,
            order_type: value.order_type,
            base_asset_amount: value.amount.unsigned_abs(),
            direction: if value.amount >= 0 {
                PositionDirection::Long
            } else {
                PositionDirection::Short
            },
            price: value.price,
            immediate_or_cancel: value.immediate_or_cancel,
            reduce_only: value.reduce_only,
            post_only: if value.post_only {
                PostOnlyParam::MustPostOnly
            } else {
                PostOnlyParam::None
            },
            user_order_id: value.user_order_id,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Market {
    /// The market index
    pub id: u16,
    #[serde(
        rename = "type",
        serialize_with = "market_type_ser",
        deserialize_with = "market_type_de"
    )]
    /// The market type (Spot or Perp)
    pub market_type: MarketType,
}

impl Market {
    pub fn as_market_id(self) -> drift_sdk::types::MarketId {
        unsafe { std::mem::transmute(self) }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPositionsRequest {
    pub market: Option<Market>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOrdersRequest {
    pub market: Option<Market>,
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

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrdersRequest {
    /// Market to cancel orders
    pub market: Option<Market>,
    /// order Ids to cancel
    pub ids: Vec<u32>,
    /// user assigned order Ids to cancel
    pub user_ids: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOrderbookRequest {
    pub market: Market,
}
