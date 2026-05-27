use std::{env, net::Ipv4Addr, time::Duration};
use tokio::time;

use actix_web::{web, App, HttpServer};
use tokio::sync::RwLock;

mod auth;
mod blockchain_factory;
mod client;
mod config;
mod dynamic_config;
mod responses;
mod rest_api;
mod service;
mod util;

#[cfg(test)]
mod test_support;

use crate::{
    config::{get_config, Config},
    rest_api::{
        add_client, balance, delete_client, get_address, get_funds, health, index, status,
        update_clients, AppState,
    },
    service::Service,
};

// Given the config return the websever ip address and port
fn get_addr(config: &Config) -> (Ipv4Addr, u16) {
    let port = config.web_interface.port;
    match env::var_os("APP_ENV") {
        // Allow all access in docker
        // (required as otherwise the localmachine can not access the webserver)
        Some(content) if content == "docker" => (Ipv4Addr::new(0, 0, 0, 0), port),
        Some(_) | None => (config.web_interface.address, port),
    }
}

/// Main - Read config and setup Web server.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let config = get_config("FS_CONFIG", "data/financing-service.toml").map_err(|e| {
        eprintln!("Unable to read config: {e}");
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;

    let log_level = config.get_log_level().map_err(|e| {
        eprintln!("Invalid logging configuration: {e}");
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;

    simple_logger::init_with_level(log_level).map_err(|e| {
        eprintln!("Unable to initialize logger: {e}");
        std::io::Error::other(e.to_string())
    })?;

    let service = Service::new(&config).await.map_err(|e| {
        log::error!("Unable to start service: {e}");
        std::io::Error::other(e)
    })?;
    if service.admin_auth_required() {
        log::info!("Admin API key authentication enabled for POST /client");
    } else {
        log::warn!(
            "Admin API key not configured; POST /client is unauthenticated. Set web_interface.admin_api_key or restrict via network isolation."
        );
    }
    let clients_without_api_key = service.clients_without_api_key();
    if service.client_count() > 0 && clients_without_api_key.is_empty() {
        log::info!("Per-client API key authentication enabled for all clients");
    } else if !clients_without_api_key.is_empty() {
        log::warn!(
            "Clients without api_key configured: {}. Protect these via network isolation or set api_key per client.",
            clients_without_api_key.join(", ")
        );
    }
    let app_state = web::Data::new(AppState {
        service: RwLock::new(service),
    });
    let app_state2 = app_state.clone();
    let addr = get_addr(&config);

    // Setup periodic task
    let utxo_refresh_period = config.service.utxo_refresh_period;
    tokio::spawn(async move {
        // Every minute
        let mut interval = time::interval(Duration::from_secs(utxo_refresh_period));
        loop {
            interval.tick().await;
            // Refresh the utxo for clients
            update_clients(app_state2.clone()).await;
        }
    });

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .service(index)
            .service(health)
            .service(status)
            .service(balance)
            .service(get_funds)
            .service(add_client)
            .service(delete_client)
            .service(get_address)
    })
    .bind(addr)
    .map_err(|e| {
        log::error!(r#"Unable to bind to address/port "{addr:?}": {e}"#);
        e
    })?
    .run()
    .await
}
