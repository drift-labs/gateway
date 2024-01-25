use actix_web::{
    delete, get, patch, post,
    web::{self, Json},
    App, Either, HttpResponse, HttpServer, Responder,
};
use argh::FromArgs;
use log::{error, info};

use controller::{create_wallet, AppState, ControllerError};
use drift_sdk::Pubkey;
use serde_json::json;
use std::{borrow::Borrow, str::FromStr, sync::Arc};
use types::{
    CancelAndPlaceRequest, CancelOrdersRequest, GetOrderbookRequest, ModifyOrdersRequest,
    PlaceOrdersRequest,
};

mod controller;
mod types;
mod websocket;

#[derive(serde::Deserialize)]
struct Args {
    #[serde(default, rename = "subAccountId")]
    sub_account_id: u16,
}

#[get("/markets")]
async fn get_markets(controller: web::Data<AppState>) -> impl Responder {
    let markets = controller.get_markets();
    Json(markets)
}

#[get("/orders")]
async fn get_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    let mut req = None;
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = Some(deser),
            Err(err) => return handle_deser_error(err),
        }
    };

    handle_result(controller.get_orders(req, args.sub_account_id).await)
}

#[post("/orders")]
async fn create_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    match serde_json::from_slice::<'_, PlaceOrdersRequest>(body.as_ref()) {
        Ok(req) => handle_result(controller.place_orders(req, args.sub_account_id).await),
        Err(err) => handle_deser_error(err),
    }
}

#[patch("/orders")]
async fn modify_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    match serde_json::from_slice::<'_, ModifyOrdersRequest>(body.as_ref()) {
        Ok(req) => handle_result(controller.modify_orders(req, args.sub_account_id).await),
        Err(err) => handle_deser_error(err),
    }
}

#[delete("/orders")]
async fn cancel_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    let mut req = CancelOrdersRequest::default();
    // handle the body manually to allow empty payload `Json` requires some body is set
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => return handle_deser_error(err),
        }
    };
    handle_result(controller.cancel_orders(req, args.sub_account_id).await)
}

#[post("/orders/cancelAndPlace")]
async fn cancel_and_place_orders(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    match serde_json::from_slice::<'_, CancelAndPlaceRequest>(body.as_ref()) {
        Ok(req) => handle_result(
            controller
                .cancel_and_place_orders(req, args.sub_account_id)
                .await,
        ),
        Err(err) => handle_deser_error(err),
    }
}

#[get("/positions")]
async fn get_positions(
    controller: web::Data<AppState>,
    body: web::Bytes,
    args: web::Query<Args>,
) -> impl Responder {
    let mut req = None;
    // handle the body manually to allow empty payload `Json` requires some body is set
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = Some(deser),
            Err(err) => return handle_deser_error(err),
        }
    };

    handle_result(controller.get_positions(req, args.sub_account_id).await)
}

#[get("/orderbook")]
async fn get_orderbook(controller: web::Data<AppState>, body: web::Bytes) -> impl Responder {
    match serde_json::from_slice::<'_, GetOrderbookRequest>(body.as_ref()) {
        Ok(req) => handle_result(controller.get_orderbook(req).await),
        Err(err) => handle_deser_error(err),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config: GatewayConfig = argh::from_env();
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    let secret_key = std::env::var("DRIFT_GATEWAY_KEY");
    let delegate = config
        .delegate
        .map(|ref x| Pubkey::from_str(x).expect("valid pubkey"));
    let emulate = config
        .emulate
        .map(|ref x| Pubkey::from_str(x).expect("valid pubkey"));
    let wallet = create_wallet(secret_key.ok(), emulate, delegate);
    let state = AppState::new(&config.rpc_host, config.dev, wallet).await;

    info!(
        "🏛️ gateway listening at http://{}:{}",
        config.host, config.port
    );

    if delegate.is_some() {
        info!(
            "🪪: authority: {:?}, default sub-account: {:?}, 🔑 delegate: {:?}",
            state.authority(),
            state.default_sub_account(),
            state.signer(),
        );
    } else {
        info!(
            "🪪: authority: {:?}, default sub-account: {:?}",
            state.authority(),
            state.default_sub_account()
        );
    }

    let client = Box::leak(Box::new(Arc::clone(state.client.borrow())));
    websocket::start_ws_server(
        format!("{}:1337", &config.host).as_str(),
        config.rpc_host.replace("http", "ws"),
        state.wallet.clone(),
        client.program_data(),
    )
    .await;

    HttpServer::new(move || {
        App::new().app_data(web::Data::new(state.clone())).service(
            web::scope("/v2")
                .service(get_markets)
                .service(get_positions)
                .service(get_orders)
                .service(create_orders)
                .service(cancel_orders)
                .service(modify_orders)
                .service(get_orderbook)
                .service(cancel_and_place_orders),
        )
    })
    .bind((config.host, config.port))?
    .run()
    .await
}

#[derive(FromArgs)]
/// Drift gateway server
struct GatewayConfig {
    /// the solana RPC URL
    #[argh(positional)]
    rpc_host: String,
    /// run in devnet mode
    #[argh(switch)]
    dev: bool,
    /// gateway host address
    #[argh(option, default = "String::from(\"127.0.0.1\")")]
    host: String,
    /// gateway port
    #[argh(option, default = "8080")]
    port: u16,
    /// use delegated signing mode, provide the delegator's pubkey
    #[argh(option)]
    delegate: Option<String>,
    /// run the gateway in read-only mode for given authority pubkey
    #[argh(option)]
    emulate: Option<String>,
}

fn handle_result<T>(result: Result<T, ControllerError>) -> Either<HttpResponse, Json<T>> {
    match result {
        Ok(payload) => Either::Right(Json(payload)),
        Err(ControllerError::Sdk(err)) => {
            error!("{err:?}");
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
        Err(ControllerError::UnknownOrderId(id)) => {
            Either::Left(HttpResponse::NotFound().json(json!(
                {
                    "code": 404,
                    "reason": format!("order: {id}"),
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

#[cfg(test)]
mod tests {
    use actix_web::{http::Method, test, App};

    use crate::types::Market;

    use self::controller::create_wallet;

    use super::*;

    const TEST_ENDPOINT: &str = "https://api.devnet.solana.com";

    fn get_seed() -> String {
        std::env::var("DRIFT_GATEWAY_KEY")
            .expect("DRIFT_GATEWAY_KEY is set")
            .to_string()
    }

    async fn setup_controller() -> AppState {
        let wallet = create_wallet(Some(get_seed()), None, None);
        AppState::new(TEST_ENDPOINT, true, wallet).await
    }

    #[actix_web::test]
    async fn get_orders_works() {
        let controller = setup_controller().await;
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
        let controller = setup_controller().await;
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
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_orderbook_works() {
        let controller = setup_controller().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(controller))
                .service(get_orderbook),
        )
        .await;
        let req = test::TestRequest::default()
            .method(Method::GET)
            .uri("/orderbook")
            .set_json(GetOrderbookRequest {
                market: Market::perp(0), // sol-perp
            })
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn get_markets_works() {
        let controller = setup_controller().await;
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
}
