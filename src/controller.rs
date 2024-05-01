use drift_sdk::{
    constants::{ProgramData, BASE_PRECISION},
    dlob_client::DLOBClient,
    event_subscriber::{try_parse_log, CommitmentConfig},
    math::liquidation::{
        calculate_liquidation_price_and_unrealized_pnl, calculate_margin_requirements,
    },
    types::{
        self, MarketId, MarketType, ModifyOrderParams, RpcSendTransactionConfig, SdkError,
        SdkResult, VersionedMessage,
    },
    AccountProvider, DriftClient, Pubkey, RpcAccountProvider, TransactionBuilder, Wallet,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use log::{debug, warn};
use rust_decimal::Decimal;
use solana_client::{client_error::ClientErrorKind, rpc_config::RpcTransactionConfig};
use solana_sdk::signature::Signature;
use solana_transaction_status::{option_serializer::OptionSerializer, UiTransactionEncoding};
use std::{borrow::Cow, str::FromStr, sync::Arc};
use thiserror::Error;

use crate::{
    types::{
        get_market_decimals, AllMarketsResponse, CancelAndPlaceRequest, CancelOrdersRequest,
        GetOrderbookRequest, GetOrdersRequest, GetOrdersResponse, GetPositionsRequest,
        GetPositionsResponse, Market, MarketInfoResponse, ModifyOrdersRequest, Order, OrderbookL2,
        PerpPosition, PerpPositionExtended, PlaceOrdersRequest, SolBalanceResponse, SpotPosition,
        TxEventsResponse, TxResponse, UserMarginResponse, PRICE_DECIMALS,
    },
    websocket::map_drift_event_for_account,
    Context, LOG_TARGET,
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
    #[error("tx not found: {tx_sig}")]
    TxNotFound { tx_sig: String },
}

#[derive(Clone)]
pub struct AppState {
    pub wallet: Wallet,
    /// true if gateway is using delegated signing
    delegated: bool,
    pub client: Arc<DriftClient<RpcAccountProvider>>,
    dlob_client: DLOBClient,
    /// Solana tx commitment level for preflight confirmation
    tx_commitment: CommitmentConfig,
    /// default sub_account_id to use if not provided
    default_subaccount_id: u16,
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
        self.wallet.sub_account(self.default_subaccount_id)
    }
    pub fn resolve_sub_account(&self, sub_account_id: Option<u16>) -> Pubkey {
        self.wallet
            .sub_account(sub_account_id.unwrap_or(self.default_subaccount_id))
    }

    pub async fn new(
        endpoint: &str,
        devnet: bool,
        wallet: Wallet,
        commitment: Option<(CommitmentConfig, CommitmentConfig)>,
        default_subaccount_id: Option<u16>,
    ) -> Self {
        let (state_commitment, tx_commitment) =
            commitment.unwrap_or((CommitmentConfig::confirmed(), CommitmentConfig::confirmed()));
        let context = if devnet {
            types::Context::DevNet
        } else {
            types::Context::MainNet
        };

        let account_provider = RpcAccountProvider::with_commitment(endpoint, state_commitment);
        let client = DriftClient::new(context, account_provider, wallet.clone())
            .await
            .expect("ok");
        client.subscribe().await.expect("subd onchain data");

        let dlob_endpoint = if devnet {
            "https://master.dlob.drift.trade"
        } else {
            "https://dlob.drift.trade"
        };

        Self {
            delegated: wallet.is_delegated(),
            wallet,
            client: Arc::new(client),
            dlob_client: DLOBClient::new(dlob_endpoint),
            tx_commitment,
            default_subaccount_id: default_subaccount_id.unwrap_or(0),
        }
    }

    /// Return SOL balance of the tx signing account
    pub async fn get_sol_balance(&self) -> GatewayResult<SolBalanceResponse> {
        let balance = self
            .client
            .inner()
            .get_balance(&self.wallet.signer())
            .await
            .map_err(|err| ControllerError::Sdk(err.into()))?;
        Ok(SolBalanceResponse {
            balance: Decimal::new(balance as i64, BASE_PRECISION.ilog10()).normalize(),
        })
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
        ctx: Context,
        req: CancelOrdersRequest,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );
        let priority_fee = ctx.cu_price.unwrap_or(pf);
        debug!(target: LOG_TARGET, "priority_fee: {priority_fee:?}");
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data?),
            self.delegated,
        )
        .with_priority_fee(priority_fee, ctx.cu_limit);
        let tx = build_cancel_ix(builder, req)?.build();
        self.send_tx(tx, "cancel_orders").await
    }

    /// Return orders by position if given, otherwise return all positions
    pub async fn get_positions(
        &self,
        ctx: Context,
        req: Option<GetPositionsRequest>,
    ) -> GatewayResult<GetPositionsResponse> {
        let (all_spot, all_perp) = self
            .client
            .all_positions(&self.resolve_sub_account(ctx.sub_account_id))
            .await?;

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

    pub async fn get_margin_info(&self, ctx: Context) -> GatewayResult<UserMarginResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        calculate_margin_requirements(
            &self.client,
            &self.client.get_user_account(&sub_account).await?,
        )
        .map(Into::into)
        .map_err(|err| ControllerError::Sdk(err))
    }

    pub async fn get_position_extended(
        &self,
        ctx: Context,
        market: Market,
    ) -> GatewayResult<PerpPosition> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let (perp_position, user, oracle) = tokio::join!(
            self.client.perp_position(&sub_account, market.market_index),
            self.client.get_user_account(&sub_account),
            self.client
                .oracle_price(MarketId::perp(market.market_index)),
        );

        if let Some(perp_position) = perp_position? {
            let result = calculate_liquidation_price_and_unrealized_pnl(
                &self.client,
                &user?,
                market.market_index,
            )?;
            let oracle_price = oracle?;
            let unsettled_pnl = Decimal::new(
                perp_position
                    .get_unrealized_pnl(oracle_price)
                    .unwrap_or_default() as i64,
                PRICE_DECIMALS,
            );

            let mut p: PerpPosition = perp_position.into();
            p.set_extended_info(PerpPositionExtended {
                liquidation_price: Decimal::new(result.liquidation_price, PRICE_DECIMALS),
                unrealized_pnl: Decimal::new(result.unrealized_pnl as i64, PRICE_DECIMALS),
                unsettled_pnl: unsettled_pnl.normalize(),
                oracle_price: Decimal::new(oracle_price, PRICE_DECIMALS),
            });

            Ok(p)
        } else {
            Err(ControllerError::BadRequest("no position".to_string()))
        }
    }

    /// Return orders by market if given, otherwise return all orders
    pub async fn get_orders(
        &self,
        ctx: Context,
        req: Option<GetOrdersRequest>,
    ) -> GatewayResult<GetOrdersResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
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

    pub async fn get_perp_market_info(
        &self,
        market_index: u16,
    ) -> GatewayResult<MarketInfoResponse> {
        let perp = self.client.get_perp_market_info(market_index).await?;
        let open_interest = (perp.get_open_interest() / BASE_PRECISION as u128) as u64;
        let max_open_interest = (perp.amm.max_open_interest / BASE_PRECISION as u128) as u64;

        Ok(MarketInfoResponse {
            open_interest,
            max_open_interest,
        })
    }

    pub async fn cancel_and_place_orders(
        &self,
        ctx: Context,
        req: CancelAndPlaceRequest,
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

        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data?),
            self.delegated,
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit);

        let builder = build_cancel_ix(builder, req.cancel)?;
        let tx = build_modify_ix(builder, req.modify, self.client.program_data())?
            .place_orders(orders)
            .build();

        self.send_tx(tx, "cancel_and_place").await
    }

    pub async fn place_orders(
        &self,
        ctx: Context,
        req: PlaceOrdersRequest,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

        let priority_fee = ctx.cu_price.unwrap_or(pf);
        debug!(target: LOG_TARGET, "priority fee: {priority_fee:?}");

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
            Cow::Owned(account_data?),
            self.delegated,
        )
        .with_priority_fee(priority_fee, ctx.cu_limit)
        .place_orders(orders)
        .build();

        self.send_tx(tx, "place_orders").await
    }

    pub async fn modify_orders(
        &self,
        ctx: Context,
        req: ModifyOrdersRequest,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let (account_data, pf) = tokio::join!(
            self.client.get_user_account(&sub_account),
            get_priority_fee(&self.client)
        );

        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data?),
            self.delegated,
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit);
        let tx = build_modify_ix(builder, req, self.client.program_data())?.build();
        self.send_tx(tx, "modify_orders").await
    }

    pub async fn get_orderbook(&self, req: GetOrderbookRequest) -> GatewayResult<OrderbookL2> {
        let book = self
            .dlob_client
            .get_l2(req.market.as_market_id(), None)
            .await?;
        let decimals = get_market_decimals(self.client.program_data(), req.market);
        Ok(OrderbookL2::new(book, decimals))
    }

    pub async fn get_tx_events_for_subaccount_id(
        &self,
        ctx: Context,
        tx_sig: &str,
    ) -> GatewayResult<TxEventsResponse> {
        let signature = Signature::from_str(tx_sig).map_err(|err| {
            warn!(target: LOG_TARGET, "failed to parse transaction signature: {err:?}");
            ControllerError::BadRequest(format!("failed to parse transaction signature: {err:?}"))
        })?;

        match self
            .client
            .inner()
            .get_transaction_with_config(
                &signature,
                RpcTransactionConfig {
                    encoding: Some(UiTransactionEncoding::Base64),
                    commitment: if self.tx_commitment.is_processed() {
                        Some(CommitmentConfig::confirmed())
                    } else {
                        Some(self.tx_commitment)
                    },
                    max_supported_transaction_version: Some(0),
                },
            )
            .await
        {
            Ok(tx) => {
                let mut events = Vec::new();
                if let Some(meta) = tx.transaction.meta {
                    match meta.log_messages {
                        OptionSerializer::Some(logs) => {
                            let sub_account = self.resolve_sub_account(ctx.sub_account_id);
                            for (tx_idx, log) in logs.iter().enumerate() {
                                if let Some(evt) = try_parse_log(log.as_str(), tx_sig, tx_idx) {
                                    let (_, gw_event) = map_drift_event_for_account(
                                        self.client.program_data(),
                                        &evt,
                                        sub_account,
                                    );
                                    if gw_event.is_none() {
                                        continue;
                                    }
                                    events.push(gw_event.unwrap());
                                }
                            }
                        }
                        OptionSerializer::None | OptionSerializer::Skip => {}
                    }
                }
                Ok(TxEventsResponse::new(events))
            }
            Err(err) => {
                let tx_error = err.get_transaction_error();
                warn!(target: LOG_TARGET, "failed to get transaction: {err:?}, tx_error: {tx_error:?}");
                if matches!(err.kind(), ClientErrorKind::SerdeJson(_)) {
                    Err(ControllerError::TxNotFound {
                        tx_sig: tx_sig.to_string(),
                    })
                } else {
                    Ok(TxEventsResponse::default())
                }
            }
        }
    }

    async fn send_tx(
        &self,
        tx: VersionedMessage,
        reason: &'static str,
    ) -> GatewayResult<TxResponse> {
        let (recent_block_hash, _) = self
            .client
            .inner()
            .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
            .await
            .map_err(SdkError::from)?;
        let tx = self.wallet.sign_tx(tx, recent_block_hash)?;
        let result = self
            .client
            .inner()
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    max_retries: Some(0),
                    preflight_commitment: Some(self.tx_commitment.commitment),
                    ..Default::default()
                },
            )
            .await
            .map(|s| {
                debug!(target: LOG_TARGET, "sent tx ({reason}): {s}");
                TxResponse::new(s.to_string())
            })
            .map_err(|err| {
                warn!(target: LOG_TARGET, "sending tx ({reason}) failed: {err:?}");
                handle_tx_err(err.into())
            });

        // tx has some program/logic error, retry won't fix
        if let Err(ControllerError::TxFailed { .. }) = result {
            return result;
        }

        // double send the tx to help chances of landing
        let client = Arc::clone(&self.client);
        let commitment = self.tx_commitment.commitment;
        tokio::spawn(async move {
            if let Err(err) = client
                .inner()
                .send_transaction_with_config(
                    &tx,
                    RpcSendTransactionConfig {
                        max_retries: Some(0),
                        preflight_commitment: Some(commitment),
                        ..Default::default()
                    },
                )
                .await
            {
                warn!(target: LOG_TARGET, "retry tx failed: {err:?}");
            }
        });

        result
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

/// get priority fee estimated from chain
async fn get_priority_fee<T: AccountProvider>(client: &DriftClient<T>) -> u64 {
    let mut priority_fee = 1_000_u64;
    // use sol-perp market as proxy for local-ish drift fee market
    if let Ok(mut recent_fees) = client
        .get_recent_priority_fees(&[MarketId::perp(0)], Some(100))
        .await
    {
        recent_fees.sort_unstable();
        let idx = (recent_fees.len() * 90) / 100; // 90-th percentile
        priority_fee = recent_fees[idx];
    } else {
        warn!(target: "controller", "failed to fetch live priority fee");
    }

    priority_fee
}

#[cfg(test)]
mod tests {
    use super::*;

    #[ignore]
    #[tokio::test]
    async fn test_pf() {
        // flakey needs a mainnet RPC getProgramAccounts
        let account_provider = RpcAccountProvider::new("https://api.devnet.solana.com");
        let client = DriftClient::new(
            types::Context::DevNet,
            account_provider,
            Wallet::read_only(Pubkey::new_unique()),
        )
        .await
        .unwrap();

        assert!(get_priority_fee(&client).await > 0);
    }
}
