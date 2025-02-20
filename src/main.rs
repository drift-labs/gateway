use std::{borrow::Borrow, str::FromStr, sync::Arc, time::Duration};

use actix_web::{
    delete, get,
    middleware::Logger,
    patch, post,
    web::{self, Json},
    App, Either, HttpResponse, HttpServer, Responder,
};
use argh::FromArgs;
use drift_rs::{
    types::{CommitmentConfig, MarginRequirementType, MarketId},
    Pubkey,
};
use log::{debug, info, warn};
use serde_json::json;

use crate::{
    controller::{create_wallet, AppState, ControllerError},
    types::{
        CancelAndPlaceRequest, CancelOrdersRequest, Market, ModifyOrdersRequest, PlaceOrdersRequest,
    },
};

mod controller;
mod types;
mod websocket;

pub const LOG_TARGET: &str = "gateway";

/// Request context
#[derive(serde::Deserialize, Default)]
struct Context {
    #[serde(default, rename = "subAccountId")]
    pub sub_account_id: Option<u16>,
    #[serde(default, rename = "computeUnitLimit")]
    pub cu_limit: Option<u32>,
    #[serde(default, rename = "computeUnitPrice")]
    pub cu_price: Option<u64>,
    /// Tx retry TTL
    #[serde(default, rename = "ttl")]
    pub ttl: Option<u16>,
}

#[get("/markets")]
async fn get_markets(controller: web::Data<AppState>) -> impl Responder {
    let markets = controller.get_markets();
    Json(markets)
}

#[get("/marketInfo/{index}")]
async fn get_market_info(controller: web::Data<AppState>, path: web::Path<u16>) -> impl Responder {
    handle_result(controller.get_perp_market_info(*path).await)
}

#[get("/orders")]
async fn get_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    let mut req = None;
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = Some(deser),
            Err(err) => return handle_deser_error(err),
        }
    };

    handle_result(controller.get_orders(ctx.0, req).await)
}

#[post("/orders")]
async fn create_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    match serde_json::from_slice::<'_, PlaceOrdersRequest>(body.as_ref()) {
        Ok(req) => {
            debug!(target: LOG_TARGET, "request: {req:?}");
            handle_result(controller.place_orders(ctx.0, req).await)
        }
        Err(err) => handle_deser_error(err),
    }
}

#[patch("/orders")]
async fn modify_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    match serde_json::from_slice::<'_, ModifyOrdersRequest>(body.as_ref()) {
        Ok(req) => {
            debug!(target: LOG_TARGET, "request: {req:?}");
            handle_result(controller.modify_orders(ctx.0, req).await)
        }
        Err(err) => handle_deser_error(err),
    }
}

#[delete("/orders")]
async fn cancel_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    let mut req = CancelOrdersRequest::default();
    // handle the body manually to allow empty payload `Json` requires some body is set
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => return handle_deser_error(err),
        }
    };
    debug!(target: LOG_TARGET, "request: {req:?}");
    handle_result(controller.cancel_orders(ctx.0, req).await)
}

#[post("/orders/cancelAndPlace")]
async fn cancel_and_place_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    match serde_json::from_slice::<'_, CancelAndPlaceRequest>(body.as_ref()) {
        Ok(req) => {
            debug!(target: LOG_TARGET, "request: {req:?}");
            handle_result(controller.cancel_and_place_orders(ctx.0, req).await)
        }
        Err(err) => handle_deser_error(err),
    }
}

#[get("/positions")]
async fn get_positions(
    controller: web::Data<AppState>,
    body: web::Bytes,
    ctx: web::Query<Context>,
) -> impl Responder {
    let mut req = None;
    // handle the body manually to allow empty payload `Json` requires some body is set
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = Some(deser),
            Err(err) => return handle_deser_error(err),
        }
    };

    handle_result(controller.get_positions(ctx.0, req).await)
}

#[get("/positionInfo/{index}")]
async fn get_positions_extended(
    controller: web::Data<AppState>,
    path: web::Path<u16>,
    ctx: web::Query<Context>,
) -> impl Responder {
    let index = path.into_inner();
    handle_result(
        controller
            .get_position_extended(ctx.0, Market::perp(index))
            .await,
    )
}

#[get("/balance")]
async fn get_sol_balance(controller: web::Data<AppState>) -> impl Responder {
    handle_result(controller.get_sol_balance().await)
}

