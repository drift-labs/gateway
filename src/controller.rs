use drift_sdk::{types::Context, DriftClient, TransactionBuilder, Wallet};

use crate::types::{AllMarketsResponse, GetOrdersResponse, GetPositionsResponse};

#[derive(Clone)]
pub struct AppState {
    wallet: Wallet,
    client: DriftClient,
}

impl AppState {
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
        client.subscribe_account(&wallet).await.expect("cache on");
        Self { wallet, client }
    }

    pub async fn cancel_orders(&self) -> Result<String, ()> {
        let tx = TransactionBuilder::new(
            &self.wallet,
            &self
                .client
                .get_account_data(&self.wallet)
                .await
                .map_err(|_| ())?,
        )
        .cancel_all_orders()
        .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map_err(|_| ())
            .map(|s| s.to_string())
    }

    pub async fn get_positions(&self) -> Result<GetPositionsResponse, ()> {
        // TODO: log/surface sdk error
        let (spot, perp) = self
            .client
            .all_positions(&self.wallet)
            .await
            .map_err(|_| ())?;
        Ok(GetPositionsResponse {
            spot: spot.iter().map(|x| (*x).into()).collect(),
            perp: perp.iter().map(|x| (*x).into()).collect(),
        })
    }

    pub async fn get_orders(&self) -> Result<GetOrdersResponse, ()> {
        // TODO: log/surface sdk error
        let orders = self.client.all_orders(&self.wallet).await.map_err(|_| ())?;
        Ok(GetOrdersResponse {
            orders: orders.into_iter().map(Into::into).collect(),
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
}
