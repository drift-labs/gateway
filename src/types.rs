//! Defines types for:
//! - gateway request/responses
//! - wrappers for presenting drift program types with less implementation detail
//!
use drift_sdk::{
    constants::{
        spot_market_config_by_index, PerpMarketConfig, SpotMarketConfig, BASE_PRECISION,
        PRICE_PRECISION,
    },
    types::{
        self as sdk_types, Context, MarketType, OrderParams, PositionDirection, PostOnlyParam,
    },
};
use rust_decimal::Decimal;
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
    amount: Decimal,
    filled: Decimal,
    price: Decimal,
    post_only: bool,
    reduce_only: bool,
    user_order_id: u8,
    order_id: u32,
    immediate_or_cancel: bool,
}

impl Order {
    pub fn from_sdk_order(value: sdk_types::Order, context: Context) -> Self {
        let precision = if let MarketType::Perp = value.market_type {
            BASE_PRECISION.ilog10()
        } else {
            let config =
                spot_market_config_by_index(context, value.market_index).expect("market exists");
            config.precision_exp as u32
        };

        Order {
            market_id: value.market_index,
            market_type: value.market_type,
            price: Decimal::new(value.price as i64, PRICE_PRECISION.ilog10()),
            amount: Decimal::new(value.base_asset_amount as i64, precision),
            filled: Decimal::new(value.base_asset_amount_filled as i64, precision),
            immediate_or_cancel: value.immediate_or_cancel,
            reduce_only: value.reduce_only,
            order_type: value.order_type,
            order_id: value.order_id,
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
    amount: Decimal,
    #[serde(rename = "type")]
    balance_type: String, // deposit or borrow
    market_id: u16,
}

impl SpotPosition {
    pub fn from_sdk_type(
        value: &sdk_types::SpotPosition,
        spot_market: &sdk_types::SpotMarket,
    ) -> Self {
        // TODO: handle error
        let token_amount = value.get_token_amount(spot_market).expect("ok");
        Self {
            amount: Decimal::from_i128_with_scale(token_amount as i128, spot_market.decimals),
            market_id: value.market_index,
            balance_type: if value.balance_type == Default::default() {
                "deposit".into()
            } else {
                "borrow".into()
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct PerpPosition {
    amount: Decimal,
    average_entry: Decimal,
    market_id: u16,
}

impl From<sdk_types::PerpPosition> for PerpPosition {
    fn from(value: sdk_types::PerpPosition) -> Self {
        let amount = Decimal::new(value.base_asset_amount, BASE_PRECISION.ilog10());
        Self {
            amount,
            market_id: value.market_index,
            average_entry: Decimal::new(
                value.quote_entry_amount.abs() / value.base_asset_amount.abs().max(1),
                PRICE_PRECISION.ilog10(),
            ),
        }
    }
}

pub type ModifyOrdersRequest = PlaceOrdersRequest;

#[derive(Serialize, Deserialize, Debug)]
pub struct PlaceOrdersRequest {
    pub orders: Vec<PlaceOrder>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlaceOrder {
    market_id: u16,
    #[serde(
        serialize_with = "market_type_ser",
        deserialize_with = "market_type_de"
    )]
    market_type: sdk_types::MarketType,
    amount: Decimal,
    price: Decimal,
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

#[inline]
/// Convert decimal to unsigned fixed-point representation with `target` precision
fn scale_decimal_to_u64(x: Decimal, target: u32) -> u64 {
    ((x.mantissa().unsigned_abs() * target as u128) / 10_u128.pow(x.scale())) as u64
}

impl PlaceOrder {
    pub fn to_order_params(self, context: Context) -> OrderParams {
        let target_scale = if let MarketType::Perp = self.market_type {
            BASE_PRECISION as u32
        } else {
            let config =
                spot_market_config_by_index(context, self.market_id).expect("market exists");
            config.precision as u32
        };
        let base_amount = scale_decimal_to_u64(self.amount, target_scale);
        let price = scale_decimal_to_u64(self.price, PRICE_PRECISION as u32);

        OrderParams {
            market_index: self.market_id,
            market_type: self.market_type,
            order_type: self.order_type,
            base_asset_amount: base_amount,
            direction: if self.amount.is_sign_negative() {
                PositionDirection::Short
            } else {
                PositionDirection::Long
            },
            price,
            immediate_or_cancel: self.immediate_or_cancel,
            reduce_only: self.reduce_only,
            post_only: if self.post_only {
                PostOnlyParam::TryPostOnly
            } else {
                PostOnlyParam::None
            },
            user_order_id: self.user_order_id,
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

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetPositionsRequest {
    #[serde(default)]
    pub market: Option<Market>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetOrdersRequest {
    #[serde(default)]
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

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrdersRequest {
    /// Market to cancel orders
    #[serde(default)]
    pub market: Option<Market>,
    /// order Ids to cancel
    #[serde(default)]
    pub ids: Vec<u32>,
    /// user assigned order Ids to cancel
    #[serde(default)]
    pub user_ids: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOrderbookRequest {
    pub market: Market,
}

#[cfg(test)]
mod tests {
    use drift_sdk::types::{Context, MarketType};
    use std::str::FromStr;

    use crate::types::Order;

    use super::{Decimal, PlaceOrder};

    #[test]
    fn place_order_to_order() {
        let cases = [
            ("0.1234", 123_400_000_u64),
            ("123", 123_000_000_000),
            ("-123.456", 123_456_000_000),
            ("0.0034", 3_400_000),
            ("10.0023", 10_002_300_000),
            ("5.12345678911", 5_123_456_789),
        ];
        for (input, expected) in cases {
            let p = PlaceOrder {
                amount: Decimal::from_str(input).unwrap(),
                price: Decimal::from_str(input).unwrap(),
                market_id: 0,
                market_type: MarketType::Perp,
                ..Default::default()
            };
            let order_params = p.to_order_params(Context::DevNet);
            assert_eq!(order_params.base_asset_amount, expected);
            assert_eq!(
                order_params.price,
                expected / 1_000 // 1e9 - 1e6
            );
        }
    }

    #[test]
    fn place_order_to_order_spot() {
        let cases = [
            ("0.1234", 123_400u64, 0_u16),
            ("123", 123_000_000_000, 1),
            ("1.23", 1_230_000_000, 1),
            ("5.123456789", 512_345_678, 4),
        ];
        for (input, expected, market_index) in cases {
            let p = PlaceOrder {
                amount: Decimal::from_str(input).unwrap(),
                price: Decimal::from_str(input).unwrap(),
                market_id: market_index,
                market_type: MarketType::Spot,
                ..Default::default()
            };
            let order_params = p.to_order_params(Context::MainNet);
            assert_eq!(order_params.base_asset_amount, expected);
        }
    }

    #[test]
    fn order_from_sdk_order() {
        let cases = [
            (123_4000u64, Decimal::from_str("1.23400").unwrap(), 0_u16),
            (123_000_000_000, Decimal::from_str("123.0").unwrap(), 1),
            (512_345_678, Decimal::from_str("5.12345678").unwrap(), 4),
        ];
        for (input, expected, market_index) in cases {
            let o = drift_sdk::types::Order {
                base_asset_amount: input,
                price: input,
                market_index,
                market_type: MarketType::Spot,
                ..Default::default()
            };
            let gateway_order = Order::from_sdk_order(o, Context::MainNet);
            assert_eq!(gateway_order.amount, expected);
        }
    }
}
