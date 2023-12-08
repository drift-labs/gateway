use drift_sdk::{
    types::{Context, MarketType, SdkError},
    DriftClient, Pubkey, TransactionBuilder, Wallet,
};
use log::error;
use thiserror::Error;

use crate::types::{
    AllMarketsResponse, CancelOrdersRequest, GetOrdersRequest, GetOrdersResponse,
    GetPositionsRequest, GetPositionsResponse, PlaceOrdersRequest,
};

#[derive(Error, Debug)]
pub enum ControllerError {
    #[error("internal server error")]
    Sdk(#[from] SdkError),
    // #[error("the data for key `{0}` is not available")]
    // Redaction(String),
}

#[derive(Clone)]
pub struct AppState {
    wallet: Wallet,
    client: DriftClient,
}

impl AppState {
    pub fn user(&self) -> &Pubkey {
        self.wallet.user()
    }
    pub fn authority(&self) -> Pubkey {
        self.wallet.authority()
    }
    pub async fn new(secret_key: &str, endpoint: &str, devnet: bool) -> Self {
        let wallet = Wallet::try_from_str(
            if devnet {
                Context::Dev
            } else {
                Context::Mainnet
            },
            secret_key,
        )
        .expect("valid key");
        let client = DriftClient::new(endpoint).await.expect("connects");
        client
            .subscribe_account(wallet.user())
            .await
            .expect("cache on");
        Self { wallet, client }
    }

    /// Cancel orders
    ///
    /// There are 3 intended scenarios for cancellation, in order of priority:
    /// 1) "market" is set cancel all orders in the market
    /// 2) ids are given cancel all orders by id
    /// 3) catch all cancel all orders
    pub async fn cancel_orders(&self, req: CancelOrdersRequest) -> Result<String, ControllerError> {
        let user_data = self.client.get_account_data(self.user()).await?;
        let builder = TransactionBuilder::new(&self.wallet, &user_data);

        let tx = if let Some(market) = req.market {
            builder.cancel_orders((market.id, market.market_type).into(), None)
        } else if !req.ids.is_empty() {
            builder.cancel_orders_by_id(req.ids)
        } else {
            builder.cancel_all_orders()
        }
        .build();

        let signature = self.client.sign_and_send(&self.wallet, tx).await?;

        Ok(signature.to_string())
    }

    /// Return orders by position if given, otherwise return all positions
    pub async fn get_positions(
        &self,
        req: GetPositionsRequest,
    ) -> Result<GetPositionsResponse, ControllerError> {
        let (spot, perp) = self.client.all_positions(self.user()).await?;
        Ok(GetPositionsResponse {
            spot: spot
                .iter()
                .filter(|p| {
                    if let Some(ref market) = req.market {
                        p.market_index == market.id && MarketType::Spot == market.market_type
                    } else {
                        true
                    }
                })
                .map(|x| (*x).into())
                .collect(),
            perp: perp
                .iter()
                .filter(|p| {
                    if let Some(ref market) = req.market {
                        p.market_index == market.id && MarketType::Perp == market.market_type
                    } else {
                        true
                    }
                })
                .map(|x| (*x).into())
                .collect(),
        })
    }

    /// Return orders by market if given, otherwise return all orders
    pub async fn get_orders(
        &self,
        req: GetOrdersRequest,
    ) -> Result<GetOrdersResponse, ControllerError> {
        let orders = self.client.all_orders(self.user()).await?;
        Ok(GetOrdersResponse {
            orders: orders
                .into_iter()
                .filter(|o| {
                    if let Some(ref market) = req.market {
                        o.market_index == market.id && o.market_type == market.market_type
                    } else {
                        true
                    }
                })
                .map(Into::into)
                .collect(),
        })
    }

    pub fn get_markets(&self) -> AllMarketsResponse {
        let spot = drift_sdk::constants::spot_markets(self.wallet.context());
        let perp = drift_sdk::constants::perp_markets(self.wallet.context());

        AllMarketsResponse {
            spot: spot.iter().map(|x| (*x).into()).collect(),
            perp: perp.iter().map(|x| (*x).into()).collect(),
        }
    }

    pub async fn place_orders(&self, req: PlaceOrdersRequest) -> Result<String, ControllerError> {
        let orders = req.orders.into_iter().map(Into::into).collect();
        let tx = TransactionBuilder::new(
            &self.wallet,
            &self.client.get_account_data(self.user()).await?,
        )
        .place_orders(orders)
        .build();

        let signature = self.client.sign_and_send(&self.wallet, tx).await?;

        Ok(signature.to_string())
    }
}
