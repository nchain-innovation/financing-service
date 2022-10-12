use actix_web::{web, App, HttpServer};
// use std::sync::Mutex;
use async_mutex::Mutex;

mod blockchain_interface;
mod client;
mod config;
mod service;

mod rest_api;

use crate::config::get_config;
use crate::rest_api::{balance, get_funds, index, status, AppState};
use crate::service::Service;

/// Main - Read config and setup Web server.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config = match get_config("FS_CONFIG", "data/financing-service.toml") {
        Some(config) => config,
        None => panic!("Unable to read config"),
    };

    let service = Service::new(&config);

    let counter = web::Data::new(AppState {
        service: Mutex::new(service.await),
    });

    let addr = (config.web_interface.address, config.web_interface.port);

    HttpServer::new(move || {
        App::new()
            .app_data(counter.clone())
            //.route("/", web::get().to(index))
            .service(index)
            .service(status)
            .service(balance)
            .service(get_funds)
    })
    .bind(addr)?
    .run()
    .await
}
