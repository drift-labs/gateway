use std::{borrow::Cow, sync::Arc};

use drift_sdk::{
    constants::ProgramData,
    dlob::DLOBClient,
    types::{Context, MarketType, ModifyOrderParams, SdkError, SdkResult},
    AccountProvider, DriftClient, Pubkey, TransactionBuilder, Wallet, WsAccountProvider,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use log::{error, warn};
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
        let client = DriftClient::new(context, account_provider)
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
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data?),
        )
        .priority_fee(pf)
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
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

        let builder = TransactionBuilder::new(
            self.client.program_data(),
            self.wallet.sub_account(0),
            Cow::Owned(account_data?),
        )
        .priority_fee(pf)
        .payer(self.wallet.signer());

        let builder = build_cancel_ix(builder, req.cancel)?;
        let tx = build_modify_ix(builder, req.modify, self.client.program_data())?
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
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

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
            Cow::Borrowed(&account_data?),
        )
        .priority_fee(pf)
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
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data?),
        )
        .priority_fee(pf)
        .payer(self.wallet.signer());
        let tx = build_modify_ix(builder, req, self.client.program_data())?.build();

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

fn build_modify_ix<'a>(
    builder: TransactionBuilder<'a>,
    req: ModifyOrdersRequest,
    program_data: &ProgramData,
) -> GatewayResult<TransactionBuilder<'a>> {
    if req.orders.is_empty() {
        return Ok(builder);
    }

    let by_user_order_ids = req.orders[0].user_order_id.is_some_and(|x| x > 0);
    if by_user_order_ids {
        let mut params = Vec::<(u8, ModifyOrderParams)>::with_capacity(req.orders.len());
        for order in req.orders {
            let base_decimals = get_market_decimals(program_data, order.market);
            params.push((
                order.user_order_id.ok_or(ControllerError::BadRequest(
                    "userOrderId not set".to_string(),
                ))?,
                order.to_order_params(base_decimals),
            ));
        }
        Ok(builder.modify_orders_by_user_id(params.as_slice()))
    } else {
        let mut params = Vec::<(u32, ModifyOrderParams)>::with_capacity(req.orders.len());
        for order in req.orders {
            let base_decimals = get_market_decimals(program_data, order.market);
            params.push((
                order
                    .order_id
                    .ok_or(ControllerError::BadRequest("orderId not set".to_string()))?,
                order.to_order_params(base_decimals),
            ));
        }
        Ok(builder.modify_orders(params.as_slice()))
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
    match (&secret_key, emulate, delegate) {
        (Some(secret_key), _, delegate) => {
            let mut wallet = Wallet::try_from_str(secret_key).expect("valid key");
            if let Some(authority) = delegate {
                wallet.to_delegated(authority);
            }
            wallet
        }
        (None, Some(emulate), None) => Wallet::read_only(emulate),
        _ => {
            panic!("expected 'DRIFT_GATEWAY_KEY' or --emulate <pubkey>");
        }
    }
}

/// get average priority fee from chain, no accounts writable (user's subaccount is negligible)
async fn get_priority_fee<T: AccountProvider>(client: &DriftClient<T>) -> u64 {
    let mut priority_fee = 1_u64;
    if let Ok(recent_fees) = client.get_recent_priority_fees(&[], Some(16)).await {
        // includes possibly 0 values
        if let Some(avg_priority_fee) = recent_fees
            .iter()
            .sum::<u64>()
            .checked_div(recent_fees.len() as u64)
        {
            priority_fee = avg_priority_fee;
        }
    } else {
        warn!(target: "controller", "failed to fetch live priority fee");
    }

    priority_fee
}
