use actix_web::{
    delete, get, patch, post,
    web::{self, Json},
    App, Either, HttpResponse, HttpServer, Responder,
};
use argh::FromArgs;
use log::{error, info};

use controller::{AppState, ControllerError};
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
    if body.len() > 0 {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => {
                return Either::Left(HttpResponse::BadRequest().message_body(err.to_string()))
            }
        }
    };

    match controller.get_orders(req).await {
        Err(err) => {
            error!("{err:?}");
            Either::Left(HttpResponse::InternalServerError().message_body(err.to_string()))
        }
        Ok(payload) => Either::Right(Json(payload)),
    }
}

#[post("/orders")]
async fn create_orders(
    controller: web::Data<AppState>,
    req: Json<PlaceOrdersRequest>,
) -> impl Responder {
    match controller.place_orders(req.0).await {
        Err(err) => {
            // TODO: convert into http status code / return error to client
            error!("{err:?}");
            Either::Left(HttpResponse::InternalServerError())
        }
        Ok(payload) => Either::Right(Json(payload)),
    }
}

#[patch("/orders")]
async fn modify_orders(
    controller: web::Data<AppState>,
    req: Json<ModifyOrdersRequest>,
) -> impl Responder {
    match controller.modify_orders(req.0).await {
        Err(ControllerError::Sdk(err)) => {
            error!("{err:?}");
            Either::Left(HttpResponse::InternalServerError())
        }
        Err(ControllerError::UnknownOrderId) => Either::Left(HttpResponse::NotFound()),
        Ok(payload) => Either::Right(Json(payload)),
    }
}

#[delete("/orders")]
async fn cancel_orders(
    controller: web::Data<AppState>,
    req: Json<CancelOrdersRequest>,
) -> impl Responder {
    match controller.cancel_orders(req.0).await {
        Err(err) => {
            error!("{err:?}");
            Either::Left(HttpResponse::InternalServerError())
        }
        Ok(payload) => Either::Right(Json(payload)),
    }
}

#[get("/positions")]
async fn get_positions(
    controller: web::Data<AppState>,
    body: actix_web::web::Bytes,
) -> impl Responder {
    let mut req = GetPositionsRequest::default();
    if body.len() > 0 {
        match serde_json::from_slice(body.as_ref()) {
            Ok(deser) => req = deser,
            Err(err) => {
                return Either::Left(HttpResponse::BadRequest().message_body(err.to_string()))
            }
        }
    };

    match controller.get_positions(req).await {
        Err(err) => {
            error!("{err:?}");
            Either::Left(HttpResponse::InternalServerError().message_body(err.to_string()))
        }
        Ok(payload) => Either::Right(Json(payload)),
    }
}

#[get("/orderbooks")]
async fn get_orderbooks(
    controller: web::Data<AppState>,
    req: Json<GetOrderbookRequest>,
) -> impl Responder {
    let dlob = controller.stream_orderbook(req.0);
    // there's no graceful shutdown for the stream: https://github.com/actix/actix-web/issues/1313
    HttpResponse::Ok().streaming(dlob)
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
                .service(get_orderbooks),
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
