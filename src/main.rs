use std::{
    time::Duration, net::Ipv4Addr,
    env,
};
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
    config::{get_config, Config},
    rest_api::{balance, get_funds, index, status, update_clients, AppState},
    service::Service,
};


// Given the config return the websever ip address and port
fn get_addr(config: &Config) -> (Ipv4Addr, u16) {
    let port = config.web_interface.port;
    match env::var_os("APP_ENV") {
        // Allow all access in docker
        // (required as otherwise the localmachine can not access the webserver)
        Some(content) if content == "docker" => (Ipv4Addr::new(0,0,0,0), port),
        Some(_) | None => (config.web_interface.address, port),
    }
}

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

    let addr = get_addr(&config);

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
