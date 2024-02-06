//! Defines types for:
//! - gateway request/responses
//! - wrappers for presenting drift program types with less implementation detail
//!
use drift_sdk::{
    constants::{ProgramData, BASE_PRECISION, PRICE_PRECISION},
    dlob::{self, L2Level, L2Orderbook},
    types::{
        self as sdk_types, MarketPrecision, MarketType, ModifyOrderParams, OrderParams, PerpMarket,
        PositionDirection, PostOnlyParam, SpotMarket,
    },
};
use rust_decimal::Decimal;
use serde::{ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};

/// decimal places in price values
pub const PRICE_DECIMALS: u32 = PRICE_PRECISION.ilog10();

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    #[serde(serialize_with = "order_type_ser", deserialize_with = "order_type_de")]
    order_type: sdk_types::OrderType,
    market_index: u16,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    oracle_price_offset: Option<Decimal>,
}

impl Order {
    pub fn from_sdk_order(value: sdk_types::Order, base_decimals: u32) -> Self {
        // 0 = long
        // 1 = short
        let to_sign = 1_i64 - 2 * (value.direction as i64);

        Order {
            market_index: value.market_index,
            market_type: value.market_type,
            price: Decimal::new(value.price as i64, PRICE_DECIMALS),
            amount: Decimal::new(value.base_asset_amount as i64 * to_sign, base_decimals),
            filled: Decimal::new(value.base_asset_amount_filled as i64, base_decimals),
            immediate_or_cancel: value.immediate_or_cancel,
            reduce_only: value.reduce_only,
            order_type: value.order_type,
            order_id: value.order_id,
            post_only: value.post_only,
            user_order_id: value.user_order_id,
            oracle_price_offset: if value.oracle_price_offset == 0 {
                None
            } else {
                Some(Decimal::new(
                    value.oracle_price_offset as i64,
                    PRICE_DECIMALS,
                ))
            },
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
#[serde(rename_all = "camelCase")]
pub struct SpotPosition {
    amount: Decimal,
    #[serde(rename = "type")]
    balance_type: String, // deposit or borrow
    market_index: u16,
}

impl SpotPosition {
    pub fn from_sdk_type(
        value: &sdk_types::SpotPosition,
        spot_market: &sdk_types::SpotMarket,
    ) -> Self {
        // TODO: handle error
        let token_amount = value.get_token_amount(spot_market).expect("ok");
        Self {
            amount: Decimal::from_i128_with_scale(token_amount as i128, spot_market.decimals)
                .normalize(),
            market_index: value.market_index,
            balance_type: if value.balance_type == Default::default() {
                "deposit".into()
            } else {
                "borrow".into()
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpPosition {
    amount: Decimal,
    average_entry: Decimal,
    market_index: u16,
    #[serde(skip)]
    inner: sdk_types::PerpPosition,
}

impl PerpPosition {
    pub fn calculate_unrealized_pnl(&mut self, oracle_price: i64) -> Decimal {
        Decimal::new(
            self.inner
                .get_unrealized_pnl(oracle_price)
                .expect("no overflow") as i64,
            PRICE_DECIMALS,
        )
    }
}

impl From<sdk_types::PerpPosition> for PerpPosition {
    fn from(value: sdk_types::PerpPosition) -> Self {
        let amount = Decimal::new(value.base_asset_amount, BASE_PRECISION.ilog10());
        let average_entry = Decimal::new(value.quote_entry_amount.abs(), PRICE_DECIMALS) / amount;

        Self {
            amount: amount.normalize(),
            market_index: value.market_index,
            average_entry: average_entry.normalize().round_dp(4),
            inner: value,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ModifyOrdersRequest {
    pub orders: Vec<ModifyOrder>,
}

#[cfg_attr(test, derive(Default))]
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModifyOrder {
    #[serde(flatten)]
    pub market: Market,
    amount: Option<Decimal>,
    price: Option<Decimal>,
    pub user_order_id: Option<u8>,
    pub order_id: Option<u32>,
    reduce_only: Option<bool>,
    oracle_price_offset: Option<Decimal>,
    max_ts: Option<i64>,
}

impl ModifyOrder {
    pub fn to_order_params(self, base_decimals: u32) -> ModifyOrderParams {
        let target_scale = 10_u32.pow(base_decimals);

        let (amount, direction) = if let Some(base_amount) = self.amount {
            let direction = if base_amount.is_sign_negative() {
                PositionDirection::Short
            } else {
                PositionDirection::Long
            };
            (
                Some(scale_decimal_to_u64(base_amount.abs(), target_scale)),
                Some(direction),
            )
        } else {
            (None, None)
        };

        let price = self
            .price
            .map(|p| scale_decimal_to_u64(p, PRICE_PRECISION as u32));

        let oracle_price_offset = self
            .oracle_price_offset
            .map(|p| scale_decimal_to_i64(p, PRICE_PRECISION as u32) as i32);

        ModifyOrderParams {
            base_asset_amount: amount,
            direction,
            price,
            reduce_only: self.reduce_only,
            oracle_price_offset,
            max_ts: self.max_ts,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PlaceOrdersRequest {
    pub orders: Vec<PlaceOrder>,
}

#[cfg_attr(test, derive(Default))]
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlaceOrder {
    #[serde(flatten)]
    pub market: Market,
    amount: Decimal,
    #[serde(default)]
    price: Decimal,
    /// 0 indicates it is not set (according to program)
    #[serde(default)]
    pub user_order_id: u8,
    #[serde(
        serialize_with = "order_type_ser",
        deserialize_with = "order_type_de",
        default
    )]
    order_type: sdk_types::OrderType,
    #[serde(default)]
    post_only: bool,
    #[serde(default)]
    reduce_only: bool,
    #[serde(default)]
    oracle_price_offset: Option<Decimal>,
    max_ts: Option<i64>,
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

#[inline]
/// Convert decimal to unsigned fixed-point representation with `target` precision
fn scale_decimal_to_i64(x: Decimal, target: u32) -> i64 {
    ((x.mantissa() * target as i128) / 10_i128.pow(x.scale())) as i64
}

impl PlaceOrder {
    pub fn to_order_params(self, base_decimals: u32) -> OrderParams {
        let target_scale = 10_u32.pow(base_decimals);
        let base_amount = scale_decimal_to_u64(self.amount.abs(), target_scale);
        let price = if self.oracle_price_offset.is_none() {
            scale_decimal_to_u64(self.price, PRICE_PRECISION as u32)
        } else {
            0
        };

        OrderParams {
            market_index: self.market.market_index,
            market_type: self.market.market_type,
            order_type: self.order_type,
            base_asset_amount: base_amount,
            direction: if self.amount.is_sign_negative() {
                PositionDirection::Short
            } else {
                PositionDirection::Long
            },
            price,
            reduce_only: self.reduce_only,
            post_only: if self.post_only {
                PostOnlyParam::MustPostOnly // this will report the failure to the gateway caller
            } else {
                PostOnlyParam::None
            },
            user_order_id: self.user_order_id,
            oracle_price_offset: self
                .oracle_price_offset
                .map(|x| scale_decimal_to_i64(x, PRICE_PRECISION as u32) as i32),
            max_ts: self.max_ts,
            ..Default::default()
        }
    }
}

#[cfg_attr(test, derive(Default))]
#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Market {
    /// The market index
    pub market_index: u16,
    #[serde(
        serialize_with = "market_type_ser",
        deserialize_with = "market_type_de"
    )]
    /// The market type (Spot or Perp)
    pub market_type: MarketType,
}

impl Market {
    pub fn new(market_index: u16, market_type: MarketType) -> Self {
        Self {
            market_index,
            market_type,
        }
    }
    pub fn spot(index: u16) -> Self {
        Self {
            market_index: index,
            market_type: MarketType::Spot,
        }
    }
    pub fn perp(index: u16) -> Self {
        Self {
            market_index: index,
            market_type: MarketType::Perp,
        }
    }
    pub fn as_market_id(self) -> drift_sdk::types::MarketId {
        unsafe { std::mem::transmute(self) }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPositionsRequest {
    #[serde(flatten)]
    pub market: Market,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOrdersRequest {
    #[serde(flatten)]
    pub market: Market,
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
#[serde(rename_all = "camelCase")]
pub struct MarketInfo {
    #[serde(rename = "marketIndex")]
    market_id: u16,
    symbol: String,
    price_step: Decimal,
    amount_step: Decimal,
    min_order_size: Decimal,
}

impl From<SpotMarket> for MarketInfo {
    fn from(value: SpotMarket) -> Self {
        Self {
            market_id: value.market_index,
            symbol: unsafe { core::str::from_utf8_unchecked(&value.name) }
                .trim_end()
                .to_string(),
            price_step: Decimal::new(value.price_tick() as i64, PRICE_PRECISION.ilog10())
                .normalize(),
            amount_step: Decimal::new(value.quantity_tick() as i64, value.decimals).normalize(),
            min_order_size: Decimal::new(value.min_order_size() as i64, value.decimals).normalize(),
        }
    }
}

impl From<PerpMarket> for MarketInfo {
    fn from(value: PerpMarket) -> Self {
        Self {
            market_id: value.market_index,
            symbol: unsafe { core::str::from_utf8_unchecked(&value.name) }
                .trim_end()
                .to_string(),
            price_step: Decimal::new(value.price_tick() as i64, PRICE_PRECISION.ilog10())
                .normalize(),
            amount_step: Decimal::new(value.quantity_tick() as i64, BASE_PRECISION.ilog10())
                .normalize(),
            min_order_size: Decimal::new(value.min_order_size() as i64, BASE_PRECISION.ilog10())
                .normalize(),
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
    #[serde(flatten, default)]
    pub market: Option<Market>,
    /// order Ids to cancel
    #[serde(default)]
    pub ids: Option<Vec<u32>>,
    /// user assigned order Ids to cancel
    #[serde(default)]
    pub user_ids: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOrderbookRequest {
    #[serde(flatten)]
    pub market: Market,
}

#[derive(Serialize, Deserialize)]
pub struct TxResponse {
    tx: String,
}

impl TxResponse {
    pub fn new(tx_signature: String) -> Self {
        Self { tx: tx_signature }
    }
}

#[derive(Serialize, Deserialize)]
pub struct CancelAndPlaceRequest {
    pub cancel: CancelOrdersRequest,
    pub modify: ModifyOrdersRequest,
    pub place: PlaceOrdersRequest,
}

/// Serialize DLOB with human readable numeric values
pub struct OrderbookL2 {
    inner: L2Orderbook,
    decimals: u32,
}

impl OrderbookL2 {
    pub fn new(inner: L2Orderbook, decimals: u32) -> Self {
        Self { inner, decimals }
    }
}

impl Serialize for OrderbookL2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("slot", &self.inner.slot)?;
        map.serialize_entry(
            "bids",
            &PriceLevelSerializer {
                inner: self.inner.bids.as_slice(),
                decimals: self.decimals,
            },
        )?;
        map.serialize_entry(
            "asks",
            &PriceLevelSerializer {
                inner: self.inner.asks.as_slice(),
                decimals: self.decimals,
            },
        )?;
        map.end()
    }
}

struct PriceLevelSerializer<'a> {
    inner: &'a [L2Level],
    /// decimal precision of the asset
    decimals: u32,
}

impl<'a> Serialize for PriceLevelSerializer<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.inner.iter().map(|l| PriceLevel::new(l, self.decimals)))
    }
}

#[derive(Serialize, Deserialize)]
pub struct PriceLevel {
    price: Decimal,
    amount: Decimal,
}

impl PriceLevel {
    pub fn new(level: &dlob::L2Level, decimals: u32) -> Self {
        Self {
            price: Decimal::new(level.price as i64, PRICE_PRECISION.ilog10()),
            amount: Decimal::new(level.size as i64, decimals),
        }
    }
}

/// Return the number of decimal places for the market
#[inline]
pub(crate) fn get_market_decimals(program_data: &ProgramData, market: Market) -> u32 {
    if let MarketType::Perp = market.market_type {
        BASE_PRECISION.ilog10()
    } else {
        let spot_market = program_data
            .spot_market_config_by_index(market.market_index)
            .expect("market exists");
        spot_market.decimals
    }
}

#[derive(Serialize)]
pub struct SolBalanceResponse {
    pub balance: Decimal,
}

#[cfg(test)]
mod tests {
    use drift_sdk::{
        constants::BASE_PRECISION,
        types::{MarketType, OrderType, PositionDirection},
    };
    use std::str::FromStr;

    use crate::types::{Market, ModifyOrder, Order};

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
                market: Market::perp(0),
                ..Default::default()
            };
            let order_params = p.to_order_params(9);
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
            ("0.1234", 123_400u64, 6),
            ("123", 123_000_000_000, 9),
            ("1.23", 1_230_000_000, 9),
            ("-1.23", 1_230_000_000, 9),
            ("5.123456789", 512_345_678, 8), // truncates extra decimals
        ];
        for (input, expected, base_decimals) in cases {
            let p = PlaceOrder {
                amount: Decimal::from_str(input).unwrap(),
                price: Decimal::from_str(input).unwrap(),
                ..Default::default()
            };
            let is_short = p.amount.is_sign_negative();
            let order_params = p.to_order_params(base_decimals);
            assert_eq!(order_params.base_asset_amount, expected);
            if is_short {
                assert_eq!(order_params.direction, PositionDirection::Short);
            } else {
                assert_eq!(order_params.direction, PositionDirection::Long);
            }
        }
    }

    #[test]
    fn oracle_price_offset_works() {
        let p = PlaceOrder {
            price: Decimal::from_str("1.23").unwrap(),
            oracle_price_offset: Decimal::from_str("-0.5").ok(),
            order_type: OrderType::Limit,
            market: Market::perp(0),
            ..Default::default()
        };
        let order = p.to_order_params(6);
        assert_eq!(order.price, 0);
        assert_eq!(order.oracle_price_offset, Some(-500_000));

        let o = drift_sdk::types::Order {
            base_asset_amount: 1 * BASE_PRECISION,
            price: 0,
            market_index: 0,
            market_type: MarketType::Perp,
            oracle_price_offset: -500_000,
            ..Default::default()
        };
        let order = Order::from_sdk_order(o, BASE_PRECISION.ilog10());
        assert_eq!(order.price, Decimal::ZERO,);

        assert_eq!(order.oracle_price_offset, Decimal::from_str("-0.5").ok());
    }

    #[test]
    fn order_from_sdk_order() {
        let cases = [
            (1_230_400_000_u64, Decimal::from_str("1.2304").unwrap(), 9),
            (123_000_000_000, Decimal::from_str("123.0").unwrap(), 9),
            (5_123_456_789, Decimal::from_str("5.123456789").unwrap(), 9),
        ];
        for (input, expected, base_decimals) in cases {
            let o = drift_sdk::types::Order {
                base_asset_amount: input,
                price: input,
                market_type: MarketType::Perp,
                ..Default::default()
            };
            let gateway_order = Order::from_sdk_order(o, base_decimals);
            assert_eq!(gateway_order.amount, expected);
        }
    }

    #[test]
    fn modify_order_to_order_params() {
        let m = ModifyOrder {
            amount: Decimal::from_str("-0.5").ok(),
            price: Decimal::from_str("11.1").ok(),
            oracle_price_offset: Decimal::from_str("0.1").ok(),
            ..Default::default()
        };
        let order_params = m.to_order_params(9);

        assert_eq!(order_params.direction, Some(PositionDirection::Short));
        assert_eq!(order_params.base_asset_amount, Some(500_000_000));
        assert_eq!(order_params.price, Some(11_100_000));
        assert_eq!(order_params.oracle_price_offset, Some(100_000));

        let m = ModifyOrder {
            amount: Decimal::from_str("12").ok(),
            price: Decimal::from_str("1.02").ok(),
            oracle_price_offset: Decimal::from_str("-2").ok(),
            ..Default::default()
        };
        let order_params = m.to_order_params(9);

        assert_eq!(order_params.direction, Some(PositionDirection::Long));
        assert_eq!(order_params.base_asset_amount, Some(12_000_000_000));
        assert_eq!(order_params.price, Some(1_020_000));
        assert_eq!(order_params.oracle_price_offset, Some(-2_000_000));
    }
}