#[get("/transactionEvent/{tx_sig}")]
async fn get_tx_events(
    controller: web::Data<AppState>,
    path: web::Path<String>,
    ctx: web::Query<Context>,
) -> impl Responder {
    let tx_sig = path.into_inner();
    handle_result(
        controller
            .get_tx_events_for_subaccount_id(ctx.0, tx_sig.as_str())
            .await,
    )
}

#[get("/user/marginInfo")]
async fn get_margin_info(
    controller: web::Data<AppState>,
    ctx: web::Query<Context>,
) -> impl Responder {
    handle_result(controller.get_margin_info(ctx.0).await)
}

#[get("/leverage")]
async fn get_leverage(controller: web::Data<AppState>, ctx: web::Query<Context>) -> impl Responder {
    handle_result(controller.get_leverage(ctx.0).await)
}

#[get("/collateral")]
async fn get_collateral(
    controller: web::Data<AppState>,
    ctx: web::Query<Context>,
) -> impl Responder {
    handle_result(
        controller
            .get_collateral(ctx.0, MarginRequirementType::Maintenance)
            .await,
    )
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config: GatewayConfig = argh::from_env();

    let mut logger = env_logger::Builder::from_default_env();
    if config.verbose {
        logger
            .filter_module(LOG_TARGET, log::LevelFilter::Debug)
            .filter_module("rpc", log::LevelFilter::Debug)
            .filter_module("ws", log::LevelFilter::Debug)
            .filter_module("wsaccsub", log::LevelFilter::Debug)
            .filter_module("marketmap", log::LevelFilter::Debug)
            .filter_module("oraclemap", log::LevelFilter::Debug)
    } else {
        logger.filter_module(LOG_TARGET, log::LevelFilter::Info)
    }
    .init();

    let secret_key = std::env::var("DRIFT_GATEWAY_KEY");
    let delegate = config
        .delegate
        .map(|ref x| Pubkey::from_str(x).expect("valid pubkey"));
    let emulate = config
        .emulate
        .map(|ref x| Pubkey::from_str(x).expect("valid pubkey"));
    let wallet = create_wallet(secret_key.ok(), emulate, delegate);
    let state_commitment = CommitmentConfig::from_str(&config.commitment)
        .expect("one of: processed | confirmed | finalized");
    let tx_commitment = CommitmentConfig::from_str(&config.tx_commitment)
        .expect("one of: processed | confirmed | finalized");
    let extra_rpcs = config.extra_rpcs.as_ref();
    let state = AppState::new(
        &config.rpc_host,
        config.dev,
        wallet,
        Some((state_commitment, tx_commitment)),
        Some(config.default_sub_account_id),
        config.skip_tx_preflight,
        extra_rpcs
            .map(|s| s.split(",").collect())
            .unwrap_or_default(),
    )
    .await;

    // start market+oracle subs
    let mut markets = Vec::<MarketId>::default();
    if let Some(ref user_markets) = config.markets {
        markets.extend(parse_markets(&state.client, user_markets).expect("valid markets"));
    };
    state.subscribe_market_data(&markets).await;
    info!(target: LOG_TARGET, "subscribed to market data updates 🛜");

    info!(
        target: LOG_TARGET,
        "🏛️ gateway listening at http://{}:{}", config.host, config.port
    );

    if delegate.is_some() {
        info!(
            target: LOG_TARGET,
            "🪪 authority: {:?}, default sub-account: {:?}, 🔑 delegate: {:?}",
            state.authority(),
            state.default_sub_account(),
            state.signer(),
        );
    } else {
        info!(
            target: LOG_TARGET,
            "🪪 authority: {:?}, default sub-account: {:?}",
            state.authority(),
            state.default_sub_account()
        );
        if emulate.is_some() {
            warn!("using emulation mode, tx signing unavailable");
        }
    }

    let client = Box::leak(Box::new(Arc::clone(state.client.borrow())));
    websocket::start_ws_server(
        format!("{}:{}", &config.host, config.ws_port).as_str(),
        client.ws(),
        state.wallet.inner().clone(),
        client.program_data(),
    )
    .await;

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::new("%a | %s | %r | (%Dms)").log_target(LOG_TARGET))
            .app_data(web::Data::new(state.clone()))
            .service(
                web::scope("/v2")
                    .service(get_markets)
                    .service(get_positions)
                    .service(get_orders)
                    .service(create_orders)
                    .service(cancel_orders)
                    .service(modify_orders)
                    .service(cancel_and_place_orders)
                    .service(get_sol_balance)
                    .service(get_positions_extended)
                    .service(get_tx_events)
                    .service(get_market_info)
                    .service(get_margin_info)
                    .service(get_leverage)
                    .service(get_collateral),
            )
    })
    .keep_alive(Duration::from_secs(config.keep_alive_timeout as u64))
    .bind((config.host, config.port))?
    .run()
    .await
}

