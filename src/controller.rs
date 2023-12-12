use std::task::Poll;

use actix_web::{web::Bytes, Error};
use drift_sdk::{
    dlob::{DLOBClient, OrderbookStream},
    types::{Context, MarketType, OrderParams, SdkError},
    DriftClient, Pubkey, TransactionBuilder, Wallet,
};
use futures_util::{Stream, StreamExt};
use log::error;
use thiserror::Error;

use crate::types::{
    AllMarketsResponse, CancelOrdersRequest, GetOrderbookRequest, GetOrdersRequest,
    GetOrdersResponse, GetPositionsRequest, GetPositionsResponse, ModifyOrdersRequest,
    PlaceOrdersRequest,
};

#[derive(Error, Debug)]
pub enum ControllerError {
    #[error("internal server error")]
    Sdk(#[from] SdkError),
    #[error("order id not found")]
    UnknownOrderId,
}

#[derive(Clone)]
pub struct AppState {
    wallet: Wallet,
    client: DriftClient,
    dlob_client: DLOBClient,
}

impl AppState {
    /// Configured drift user address
    pub fn user(&self) -> &Pubkey {
        self.wallet.user()
    }
    /// Configured drift signing address + fee payer
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

        let dlob_endpoint = if devnet {
            "https://master.dlob.drift.trade"
        } else {
            "https://dlob.drift.trade"
        };
        Self {
            wallet,
            client,
            dlob_client: DLOBClient::new(dlob_endpoint),
        }
    }

    /// Cancel orders
    ///
    /// There are 4 intended scenarios for cancellation, in order of priority:
    /// 1) "market" is set, cancel all orders in the market
    /// 2) "user ids" are set, cancel all orders by user assigned id
    /// 3) ids are given, cancel all orders by id (global, exchange assigned id)
    /// 4) catch all. cancel all orders
    pub async fn cancel_orders(&self, req: CancelOrdersRequest) -> Result<String, ControllerError> {
        let user_data = self.client.get_account_data(self.user()).await?;
        let builder = TransactionBuilder::new(&self.wallet, &user_data);

        let tx = if let Some(market) = req.market {
            builder.cancel_orders((market.id, market.market_type).into(), None)
        } else if !req.user_ids.is_empty() {
            let order_ids = user_data
                .orders
                .iter()
                .filter(|o| o.slot > 0 && req.user_ids.contains(&o.user_order_id))
                .map(|o| o.order_id)
                .collect();
            builder.cancel_orders_by_id(order_ids)
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

    pub async fn modify_orders(&self, req: ModifyOrdersRequest) -> Result<String, ControllerError> {
        let user_data = &self.client.get_account_data(self.user()).await?;

        let mut params = Vec::<(u32, OrderParams)>::with_capacity(req.orders.len());
        for order in req.orders {
            if let Some(id) = order.order_id {
                params.push((id, order.into()));
            } else if order.user_order_id > 0 {
                if let Some(onchain_order) = user_data
                    .orders
                    .iter()
                    .find(|x| x.user_order_id == order.user_order_id)
                {
                    params.push((onchain_order.order_id, order.into()));
                }
            } else {
                return Err(ControllerError::UnknownOrderId);
            }
        }

        let tx = TransactionBuilder::new(&self.wallet, user_data)
            .modify_orders(params)
            .build();

        let signature = self.client.sign_and_send(&self.wallet, tx).await?;

        Ok(signature.to_string())
    }

    pub fn stream_orderbook(&self, req: GetOrderbookRequest) -> DlobStream {
        let stream = self
            .dlob_client
            .subscribe(req.market.as_market_id(), Some(1)); // poll book at 1s interval
        DlobStream(stream)
    }
}

/// Provides JSON serialized orderbook snapshots
pub struct DlobStream(OrderbookStream);
impl Stream for DlobStream {
    type Item = Result<Bytes, Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(result) => {
                let result = result.unwrap();
                if let Err(err) = result {
                    error!("orderbook stream: {err:?}");
                    return Poll::Ready(None);
                }

                let msg = serde_json::to_vec(&result.unwrap()).unwrap();
                std::task::Poll::Ready(Some(Ok(msg.into())))
            }
        }
    }
}
