use std::time::Duration;
use tokio::time;

use actix_web::{web, App, HttpServer};
use async_mutex::Mutex;

// mod blockchain_interface;
mod blockchain_factory;
mod client;
mod config;
mod rest_api;
mod service;
mod util;

use crate::{
    config::get_config,
    rest_api::{balance, get_funds, index, status, update_clients, AppState},
    service::Service,
};

/// Main - Read config and setup Web server.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config = match get_config("FS_CONFIG", "data/financing-service.toml") {
        Some(config) => config,
        None => panic!("Unable to read config"),
    };

    simple_logger::init_with_level(config.get_log_level()).unwrap();

    let service = Service::new(&config).await;

    let counter = web::Data::new(AppState {
        service: Mutex::new(service),
    });

    let counter2 = counter.clone();

    let addr = (config.web_interface.address, config.web_interface.port);

    // Setup periodic task
    tokio::spawn(async move {
        // Every minute
        let mut interval = time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            // Refresh the utxo for clients
            update_clients(counter2.clone()).await;
        }
    });

    HttpServer::new(move || {
        App::new()
            .app_data(counter.clone())
            .service(index)
            .service(status)
            .service(balance)
            .service(get_funds)
    })
    .bind(addr)?
    .run()
    .await
}
