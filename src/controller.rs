use std::sync::Arc;

use drift_sdk::{
    dlob::{DLOBClient, L2Orderbook},
    types::{Context, MarketType, ModifyOrderParams, SdkError, SdkResult},
    DriftClient, Pubkey, TransactionBuilder, Wallet, WsAccountProvider,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use log::error;
use thiserror::Error;

use crate::types::{
    AllMarketsResponse, CancelAndPlaceRequest, CancelOrdersRequest, GetOrderbookRequest,
    GetOrdersRequest, GetOrdersResponse, GetPositionsRequest, GetPositionsResponse,
    ModifyOrdersRequest, Order, PlaceOrdersRequest, SpotPosition, TxResponse,
};

pub type GatewayResult<T> = Result<T, ControllerError>;

#[derive(Error, Debug)]
pub enum ControllerError {
    #[error("internal server error")]
    Sdk(#[from] SdkError),
    #[error("order id not found")]
    UnknownOrderId(u32),
    #[error("tx failed ({code}): {reason}")]
    TxFailed { reason: String, code: u32 },
}

#[derive(Clone)]
pub struct AppState {
    wallet: Wallet,
    client: Arc<DriftClient<WsAccountProvider>>,
    dlob_client: DLOBClient,
}

impl AppState {
    /// Configured program/network context
    pub fn context(&self) -> Context {
        self.wallet.context()
    }
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
                Context::DevNet
            } else {
                Context::MainNet
            },
            secret_key,
        )
        .expect("valid key");
        let account_provider = WsAccountProvider::new(endpoint).await.expect("ws connects");
        let client = DriftClient::new(endpoint, account_provider)
            .await
            .expect("ok");

        let dlob_endpoint = if devnet {
            "https://master.dlob.drift.trade"
        } else {
            "https://dlob.drift.trade"
        };

        Self {
            wallet,
            client: Arc::new(client),
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
    pub async fn cancel_orders(&self, req: CancelOrdersRequest) -> GatewayResult<TxResponse> {
        let user_data = self.client.get_user_account(self.user()).await?;
        let builder = TransactionBuilder::new(&self.wallet, &user_data);

        let tx = if let Some(market) = req.market {
            builder.cancel_orders((market.market_index, market.market_type), None)
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

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    /// Return orders by position if given, otherwise return all positions
    pub async fn get_positions(
        &self,
        req: Option<GetPositionsRequest>,
    ) -> GatewayResult<GetPositionsResponse> {
        let (all_spot, all_perp) = self.client.all_positions(self.user()).await?;

        // calculating spot token balance requires knowing the 'spot market account' data
        let mut filtered_spot_positions = Vec::<SpotPosition>::with_capacity(all_spot.len());
        let mut filtered_spot_futs = FuturesUnordered::from_iter(
            all_spot
                .iter()
                .filter(|p| {
                    if let Some(GetPositionsRequest { ref market }) = req {
                        p.market_index == market.market_index
                            && MarketType::Spot == market.market_type
                    } else {
                        true
                    }
                })
                .map(|x| async {
                    let spot_market_info = self.client.get_spot_market_info(x.market_index).await?;
                    SdkResult::Ok(SpotPosition::from_sdk_type(x, &spot_market_info))
                }),
        );
        while let Some(result) = filtered_spot_futs.next().await {
            filtered_spot_positions.push(result?);
        }

        Ok(GetPositionsResponse {
            spot: filtered_spot_positions,
            perp: all_perp
                .iter()
                .filter(|p| {
                    if let Some(GetPositionsRequest { ref market }) = req {
                        p.market_index == market.market_index
                            && MarketType::Perp == market.market_type
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
        req: Option<GetOrdersRequest>,
    ) -> GatewayResult<GetOrdersResponse> {
        let orders = self.client.all_orders(self.user()).await?;
        Ok(GetOrdersResponse {
            orders: orders
                .into_iter()
                .filter(|o| {
                    if let Some(GetOrdersRequest { ref market }) = req {
                        o.market_index == market.market_index && o.market_type == market.market_type
                    } else {
                        true
                    }
                })
                .map(|o| Order::from_sdk_order(o, self.context()))
                .collect(),
        })
    }

    pub fn get_markets(&self) -> AllMarketsResponse {
        let spot = drift_sdk::constants::spot_market_configs(self.wallet.context());
        let perp = drift_sdk::constants::perp_market_configs(self.wallet.context());

        AllMarketsResponse {
            spot: spot.iter().map(|x| (*x).into()).collect(),
            perp: perp.iter().map(|x| (*x).into()).collect(),
        }
    }

    pub async fn cancel_and_place_orders(
        &self,
        req: CancelAndPlaceRequest,
    ) -> GatewayResult<TxResponse> {
        let orders = req
            .place
            .orders
            .into_iter()
            .map(|o| o.to_order_params(self.context()))
            .collect();

        let user_data = self.client.get_user_account(self.user()).await?;
        let builder = TransactionBuilder::new(&self.wallet, &user_data);

        let tx = if let Some(market) = req.cancel.market {
            builder.cancel_orders((market.market_index, market.market_type), None)
        } else if !req.cancel.user_ids.is_empty() {
            let order_ids = user_data
                .orders
                .iter()
                .filter(|o| o.slot > 0 && req.cancel.user_ids.contains(&o.user_order_id))
                .map(|o| o.order_id)
                .collect();
            builder.cancel_orders_by_id(order_ids)
        } else if !req.cancel.ids.is_empty() {
            builder.cancel_orders_by_id(req.cancel.ids)
        } else {
            builder.cancel_all_orders()
        }
        .place_orders(orders)
        .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn place_orders(&self, req: PlaceOrdersRequest) -> GatewayResult<TxResponse> {
        let orders = req
            .orders
            .into_iter()
            .map(|o| o.to_order_params(self.context()))
            .collect();
        let tx = TransactionBuilder::new(
            &self.wallet,
            &self.client.get_user_account(self.user()).await?,
        )
        .place_orders(orders)
        .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn modify_orders(&self, req: ModifyOrdersRequest) -> GatewayResult<TxResponse> {
        let user_data = &self.client.get_user_account(self.user()).await?;
        // NB: its possible to let the drift program sort the modifications by userOrderId
        // sorting it client side for simplicity
        let mut params = Vec::<(u32, ModifyOrderParams)>::with_capacity(req.orders.len());
        for order in req.orders {
            if let Some(order_id) = order.order_id {
                if let Some(onchain_order) =
                    user_data.orders.iter().find(|x| x.order_id == order_id)
                {
                    params.push((
                        order_id,
                        order.to_order_params(
                            onchain_order.market_index,
                            onchain_order.market_type,
                            self.context(),
                        ),
                    ));
                }
            } else if let Some(user_order_id) = order.user_order_id {
                if let Some(onchain_order) = user_data
                    .orders
                    .iter()
                    .find(|x| x.user_order_id == user_order_id)
                {
                    params.push((
                        onchain_order.order_id,
                        order.to_order_params(
                            onchain_order.market_index,
                            onchain_order.market_type,
                            self.context(),
                        ),
                    ));
                }
            } else {
                return Err(ControllerError::UnknownOrderId(
                    order
                        .order_id
                        .unwrap_or(order.user_order_id.unwrap_or(0) as u32),
                ));
            }
        }

        let tx = TransactionBuilder::new(&self.wallet, user_data)
            .modify_orders(params)
            .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn get_orderbook(&self, req: GetOrderbookRequest) -> GatewayResult<L2Orderbook> {
        let book = self.dlob_client.get_l2(req.market.as_market_id()).await;
        Ok(book?)
    }
}

fn handle_tx_err(err: SdkError) -> ControllerError {
    if let Some(code) = err.to_anchor_error_code() {
        ControllerError::TxFailed {
            reason: code.name(),
            code: code.into(),
        }
    } else {
        ControllerError::Sdk(err)
    }
}
