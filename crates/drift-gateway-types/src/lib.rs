mod account_event;
mod types;
pub use account_event::*;
pub use drift_rs;
use drift_rs::{
    constants::ProgramData,
    math::constants::{BASE_PRECISION, PRICE_PRECISION, QUOTE_PRECISION},
    types::MarketType,
};
pub use drift_rs::{constants::PROGRAM_ID, Context};
use serde::{Deserialize, Serialize};
pub use types::*;
pub const PRICE_DECIMALS: u32 = PRICE_PRECISION.ilog10();
pub const QUOTE_DECIMALS: u32 = QUOTE_PRECISION.ilog10();

/// Return the number of decimal places for the market
#[inline]
pub fn get_market_decimals(program_data: &ProgramData, market: Market) -> u32 {
    if let MarketType::Perp = market.market_type {
        BASE_PRECISION.ilog10()
    } else {
        let spot_market = program_data
            .spot_market_config_by_index(market.market_index)
            .expect("market exists");
        spot_market.decimals
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Subscribe,
    Unsubscribe,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Fills,
    Orders,
    Funding,
    Swap,
}
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WsRequest {
    pub method: Method,
    pub sub_account_id: u8,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WsEvent<T: Serialize> {
    pub data: T,
    pub channel: Channel,
    pub sub_account_id: u8,
}
