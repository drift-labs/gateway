use actix_web::{
    delete, get, patch, post,
    web::{self, Json},
    App, Either, HttpResponse, HttpServer, Responder,
};
use argh::FromArgs;
use log::{error, info};

use controller::{AppState, ControllerError};
use serde_json::json;
use types::{
    CancelOrdersRequest, GetOrderbookRequest, GetOrdersRequest, GetPositionsRequest,
    ModifyOrdersRequest, PlaceOrdersRequest,
};

mod controller;
mod types;

#[get("/markets")]
async fn get_markets(controller: web::Data<AppState>) -> impl Responder {
    let markets = controller.get_markets();
    Json(markets)
}

#[get("/orders")]
async fn get_orders(
    controller: web::Data<AppState>,
    body: actix_web::web::Bytes,
) -> impl Responder {
    let mut req = GetOrdersRequest::default();
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => {
                return Either::Left(HttpResponse::BadRequest().json(json!(
                    {
                        "code": 400,
                        "reason": err.to_string(),
                    }
                )))
            }
        }
    };

    handle_result(controller.get_orders(req).await)
}

#[post("/orders")]
async fn create_orders(
    controller: web::Data<AppState>,
    req: Json<PlaceOrdersRequest>,
) -> impl Responder {
    handle_result(controller.place_orders(req.0).await)
}

#[patch("/orders")]
async fn modify_orders(
    controller: web::Data<AppState>,
    req: Json<ModifyOrdersRequest>,
) -> impl Responder {
    handle_result(controller.modify_orders(req.0).await)
}

#[delete("/orders")]
async fn cancel_orders(
    controller: web::Data<AppState>,
    body: actix_web::web::Bytes,
) -> impl Responder {
    let mut req = CancelOrdersRequest::default();
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => {
                return Either::Left(HttpResponse::BadRequest().json(json!(
                    {
                        "code": 400,
                        "reason": err.to_string(),
                    }
                )))
            }
        }
    };
    handle_result(controller.cancel_orders(req).await)
}

#[get("/positions")]
async fn get_positions(
    controller: web::Data<AppState>,
    body: actix_web::web::Bytes,
) -> impl Responder {
    let mut req = GetPositionsRequest::default();
    // handle the body manually to allow empty payload `Json` requires some body is set
    if !body.is_empty() {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => {
                return Either::Left(HttpResponse::BadRequest().json(json!(
                    {
                        "code": 400,
                        "reason": err.to_string(),
                    }
                )))
            }
        }
    };

    handle_result(controller.get_positions(req).await)
}

#[get("/orderbook")]
async fn get_orderbook(
    controller: web::Data<AppState>,
    req: Json<GetOrderbookRequest>,
) -> impl Responder {
    let book = controller.get_orderbook(req.0).await;
    handle_result(book)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config: GatewayConfig = argh::from_env();
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    let secret_key = std::env::var("DRIFT_GATEWAY_KEY").expect("missing DRIFT_GATEWAY_KEY");
    let state = AppState::new(secret_key.as_str(), &config.rpc_host, config.dev).await;

    info!(
        "🏛️ gateway listening at http://{}:{}",
        config.host, config.port
    );
    info!(
        "🪪: authority: {:?}, user: {:?}",
        state.authority(),
        state.user()
    );

    HttpServer::new(move || {
        App::new().app_data(web::Data::new(state.clone())).service(
            web::scope("/v2")
                .service(get_markets)
                .service(get_positions)
                .service(get_orders)
                .service(create_orders)
                .service(cancel_orders)
                .service(modify_orders)
                .service(get_orderbook),
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