fn handle_result<T: std::fmt::Debug>(
    result: Result<T, ControllerError>,
) -> Either<HttpResponse, Json<T>> {
    debug!(target: LOG_TARGET, "response: {result:?}");
    match result {
        Ok(payload) => Either::Right(Json(payload)),
        Err(ControllerError::Sdk(err)) => {
            Either::Left(HttpResponse::InternalServerError().json(json!(
                {
                    "code": 500,
                    "reason": err.to_string(),
                }
            )))
        }
        Err(ControllerError::TxFailed { code, reason }) => {
            Either::Left(HttpResponse::BadRequest().json(json!(
                {
                    "code": code,
                    "reason": reason,
                }
            )))
        }
        Err(ControllerError::BadRequest(reason)) => {
            Either::Left(HttpResponse::BadRequest().json(json!(
                {
                    "code": 400,
                    "reason": reason,
                }
            )))
        }
        Err(ControllerError::TxNotFound { tx_sig }) => {
            Either::Left(HttpResponse::NotFound().json(json!(
                {
                    "code": 404,
                    "reason": format!("tx not found: {}", tx_sig),
                }
            )))
        }
    }
}

fn handle_deser_error<T>(err: serde_json::Error) -> Either<HttpResponse, Json<T>> {
    Either::Left(HttpResponse::BadRequest().json(json!(
        {
            "code": 400,
            "reason": err.to_string(),
        }
    )))
}

#[derive(FromArgs)]
/// Drift gateway server
struct GatewayConfig {
    /// the solana RPC URL
    #[argh(positional)]
    rpc_host: String,
    /// list of markets to trade
    /// e.g '--markets sol-perp,wbtc,pyusd'
    /// gateway creates market subscriptions for responsive trading
    #[argh(option)]
    markets: Option<String>,
    /// run in devnet mode
    #[argh(switch)]
    dev: bool,
    /// gateway host address
    #[argh(option, default = "String::from(\"127.0.0.1\")")]
    host: String,
    /// gateway port
    #[argh(option, default = "8080")]
    port: u16,
    /// gateway Ws port
    #[argh(option, default = "1337")]
    ws_port: u16,
    /// http keep-alive timeout in seconds
    #[argh(option, default = "3600")]
    keep_alive_timeout: u32,
    /// use delegated signing mode
    /// provide the delegator's pubkey (i.e the main account)
    /// 'DRIFT_GATEWAY_KEY' should be set to the delegate's private key
    #[argh(option)]
    delegate: Option<String>,
    /// run the gateway in read-only mode for given authority pubkey
    #[argh(option)]
    emulate: Option<String>,
    /// solana commitment level to use for transaction confirmation (default: confirmed)
    #[argh(option, default = "String::from(\"confirmed\")")]
    tx_commitment: String,
    /// solana commitment level to use for state updates (default: confirmed)
    #[argh(option, default = "String::from(\"confirmed\")")]
    commitment: String,
    /// default sub_account_id to use (default: 0)
    #[argh(option, default = "0")]
    default_sub_account_id: u16,
    /// skip tx preflight checks
    #[argh(switch)]
    skip_tx_preflight: bool,
    /// extra solana RPC urls for improved Tx broadcast
    #[argh(option)]
    extra_rpcs: Option<String>,
    /// enable debug logging
    #[argh(switch)]
    verbose: bool,
}

/// Parse raw markets list from user command
fn parse_markets(client: &drift_rs::DriftClient, markets: &str) -> Result<Vec<MarketId>, ()> {
    let mut configured_markets = Vec::<MarketId>::default();

    for ticker in markets.split(",") {
        if let Some(market) = client.market_lookup(ticker) {
            configured_markets.push(market);
        } else {
            log::error!(target: LOG_TARGET, "invalid market: {ticker:?}");
            return Err(());
        }
    }

    Ok(configured_markets)
}

#[cfg(test)]
mod tests {
    use actix_web::{http::Method, test, App};

    use self::controller::create_wallet;
    use super::*;

    fn get_seed() -> String {
        std::env::var("DRIFT_GATEWAY_KEY")
            .expect("DRIFT_GATEWAY_KEY is set")
            .to_string()
    }

