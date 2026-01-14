use std::{
    borrow::Cow,
    collections::HashSet,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use base64::Engine as _;
use drift_rs::{
    constants::{ProgramData, DEFAULT_PUBKEY},
    drift_idl::{self, types::MarginRequirementType},
    event_subscriber::{try_parse_log, CommitmentConfig, RpcClient},
    jupiter::{JupiterSwapApi, SwapMode},
    math::{
        constants::{BASE_PRECISION, MARGIN_PRECISION},
        leverage::get_leverage,
        liquidation::{
            calculate_collateral, calculate_liquidation_price_and_unrealized_pnl,
            calculate_margin_requirements,
        },
    },
    priority_fee_subscriber::{PriorityFeeSubscriber, PriorityFeeSubscriberConfig},
    slot_subscriber::SlotSubscriber,
    titan::{Provider, SwapMode as TitanSwapMode, TitanSwapApi},
    types::{
        self, accounts::SpotMarket, MarketId, MarketType, ModifyOrderParams, OrderParams,
        OrderStatus, ProgramError, RpcSendTransactionConfig, SdkError, SdkResult, VersionedMessage,
    },
    utils::get_http_url,
    DriftClient, Pubkey, TransactionBuilder, Wallet,
};
use futures_util::{
    stream::{FuturesOrdered, FuturesUnordered},
    FutureExt, StreamExt,
};
use log::{debug, info, trace, warn};
use rust_decimal::Decimal;
use sha256::digest;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_rpc_client_api::{
    client_error::ErrorKind as ClientErrorKind,
    config::{RpcAccountInfoConfig, RpcTransactionConfig},
};
use solana_sdk::signature::Signature;
use solana_transaction_status::{option_serializer::OptionSerializer, UiTransactionEncoding};
use thiserror::Error;

use crate::{
    types::{
        get_market_decimals, scale_decimal_to_u64, AllMarketsResponse, AuthorityResponse,
        CancelAndPlaceRequest, CancelOrdersRequest, GetOrdersRequest, GetOrdersResponse,
        GetPositionsRequest, GetPositionsResponse, IncomingSignedMessage, Market,
        MarketInfoResponse, ModifyOrdersRequest, Order, PerpPosition, PerpPositionExtended,
        PlaceOrderResponse, PlaceOrderType, PlaceOrdersRequest, SignedMsgOrderResult,
        SignedMsgResponse, SolBalanceResponse, SpotPosition, SwapRequest, TitanSwapRequest,
        TxEventsResponse, TxResponse, UserCollateralResponse, UserLeverageResponse,
        UserMarginResponse, PRICE_DECIMALS,
    },
    websocket::map_drift_event_for_account,
    Context, LOG_TARGET,
};

/// Default TTL in seconds of gateway tx retry
/// after which gateway will no longer resubmit or monitor the tx
// ~15 slots
const DEFAULT_TX_TTL: u16 = 6;

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
    pub wallet: Arc<Wallet>,
    pub client: Arc<DriftClient>,
    /// Solana tx commitment level for preflight confirmation
    tx_commitment: CommitmentConfig,
    /// sub_account_ids to subscribe to
    sub_account_ids: Vec<u16>,
    /// skip tx preflight on send or not (default: false)
    skip_tx_preflight: bool,
    priority_fee_subscriber: Arc<PriorityFeeSubscriber>,
    slot_subscriber: Arc<SlotSubscriber>,
    /// list of additional RPC endpoints for tx broadcast
    extra_rpcs: Vec<Arc<RpcClient>>,
    /// swift node url
    swift_node: String,
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
    pub fn sub_account(&self, sub_account_id: u16) -> Pubkey {
        self.wallet.sub_account(sub_account_id)
    }
    pub fn resolve_sub_account(&self, sub_account_id: Option<u16>) -> Pubkey {
        self.wallet
            .sub_account(sub_account_id.unwrap_or(self.default_sub_account_id()))
    }

    /// Initialize Gateway Drift client
    ///
    /// * `endpoint` - Solana RPC node HTTP/S endpoint
    /// * `devnet` - whether to run against devnet or not
    /// * `wallet` - wallet to use for tx signing
    /// * `commitment` - Slot finalisation/commitement levels
    /// * `sub_account_ids` - the sub_accounts to subscribe too. In your query specify a specific subaccount, otherwise subaccount 0 will be used as default
    /// * `skip_tx_preflight` - submit txs without checking preflight results
    /// * `extra_rpcs` - list of additional RPC endpoints for tx submission
    pub async fn new(
        endpoint: &str,
        devnet: bool,
        wallet: Wallet,
        commitment: Option<(CommitmentConfig, CommitmentConfig)>,
        sub_account_ids: Vec<u16>,
        skip_tx_preflight: bool,
        extra_rpcs: Vec<&str>,
        swift_node: String,
    ) -> Self {
        let (state_commitment, tx_commitment) =
            commitment.unwrap_or((CommitmentConfig::confirmed(), CommitmentConfig::confirmed()));
        let context = if devnet {
            types::Context::DevNet
        } else {
            types::Context::MainNet
        };

        let rpc_client = RpcClient::new_with_commitment(endpoint.into(), state_commitment);
        let client = DriftClient::new(context, rpc_client, wallet.clone())
            .await
            .expect("ok");

        for sub_account_id in &sub_account_ids {
            let sub_account = wallet.sub_account(*sub_account_id);
            if let Err(err) = client.subscribe_account(&sub_account).await {
                log::error!(target: LOG_TARGET, "couldn't subscribe to user updates: {err:?}. subaccount: {sub_account_id}");
            } else {
                log::info!(target: LOG_TARGET, "subscribed to subaccount: {sub_account}");
            }
        }

        let priority_fee_subscriber = PriorityFeeSubscriber::with_config(
            RpcClient::new_with_commitment(endpoint.into(), state_commitment),
            &[client
                .program_data()
                .perp_market_config_by_index(0)
                .expect("market exists")
                .pubkey],
            PriorityFeeSubscriberConfig {
                refresh_frequency: Some(Duration::from_millis(400 * 10)),
                window: None,
            },
        );

        let priority_fee_subscriber = if wallet.is_read_only() {
            Arc::new(priority_fee_subscriber)
        } else {
            client
                .subscribe_blockhashes()
                .await
                .expect("blockhashes subscribed");
            priority_fee_subscriber.subscribe()
        };

        let mut slot_subscriber = SlotSubscriber::new(client.ws());
        slot_subscriber
            .subscribe(|new_slot| {
                trace!(target: LOG_TARGET, "app_state slot_updated: {:#?}", new_slot);
            })
            .expect("slot subscribed");
        Self {
            client: Arc::new(client),
            tx_commitment,
            sub_account_ids,
            skip_tx_preflight,
            priority_fee_subscriber,
            slot_subscriber: Arc::new(slot_subscriber),
            wallet: Arc::new(wallet),
            extra_rpcs: extra_rpcs
                .into_iter()
                .map(|u| Arc::new(RpcClient::new(get_http_url(u).expect("valid RPC url"))))
                .collect(),
            swift_node,
        }
    }

    pub(crate) async fn sync_market_subscriptions_on_user_changes(
        &self,
        configured_markets: &[MarketId],
    ) -> Result<(), SdkError> {
        let sub_account_ids = self.sub_account_ids.clone();
        for id in sub_account_ids {
            self.sync_market_subscriptions_on_user_subaccount_changes(configured_markets, id)
                .await?;
        }

        Ok(())
    }

    async fn sync_market_subscriptions_on_user_subaccount_changes(
        &self,
        configured_markets: &[MarketId],
        sub_account_id: u16,
    ) -> Result<(), SdkError> {
        let sub_account = self.sub_account(sub_account_id);
        let state_commitment = self.tx_commitment;
        let configured_markets_vec = configured_markets.to_vec();
        let self_clone = self.clone();
        let mut current_user_markets_to_subscribe =
            self.get_marketids_to_subscribe(sub_account).await?;

        tokio::spawn(async move {
            let pubsub_config = RpcAccountInfoConfig {
                commitment: Some(state_commitment),
                data_slice: None,
                encoding: Some(UiAccountEncoding::JsonParsed),
                min_context_slot: None,
            };

            let pubsub_client = self_clone.client.ws();

            let (mut account_subscription, unsubscribe_fn) = match pubsub_client
                .account_subscribe(&sub_account, Some(pubsub_config))
                .await
            {
                Ok(res) => res,
                Err(err) => {
                    warn!(target: LOG_TARGET, "failed to subscribe to account: {err:?}");
                    return;
                }
            };

            info!(target: LOG_TARGET, "Pubsub successfully subscribed to user account updates!");

            // Process incoming account updates
            while let Some(_) = account_subscription.next().await {
                let current_market_ids_count = current_user_markets_to_subscribe.len();
                match self_clone.get_marketids_to_subscribe(sub_account).await {
                    Ok(new_market_ids) => {
                        if new_market_ids.len() != current_market_ids_count {
                            if let Err(err) = self_clone
                                .subscribe_market_data(&configured_markets_vec)
                                .await
                            {
                                warn!(target: LOG_TARGET, "error refreshing market subscriptions: {err:?}");
                            } else {
                                debug!(target: LOG_TARGET, "market subscriptions refreshed due to updated position/order state");
                            }
                        }

                        current_user_markets_to_subscribe = new_market_ids;
                    }
                    Err(err) => {
                        warn!(target: LOG_TARGET, "error getting user account during market_data sync: {err:?}");
                    }
                }
            }

            unsubscribe_fn().await;
            warn!(target: LOG_TARGET, "market subscriptions no longer synced with user account changes");
        });

        Ok(())
    }

    async fn get_marketids_to_subscribe(
        &self,
        sub_account: Pubkey,
    ) -> Result<Vec<MarketId>, SdkError> {
        let (all_spot, all_perp) = self.client.all_positions(&sub_account).await?;

        let open_orders = self.client.all_orders(&sub_account).await?;

        let user_markets: Vec<MarketId> = all_spot
            .iter()
            .map(|s| MarketId::spot(s.market_index))
            .chain(all_perp.iter().map(|p| MarketId::perp(p.market_index)))
            .chain(open_orders.iter().map(|o| {
                if o.market_type == MarketType::Spot {
                    MarketId::spot(o.market_index)
                } else {
                    MarketId::perp(o.market_index)
                }
            }))
            .collect();

        Ok(user_markets)
    }

    /// Start market and oracle data subscriptions
    ///
    /// * configured_markets - list of static markets provided by user
    ///
    /// additional subscriptions will be included based on user's current positions (on default sub-account)

    pub(crate) async fn subscribe_market_data(
        &self,
        configured_markets: &[MarketId],
    ) -> Result<(), SdkError> {
        for id in self.sub_account_ids.clone() {
            self.subscribe_market_data_for_subaccount(configured_markets, id)
                .await?;
        }
        Ok(())
    }

    async fn subscribe_market_data_for_subaccount(
        &self,
        configured_markets: &[MarketId],
        sub_account_id: u16,
    ) -> Result<(), SdkError> {
        let sub_account = self.sub_account(sub_account_id);
        let mut user_markets = self.get_marketids_to_subscribe(sub_account).await?;
        user_markets.extend_from_slice(configured_markets);

        let init_rpc_throttle: u64 = std::env::var("INIT_RPC_THROTTLE")
            .map(|s| s.parse().unwrap())
            .unwrap_or(1);

        let markets = Vec::from_iter(HashSet::<MarketId>::from_iter(user_markets).into_iter());
        info!(target: LOG_TARGET, "start market subscriptions: {markets:?}");
        tokio::time::sleep(Duration::from_secs(init_rpc_throttle)).await;
        self.client.subscribe_oracles(&markets).await?;
        tokio::time::sleep(Duration::from_secs(init_rpc_throttle)).await;
        self.client.subscribe_markets(&markets).await?;

        Ok(())
    }

    /// Return SOL balance of the tx signing account
    pub async fn get_sol_balance(&self) -> GatewayResult<SolBalanceResponse> {
        let pubkey = self.authority();
        let balance = self
            .client
            .rpc()
            .get_balance(pubkey)
            .await
            .map_err(|err| ControllerError::Sdk(err.into()))?;
        Ok(SolBalanceResponse {
            balance: Decimal::new(balance as i64, BASE_PRECISION.ilog10()).normalize(),
            pubkey: pubkey.to_string(),
        })
    }

    /// Return Pubkey of Authority (signer)
    pub fn get_authority(&self) -> GatewayResult<AuthorityResponse> {
        let pubkey = self.wallet.authority().to_string();
        Ok(AuthorityResponse { pubkey })
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
        let account_data = self.client.get_user_account(&sub_account).await?;
        let pf = self.get_priority_fee();

        let priority_fee = ctx.cu_price.unwrap_or(pf);
        debug!(target: LOG_TARGET, "priority_fee: {priority_fee:?}");
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .with_priority_fee(priority_fee, ctx.cu_limit);
        let tx = build_cancel_ix(builder, req)?.build();
        self.send_tx(tx, "cancel_orders", ctx.ttl).await
    }

    /// Return position for market if given, otherwise return all positions
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
        let filtered_spot_positions: Vec<&drift_idl::types::SpotPosition> = all_spot
            .iter()
            .filter(|p| {
                if let Some(GetPositionsRequest { ref market }) = req {
                    p.market_index == market.market_index && MarketType::Spot == market.market_type
                } else {
                    true
                }
            })
            .collect();

        let spot_market_futs = filtered_spot_positions
            .iter()
            .map(|x| self.client.get_spot_market_account(x.market_index));

        let spot_market_futs = FuturesOrdered::from_iter(spot_market_futs);
        let spot_markets = spot_market_futs
            .collect::<Vec<SdkResult<SpotMarket>>>()
            .await;

        let filtered_spot_positions = filtered_spot_positions
            .iter()
            .zip(spot_markets.iter())
            .map(|(position, market)| {
                SpotPosition::from_sdk_type(position, market.as_ref().expect("spot market"))
            })
            .collect();

        Ok(GetPositionsResponse {
            spot: filtered_spot_positions,
            perp: all_perp
                .into_iter()
                .filter(|p| {
                    if let Some(GetPositionsRequest { ref market }) = req {
                        p.market_index == market.market_index
                            && MarketType::Perp == market.market_type
                    } else {
                        true
                    }
                })
                .map(Into::into)
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
        .map_err(ControllerError::Sdk)
    }

    pub async fn get_leverage(&self, ctx: Context) -> GatewayResult<UserLeverageResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        get_leverage(
            &self.client,
            &self.client.get_user_account(&sub_account).await?,
        )
        .map(Into::into)
        .map_err(ControllerError::Sdk)
    }

    pub async fn get_collateral(
        &self,
        ctx: Context,
        margin_requirement_type: MarginRequirementType,
    ) -> GatewayResult<UserCollateralResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        calculate_collateral(
            &self.client,
            &self.client.get_user_account(&sub_account).await?,
            margin_requirement_type,
        )
        .map(Into::into)
        .map_err(ControllerError::Sdk)
    }

    pub async fn get_position_extended(
        &self,
        ctx: Context,
        market: Market,
    ) -> GatewayResult<PerpPosition> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let user = self.client.get_user_account(&sub_account).await?;

        let perp_position = user
            .perp_positions
            .iter()
            .find(|p| p.market_index == market.market_index && !p.is_available());

        if let Some(perp_position) = perp_position {
            let calc = calculate_liquidation_price_and_unrealized_pnl(
                &self.client,
                &user,
                market.market_index,
            )
            .await?;
            let unsettled_pnl = Decimal::from_i128_with_scale(
                perp_position
                    .get_unrealized_pnl(calc.oracle_price)
                    .unwrap_or_default(),
                PRICE_DECIMALS,
            );

            let mut p: PerpPosition = (*perp_position).into();
            p.set_extended_info(PerpPositionExtended {
                liquidation_price: Decimal::new(calc.liquidation_price, PRICE_DECIMALS),
                unrealized_pnl: Decimal::new(calc.unrealized_pnl as i64, PRICE_DECIMALS),
                unsettled_pnl: unsettled_pnl.normalize(),
                oracle_price: Decimal::new(calc.oracle_price, PRICE_DECIMALS),
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
        let user = self.client.get_user_account(&sub_account).await?;

        let orders: Vec<types::Order> = user
            .orders
            .into_iter()
            .filter(|o| o.status == OrderStatus::Open)
            .collect();

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
        let perp = self.client.get_perp_market_account(market_index).await?;
        let open_interest = (perp.get_open_interest() / BASE_PRECISION) as u64;
        let max_open_interest = (perp.amm.max_open_interest.as_u128() / BASE_PRECISION) as u64;

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
        let account_data = self.client.get_user_account(&sub_account).await?;
        let pf = self.get_priority_fee();

        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit);

        let builder = build_cancel_ix(builder, req.cancel)?;
        let tx = build_modify_ix(builder, req.modify, self.client.program_data())?
            .place_orders(orders)
            .build();

        self.send_tx(tx, "cancel_and_place", ctx.ttl).await
    }

    pub async fn place_orders(
        &self,
        ctx: Context,
        req: PlaceOrdersRequest,
    ) -> GatewayResult<PlaceOrderResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let account_data = self.client.get_user_account(&sub_account).await?;
        let pf = self.get_priority_fee();
        let priority_fee = ctx.cu_price.unwrap_or(pf);
        debug!(target: LOG_TARGET, "priority fee: {priority_fee:?}");

        let orders_iter = req.orders.into_iter();
        match req.place_order_type {
            PlaceOrderType::Tx => {
                let orders = orders_iter
                    .map(|o| {
                        let base_decimals =
                            get_market_decimals(self.client.program_data(), o.market);
                        o.to_order_params(base_decimals)
                    })
                    .collect();
                let tx = TransactionBuilder::new(
                    self.client.program_data(),
                    sub_account,
                    Cow::Owned(account_data),
                    self.wallet.is_delegated(),
                )
                .with_priority_fee(priority_fee, ctx.cu_limit)
                .place_orders(orders)
                .build();

                let tx_res = self.send_tx(tx, "place_orders", ctx.ttl).await;
                match tx_res {
                    Ok(tx_res) => Ok(PlaceOrderResponse::Tx(tx_res)),
                    Err(e) => Err(e),
                }
            }
            PlaceOrderType::SignedMsg => {
                let orders_len = orders_iter.len();
                let mut signed_messages = Vec::with_capacity(orders_len);
                let mut hashes: Vec<String> = Vec::with_capacity(orders_len);
                let sub_account_id = ctx.sub_account_id.unwrap_or(self.default_sub_account_id());
                let current_slot = self.slot_subscriber.current_slot();
                let orders_with_hex: Vec<(OrderParams, Vec<u8>)> = orders_iter
                    .map(|order| {
                        let base_decimals =
                            get_market_decimals(self.client.program_data(), order.market);
                        let order_for_signing_hex = order.clone();
                        let order_params = order.to_order_params(base_decimals);
                        (
                            order_params,
                            order_for_signing_hex.to_signed_order_hex(
                                order_params,
                                current_slot,
                                sub_account_id,
                            ),
                        )
                    })
                    .collect();

                for (order, message) in orders_with_hex {
                    let signature = self.wallet.sign_message(message.as_slice())?;
                    let market_type: &'static str = match order.market_type {
                        MarketType::Spot => "spot",
                        MarketType::Perp => "perp",
                    };
                    let incoming_msg = IncomingSignedMessage {
                        taker_authority: self.authority().to_string(),
                        signature: base64::prelude::BASE64_STANDARD.encode(signature),
                        message: String::from_utf8(message).unwrap(),
                        signing_authority: self.signer().to_string(),
                        market_type,
                        market_index: order.market_index,
                    };

                    signed_messages.push(incoming_msg);
                    let hash = digest(signature.as_ref());
                    hashes.push(hash);
                }

                let client = reqwest::Client::new();

                let swift_orders_url = self.swift_node.clone() + "/orders";

                let mut futures = FuturesOrdered::new();
                for msg in signed_messages {
                    let future =
                        client
                            .post(&swift_orders_url)
                            .json(&msg)
                            .send()
                            .then(|resp| async move {
                                match resp {
                                    Ok(response) => {
                                        let status = response.status();
                                        let response_text =
                                            response.text().await.unwrap_or_default();
                                        (status.to_string(), response_text)
                                    }
                                    Err(e) => {
                                        ("500".to_string(), format!("swift server error: {:?}", e))
                                    }
                                }
                            });
                    futures.push_back(future);
                }

                let responses: Vec<_> = futures.collect().await;

                let signed_msg = SignedMsgResponse {
                    results: hashes
                        .iter()
                        .zip(responses)
                        .map(|(hash, (status, response))| SignedMsgOrderResult {
                            hash: hash.clone(),
                            status: status.clone(),
                            error: Some(response.clone()),
                        })
                        .collect(),
                };

                Ok(PlaceOrderResponse::SignedMsg(signed_msg))
            }
        }
    }

    pub async fn modify_orders(
        &self,
        ctx: Context,
        req: ModifyOrdersRequest,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);
        let account_data = self.client.get_user_account(&sub_account).await?;
        let pf = self.get_priority_fee();
        let builder = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit);
        let tx = build_modify_ix(builder, req, self.client.program_data())?.build();
        self.send_tx(tx, "modify_orders", ctx.ttl).await
    }

    pub async fn swap(&self, ctx: Context, req: SwapRequest) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);

        let in_market = self
            .client
            .program_data()
            .spot_market_config_by_index(req.input_market)
            .unwrap();
        let out_market = self
            .client
            .program_data()
            .spot_market_config_by_index(req.output_market)
            .unwrap();

        let (swap_mode, amount) = if req.exact_in {
            (
                SwapMode::ExactIn,
                scale_decimal_to_u64(req.amount.abs(), 10_u32.pow(in_market.decimals)),
            )
        } else {
            (
                SwapMode::ExactOut,
                scale_decimal_to_u64(req.amount.abs(), 10_u32.pow(out_market.decimals)),
            )
        };

        let signer = self.wallet.signer();
        let (jupiter_swap_info, account_data) = tokio::try_join!(
            self.client.jupiter_swap_query(
                &signer,
                amount,
                swap_mode,
                req.slippage_bps,
                req.input_market,
                req.output_market,
                req.use_direct_routes,
                req.exclude_dexes,
                Default::default(),
            ),
            self.client.get_user_account(&sub_account)
        )?;
        let pf = self.get_priority_fee();

        let tx = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .jupiter_swap(
            jupiter_swap_info,
            in_market,
            out_market,
            &Wallet::derive_associated_token_address(&signer, in_market),
            &Wallet::derive_associated_token_address(&signer, out_market),
            None,
            None,
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit)
        .build();

        self.send_tx(tx, "swap", ctx.ttl).await
    }

    pub async fn titan_swap(
        &self,
        ctx: Context,
        req: TitanSwapRequest,
    ) -> GatewayResult<TxResponse> {
        let sub_account = self.resolve_sub_account(ctx.sub_account_id);

        let in_market = self
            .client
            .program_data()
            .spot_market_config_by_index(req.input_market)
            .unwrap();
        let out_market = self
            .client
            .program_data()
            .spot_market_config_by_index(req.output_market)
            .unwrap();

        let (swap_mode, amount) = if req.exact_in {
            (
                TitanSwapMode::ExactIn,
                scale_decimal_to_u64(req.amount.abs(), 10_u32.pow(in_market.decimals)),
            )
        } else {
            (
                TitanSwapMode::ExactOut,
                scale_decimal_to_u64(req.amount.abs(), 10_u32.pow(out_market.decimals)),
            )
        };

        let signer = self.wallet.signer();
        let (titan_swap_info, account_data) = tokio::try_join!(
            self.client.titan_swap_query(
                &signer,
                amount,
                req.max_accounts,
                swap_mode,
                req.slippage_bps,
                req.input_market,
                req.output_market,
                req.use_direct_routes,
                req.exclude_dexes,
                Some(Provider::Titan),
            ),
            self.client.get_user_account(&sub_account)
        )?;
        let pf = self.get_priority_fee();

        let tx = TransactionBuilder::new(
            self.client.program_data(),
            sub_account,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .titan_swap(
            titan_swap_info,
            in_market,
            out_market,
            &Wallet::derive_associated_token_address(&signer, in_market),
            &Wallet::derive_associated_token_address(&signer, out_market),
            None,
            None,
        )
        .with_priority_fee(ctx.cu_price.unwrap_or(pf), ctx.cu_limit)
        .build();

        self.send_tx(tx, "titan_swap", ctx.ttl).await
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
            .rpc()
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
                Ok(TxEventsResponse::new(events, true, None))
            }
            Err(err) => {
                warn!(target: LOG_TARGET, "failed to get transaction: {err:?}, tx_error: {err:?}");
                match err.kind() {
                    ClientErrorKind::SerdeJson(_) => Err(ControllerError::TxNotFound {
                        tx_sig: tx_sig.to_string(),
                    }),
                    _ => {
                        let err: SdkError = err.into();

                        if let Some(program_error) = err.to_anchor_error_code() {
                            return Ok(TxEventsResponse::new(
                                Default::default(),
                                false,
                                Some(format!("program error: {program_error}")),
                            ));
                        }
                        if err.to_out_of_sol_error().is_some() {
                            return Ok(TxEventsResponse::new(
                                Default::default(),
                                false,
                                Some("ouf of sol, top-up account".into()),
                            ));
                        }

                        Ok(TxEventsResponse::new(
                            Default::default(),
                            false,
                            Some(err.to_string()),
                        ))
                    }
                }
            }
        }
    }

    pub async fn set_margin_ratio(
        &self,
        ctx: Context,
        new_margin_ratio: Decimal,
    ) -> GatewayResult<TxResponse> {
        let sub_account_id = ctx.sub_account_id.unwrap_or(self.default_sub_account_id());
        let sub_account_address = self.wallet.sub_account(sub_account_id);
        let account_data = self.client.get_user_account(&sub_account_address).await?;

        let margin_ratio =
            Decimal::new(MARGIN_PRECISION as i64, MARGIN_PRECISION.ilog10()) / new_margin_ratio;

        let tx = TransactionBuilder::new(
            self.client.program_data(),
            sub_account_address,
            Cow::Owned(account_data),
            self.wallet.is_delegated(),
        )
        .set_max_initial_margin_ratio(
            margin_ratio.mantissa().unsigned_abs() as u32,
            sub_account_id,
        )
        .build();
        self.send_tx(tx, "set_margin_ratio", ctx.ttl).await
    }

    pub fn default_sub_account_id(&self) -> u16 {
        self.sub_account_ids[0]
    }

    fn get_priority_fee(&self) -> u64 {
        self.priority_fee_subscriber.priority_fee_nth(0.9)
    }

    /// Test tx simulate only
    #[cfg(test)]
    async fn send_tx(
        &self,
        tx: VersionedMessage,
        reason: &'static str,
        _ttl: Option<u16>,
    ) -> GatewayResult<TxResponse> {
        match self.client.simulate_tx(tx).await?.err {
            Some(err) => {
                log::error!("test tx failed: {err:?}");
                Err(ControllerError::TxFailed {
                    reason: reason.into(),
                    code: 0,
                })
            }
            None => Ok(TxResponse::new("".into())),
        }
    }

    #[cfg(not(test))]
    async fn send_tx(
        &self,
        tx: VersionedMessage,
        reason: &'static str,
        ttl: Option<u16>,
    ) -> GatewayResult<TxResponse> {
        let recent_block_hash = self.client.get_latest_blockhash().await?;
        let tx = self.wallet.sign_tx(tx, recent_block_hash)?;
        let tx_config = RpcSendTransactionConfig {
            max_retries: Some(0),
            preflight_commitment: Some(self.tx_commitment.commitment),
            skip_preflight: self.skip_tx_preflight,
            ..Default::default()
        };

        // submit to primary RPC first,
        let sig = self
            .client
            .rpc()
            .send_transaction_with_config(&tx, tx_config)
            .await
            .inspect(|s| {
                debug!(target: LOG_TARGET, "sent tx ({reason}): {s}");
            })
            .map_err(|err| {
                warn!(target: LOG_TARGET, "sending tx ({reason}) failed: {err:?}");
                // tx has some program/logic error, retry won't fix
                handle_tx_err(err.into())
            })?;

        // start a dedicated tx sending task
        // - tx is broadcast to all available RPCs
        // - retried at set intervals
        // - retried upto some given deadline
        // client should poll for the tx to confirm success
        let primary_rpc = Arc::clone(&self.client).rpc();
        let tx_signature = sig;
        let extra_rpcs = self.extra_rpcs.clone();
        tokio::spawn(async move {
            let start = SystemTime::now();
            let ttl = Duration::from_secs(ttl.unwrap_or(DEFAULT_TX_TTL) as u64);
            let mut confirmed = false;
            while SystemTime::now()
                .duration_since(start)
                .is_ok_and(|x| x < ttl)
            {
                let mut futs = FuturesUnordered::new();
                for rpc in extra_rpcs.iter() {
                    futs.push(rpc.send_transaction_with_config(&tx, tx_config));
                }
                futs.push(primary_rpc.send_transaction_with_config(&tx, tx_config));

                while let Some(res) = futs.next().await {
                    match res {
                        Ok(sig) => {
                            debug!(target: LOG_TARGET, "sent tx ({reason}): {sig}");
                        }
                        Err(err) => {
                            warn!(target: LOG_TARGET, "sending tx ({reason}) failed: {err:?}");
                        }
                    }
                }

                tokio::time::sleep(Duration::from_millis(800)).await;

                if let Ok(Some(Ok(()))) = primary_rpc.get_signature_status(&tx_signature).await {
                    confirmed = true;
                    info!(target: LOG_TARGET, "tx confirmed onchain: {tx_signature:?}");
                    break;
                }
            }
            if !confirmed {
                warn!(target: LOG_TARGET, "tx was not confirmed: {tx_signature:?}");
            }
        });

        Ok(TxResponse::new(sig.to_string()))
    }
}

fn handle_tx_err(err: SdkError) -> ControllerError {
    if let Some(program_err) = err.to_anchor_error_code() {
        match program_err {
            ProgramError::Drift(code) => ControllerError::TxFailed {
                reason: code.name(),
                code: code.into(),
            },
            ProgramError::Other { ix_idx, code } => ControllerError::TxFailed {
                reason: format!("ix idx: {ix_idx}"),
                code,
            },
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
        (Some(secret_key), _, None) => Wallet::try_from_str(secret_key).expect("valid key"),
        (Some(secret_key), _, Some(delegate)) => {
            let keypair =
                drift_rs::utils::load_keypair_multi_format(secret_key).expect("valid key");
            Wallet::delegated(keypair, delegate)
        }
        (None, Some(emulate), None) => Wallet::read_only(emulate),
        _ => {
            panic!("expected 'DRIFT_GATEWAY_KEY' or --emulate <pubkey>");
        }
    }
}
