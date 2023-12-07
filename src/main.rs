use actix_web::{
    delete, get, patch, post,
    web::{self, Json},
    App, HttpRequest, HttpServer, Responder,
};
use argh::FromArgs;
use controller::AppState;

mod controller;
mod types;

#[get("/markets")]
async fn get_markets(controller: web::Data<AppState>) -> impl Responder {
    let markets = controller.get_markets();
    Json(markets)
}

#[get("/orders")]
async fn get_orders(controller: web::Data<AppState>, req: HttpRequest) -> impl Responder {
    // TODO: add some RequestType to filter
    let orders = controller.get_orders().await;
    Json(orders)
}

#[post("/orders")]
async fn create_orders(controller: web::Data<AppState>, req: HttpRequest) -> impl Responder {
    // "ok" return tx hash
    "unimplemented".to_string()
}

#[delete("/orders")]
async fn cancel_orders(controller: web::Data<AppState>, req: HttpRequest) -> impl Responder {
    let signature = controller.cancel_orders().await;
    Json(signature)
}

#[get("/positions")]
async fn get_positions(controller: web::Data<AppState>, req: HttpRequest) -> impl Responder {
    // TODO: add some RequestType to filter
    let positions = controller.get_positions().await;
    Json(positions)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let secret_key = std::env::var("GATEWAY_SECRET_KEY").expect("missing GATEWAY_SECRET_KEY");
    let config: GatewayConfig = argh::from_env();

    let state = AppState::new(secret_key.as_str(), &config.rpc_host, config.dev).await;

    HttpServer::new(move || {
        App::new().app_data(web::Data::new(state.clone())).service(
            web::scope("/v2")
                .service(get_markets)
                .service(get_positions)
                .service(get_orders)
                .service(create_orders)
                .service(cancel_orders),
        )
    })
    .bind((config.host, config.port))?
    .run()
    .await
}

#[derive(FromArgs)]
/// Reach new heights.
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