    async fn setup_controller(emulate: Option<Pubkey>) -> AppState {
        let wallet = if emulate.is_none() {
            create_wallet(Some(get_seed()), None, None)
        } else {
            create_wallet(None, emulate, None)
        };
        let rpc_endpoint = std::env::var("TEST_RPC_ENDPOINT")
            .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());
        AppState::new(&rpc_endpoint, true, wallet, None, None, false, vec![]).await
    }

    // likely safe to ignore during development, mainy regression tests for CI
    #[actix_web::test]
    async fn delegated_signing_ok() {
        let _ = env_logger::try_init();
        let delegated_seed =
            std::env::var("TEST_DELEGATED_SIGNER").expect("delegated signing key set");
        let wallet = create_wallet(
            Some(delegated_seed),
            None,
            Some(
                "GiMXQkJXLVjScmQDkoLJShBJpTh9SDPvT2AZQq8NyEBf"
                    .try_into()
                    .unwrap(),
            ),
        );

        let rpc_endpoint = std::env::var("TEST_RPC_ENDPOINT")
            .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());
        let state = AppState::new(&rpc_endpoint, true, wallet, None, None, false, vec![]).await;

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .service(cancel_orders),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;

        let req = test::TestRequest::default()
            .method(Method::DELETE)
            .uri("/orders")
            .to_request();

        let resp = test::call_service(&app, req).await;
        dbg!(resp.response().body());
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_market_info_works() {
        let controller = setup_controller(None).await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_market_info),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/marketInfo/0")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_orders_works() {
        let controller = setup_controller(None).await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_orders),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/orders")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_positions_works() {
        let controller = setup_controller(None).await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_positions),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/positions")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success(), "{:?}", resp.into_body());
    }

    #[actix_web::test]
    async fn get_markets_works() {
        let controller = setup_controller(None).await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_markets),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/markets")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_sol_balance_works() {
        let controller = setup_controller(None).await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_sol_balance),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/balance")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_tx_events_works() {
        let controller = setup_controller(Some(
            Pubkey::from_str("8kEGX9UNrtKATDjL3ED1dmURzyASsXDe9vGzncMhsTN2").expect("pubkey"),
        ))
        .await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_tx_events),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/transactionEvent/5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5?subAccountId=0")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let body_bytes = test::read_body(resp).await;
        let events: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let expect_body = json!({
            "events": [
                {
                    "fill": {
                        "side": "buy",
                        "fee": "0.129744",
                        "amount": "5",
                        "price": "103.7945822",
                        "oraclePrice": "102.386992",
                        "orderId": 436,
                        "marketIndex": 0,
                        "marketType": "perp",
                        "ts": 1708684880,
                        "signature": "5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5",
                        "maker": null,
                        "makerFee": "0.000000",
                        "makerOrderId": 0,
                        "taker": "5Fky2PjbdFz3PVnfLLbq3caq5iBdwpEvcmrF3iageLJB",
                        "takerFee": "0.129744",
                        "takerOrderId": 436,
                        "txIdx": 6
                    }
                }
            ],
            "success": true,
        });
        assert_eq!(events, expect_body, "incorrect resp body");
    }

    #[actix_web::test]
    async fn get_tx_events_works_for_wrong_subaccount() {
        let controller = setup_controller(Some(
            Pubkey::from_str("8kEGX9UNrtKATDjL3ED1dmURzyASsXDe9vGzncMhsTN2").expect("pubkey"),
        ))
        .await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_tx_events),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/transactionEvent/5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5?subAccountId=1")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let body_bytes = test::read_body(resp).await;
        let events: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let expect_body = json!({
            "events": [],
            "success": true
        });
        assert_eq!(events, expect_body, "incorrect resp body");
    }

    #[actix_web::test]
    async fn get_tx_events_doesnt_exist() {
        let controller = setup_controller(Some(
            Pubkey::from_str("8kEGX9UNrtKATDjL3ED1dmURzyASsXDe9vGzncMhsTN2").expect("pubkey"),
        ))
        .await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_tx_events),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/transactionEvent/4Mi32iRCqo2XXPjnV4bywyBpommVmbm5AN4wqbkgGFwDM3bTz6xjNfaomAnGJNFxicoMjX5x3D1b3DGW9xwkY7ms?subAccountId=1")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_client_error());

        let body_bytes = test::read_body(resp).await;
        let events: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let expect_body = json!({
            "code": 404,
            "reason": "tx not found: 4Mi32iRCqo2XXPjnV4bywyBpommVmbm5AN4wqbkgGFwDM3bTz6xjNfaomAnGJNFxicoMjX5x3D1b3DGW9xwkY7ms"
        });
        assert_eq!(events, expect_body, "incorrect resp body");
    }
}
