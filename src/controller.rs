use std::{borrow::Cow, sync::Arc};

use drift_sdk::{
    dlob::DLOBClient,
    types::{Context, MarketType, ModifyOrderParams, SdkError, SdkResult},
    DriftClient, Pubkey, TransactionBuilder, Wallet, WsAccountProvider,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use log::error;
use thiserror::Error;

use crate::types::{
    get_market_decimals, AllMarketsResponse, CancelAndPlaceRequest, CancelOrdersRequest,
    GetOrderbookRequest, GetOrdersRequest, GetOrdersResponse, GetPositionsRequest,
    GetPositionsResponse, Market, ModifyOrdersRequest, Order, OrderbookL2, PlaceOrdersRequest,
    SpotPosition, TxResponse,
};

pub type GatewayResult<T> = Result<T, ControllerError>;

#[derive(Error, Debug)]
pub enum ControllerError {
    #[error("internal error: {0}")]
    Sdk(#[from] SdkError),
    #[error("order id not found: {0}")]
    UnknownOrderId(u32),
    #[error("{0}")]
    BadRequest(String),
    #[error("tx failed ({code}): {reason}")]
    TxFailed { reason: String, code: u32 },
}

#[derive(Clone)]
pub struct AppState {
    pub wallet: Wallet,
    pub client: Arc<DriftClient<WsAccountProvider>>,
    dlob_client: DLOBClient,
}

impl AppState {
    /// Configured drift authority address
    pub fn authority(&self) -> &Pubkey {
        self.wallet.authority()
    }
    /// Configured drift signing address
    pub fn signer(&self) -> Pubkey {
        self.wallet.signer()
    }
    pub fn default_sub_account(&self) -> Pubkey {
        self.wallet.default_sub_account()
    }
    pub async fn new(endpoint: &str, devnet: bool, wallet: Wallet) -> Self {
        let context = if devnet {
            Context::DevNet
        } else {
            Context::MainNet
        };

        let account_provider = WsAccountProvider::new(endpoint).await.expect("ws connects");
        let client = DriftClient::new(context, endpoint, account_provider)
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
    pub async fn cancel_orders(
        &self,
        req: CancelOrdersRequest,
        sub_account_id: u16,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.wallet.sub_account(sub_account_id);
        let account_data = self.client.get_user_account(&sub_account).await?;
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Borrowed(&account_data),
        )
        .payer(self.wallet.signer());

        let tx = build_cancel_ix(builder, req)?.build();

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
        sub_account_id: u16,
    ) -> GatewayResult<GetPositionsResponse> {
        let sub_account = self.wallet.sub_account(sub_account_id);
        let (all_spot, all_perp) = self.client.all_positions(&sub_account).await?;

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
        sub_account_id: u16,
    ) -> GatewayResult<GetOrdersResponse> {
        let sub_account = self.wallet.sub_account(sub_account_id);
        let orders = self.client.all_orders(&sub_account).await?;
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
                .map(|o| {
                    let base_decimals = get_market_decimals(
                        self.client.program_data(),
                        Market::new(o.market_index, o.market_type),
                    );
                    Order::from_sdk_order(o, base_decimals)
                })
                .collect(),
        })
    }

    pub fn get_markets(&self) -> AllMarketsResponse {
        let spot = self.client.program_data().spot_market_configs();
        let perp = self.client.program_data().perp_market_configs();

        AllMarketsResponse {
            spot: spot.iter().map(|x| (*x).into()).collect(),
            perp: perp.iter().map(|x| (*x).into()).collect(),
        }
    }

    pub async fn cancel_and_place_orders(
        &self,
        req: CancelAndPlaceRequest,
        sub_account_id: u16,
    ) -> GatewayResult<TxResponse> {
        let orders = req
            .place
            .orders
            .into_iter()
            .map(|o| {
                let base_decimals = get_market_decimals(self.client.program_data(), o.market);
                o.to_order_params(base_decimals)
            })
            .collect();

        let sub_account = self.wallet.sub_account(sub_account_id);
        let account_data = self.client.get_user_account(&sub_account).await?;
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            self.wallet.sub_account(0),
            Cow::Borrowed(&account_data),
        )
        .payer(self.wallet.signer());

        let tx = build_cancel_ix(builder, req.cancel)?
            .place_orders(orders)
            .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn place_orders(
        &self,
        req: PlaceOrdersRequest,
        sub_account_id: u16,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.wallet.sub_account(sub_account_id);
        let account_data = self.client.get_user_account(&sub_account).await?;

        let orders = req
            .orders
            .into_iter()
            .map(|o| {
                let base_decimals = get_market_decimals(self.client.program_data(), o.market);
                o.to_order_params(base_decimals)
            })
            .collect();
        let tx = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Borrowed(&account_data),
        )
        .payer(self.wallet.signer())
        .place_orders(orders)
        .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn modify_orders(
        &self,
        req: ModifyOrdersRequest,
        sub_account_id: u16,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.wallet.sub_account(sub_account_id);
        let account_data = &self.client.get_user_account(&sub_account).await?;
        // NB: its possible to let the drift program sort the modifications by userOrderId
        // sorting it client side for simplicity
        let mut params = Vec::<(u32, ModifyOrderParams)>::with_capacity(req.orders.len());
        for order in req.orders {
            if let Some(order_id) = order.order_id {
                if let Some(onchain_order) =
                    account_data.orders.iter().find(|x| x.order_id == order_id)
                {
                    let base_decimals = get_market_decimals(
                        self.client.program_data(),
                        Market::new(onchain_order.market_index, onchain_order.market_type),
                    );
                    params.push((order_id, order.to_order_params(base_decimals)));
                    continue;
                }
            } else if let Some(user_order_id) = order.user_order_id {
                if let Some(onchain_order) = account_data
                    .orders
                    .iter()
                    .find(|x| x.user_order_id == user_order_id)
                {
                    let base_decimals = get_market_decimals(
                        self.client.program_data(),
                        Market::new(onchain_order.market_index, onchain_order.market_type),
                    );
                    params.push((onchain_order.order_id, order.to_order_params(base_decimals)));
                    continue;
                }
            }

            return Err(ControllerError::UnknownOrderId(
                order
                    .order_id
                    .unwrap_or(order.user_order_id.unwrap_or(0) as u32),
            ));
        }

        let tx = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Borrowed(account_data),
        )
        .payer(self.wallet.signer())
        .modify_orders(params.as_slice())
        .build();

        self.client
            .sign_and_send(&self.wallet, tx)
            .await
            .map(|s| TxResponse::new(s.to_string()))
            .map_err(handle_tx_err)
    }

    pub async fn get_orderbook(&self, req: GetOrderbookRequest) -> GatewayResult<OrderbookL2> {
        let book = self.dlob_client.get_l2(req.market.as_market_id()).await?;
        let decimals = get_market_decimals(self.client.program_data(), req.market);
        Ok(OrderbookL2::new(book, decimals))
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

/// helper to transform CancelOrdersRequest into its drift program ix
fn build_cancel_ix(
    builder: TransactionBuilder<'_>,
    req: CancelOrdersRequest,
) -> GatewayResult<TransactionBuilder<'_>> {
    if let Some(market) = req.market {
        Ok(builder.cancel_orders((market.market_index, market.market_type), None))
    } else if req.user_ids.is_some() {
        let user_ids = req.user_ids.unwrap();
        if user_ids.is_empty() {
            Err(ControllerError::BadRequest(
                "user ids cannot be empty".to_owned(),
            ))
        } else {
            Ok(builder.cancel_orders_by_user_id(user_ids))
        }
    } else if req.ids.is_some() {
        let ids = req.ids.unwrap();
        if ids.is_empty() {
            Err(ControllerError::BadRequest(
                "ids cannot be empty".to_owned(),
            ))
        } else {
            Ok(builder.cancel_orders_by_id(ids))
        }
    } else {
        Ok(builder.cancel_all_orders())
    }
}

/// Initialize a wallet for controller, possible valid configs:
///
/// 1) keypair
/// 2) keypair + delegated
/// 3) emulation/RO mode
pub fn create_wallet(
    secret_key: Option<String>,
    emulate: Option<Pubkey>,
    delegate: Option<Pubkey>,
) -> Wallet {
    match (secret_key, delegate, emulate) {
        (Some(secret_key), _, delegate) => {
            let mut wallet = Wallet::try_from_str(secret_key.as_str()).expect("valid key");
            if let Some(authority) = delegate {
                wallet.to_delegated(authority);
            }
            return wallet;
        }
        (None, Some(emulate), None) => return Wallet::read_only(emulate),
        _ => {
            panic!("expected 'DRIFT_SECRET_KEY' or --emulate <pubkey>");
        }
    }
}
