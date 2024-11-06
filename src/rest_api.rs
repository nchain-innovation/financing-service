use crate::service::Service;
use actix_web::{delete, get, http::header::ContentType, post, web, HttpResponse, Responder};
use async_mutex::Mutex;
use log::{debug, info};
use serde::Deserialize;

/// Application State Data
pub struct AppState {
    pub service: Mutex<Service>,
}

/// Get Index endpoint
#[get("/")]
pub async fn index(_data: web::Data<AppState>) -> String {
    "Financing Service REST API".to_string()
}

/// Get Service Status endpoint
#[get("/status")]
pub async fn status(data: web::Data<AppState>) -> impl Responder {
    log::info!("status");

    let service = data.service.lock().await;
    let status = service.get_status();
    HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(status)
}

/// Endpoint to update all the clients, called by ticker every minute
pub async fn update_clients(data: web::Data<AppState>) -> impl Responder {
    let mut service = data.service.lock().await;
    service.update_balances().await;
    HttpResponse::Ok()
}

#[derive(Deserialize, Debug)]
pub struct FundingInfo {
    client_id: String,
    satoshi: u64,
    no_of_outpoints: u32,
    mutliple_tx: bool,
    locking_script: String,
}

/// Post Fund endpoint
/// Example:
///     curl -X POST http://127.0.0.1:8080/fund/id1/123/1/false/0000
#[post("/fund/{client_id}/{satoshi}/{no_of_outpoints}/{mutliple_tx}/{locking_script}")]
pub async fn get_funds(data: web::Data<AppState>, info: web::Path<FundingInfo>) -> impl Responder {
    log::info!("get_funds");

    let mut service = data.service.lock().await;

    // These local vars are required as the format! strings don't accept '.` in `{}`
    let client_id = &info.client_id;
    let satoshi = info.satoshi;
    let no_of_outpoints = info.no_of_outpoints;
    let mutliple_tx = info.mutliple_tx;
    let locking_script = &info.locking_script;

    info!("get_funds!");
    // Request funding outpoints
    // Do all input checks here
    if !service.is_client_id_valid(client_id) {
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Unknown client_id {client_id}\"}}"
        );
        return HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response);
    }
    if satoshi == 0 {
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Invalid satoshi value '{satoshi}'\"}}"
        );
        return HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response);
    }

    if no_of_outpoints == 0 {
        let response = format!("{{\"status\": \"Failure\", \"description\": \"Invalid no_of_outpoints value '{no_of_outpoints}'}}");
        return HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response);
    }
    // Check locking_script can be converted to bytes
    let decode_locking_script = hex::decode(locking_script);
    if decode_locking_script.is_err() {
        let response = format!("{{\"status\": \"Failure\", \"description\": \"Unable to convert locking_script to bytes '{locking_script}'\"}}");
        return HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response);
    }

    let locking_script_as_bytes = decode_locking_script.unwrap();
    debug!("locking_script_as_bytes = {:?}", &locking_script_as_bytes);

    let has_sufficent = service.has_sufficent_balance(
        client_id,
        satoshi,
        no_of_outpoints,
        mutliple_tx,
        &locking_script_as_bytes,
    );

    if has_sufficent.is_none() || !has_sufficent.unwrap() {
        let response = "{{\"status\": \"Failure\", \"description\": \"Insufficent client balance to create funding transactions.\"}}".to_string();
        return HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response);
    } else {
        let outpoints = service
            .create_funding_outpoints(
                client_id,
                satoshi,
                no_of_outpoints,
                mutliple_tx,
                &locking_script_as_bytes,
            )
            .await;
        debug!("outpoints = {:?}", &outpoints);
        HttpResponse::Ok().body(outpoints)
    }
}

#[derive(Deserialize, Debug)]
pub struct ClientInfo {
    client_id: String,
    wif: String,
}

/// Add client
/// Example:
///     curl -X POST http://127.0.0.1:8080/client/client_1/
#[post("/client/{client_id}/{wif}")]
pub async fn add_client(data: web::Data<AppState>, info: web::Path<ClientInfo>) -> impl Responder {
    let mut service = data.service.lock().await;
    // These local vars are required as the format! strings don't accept '.` in `{}`
    let client_id = &info.client_id;
    log::info!("add_client {}", &client_id);

    let wif = &info.wif;
    // check to see if client_id already exists
    if service.is_client_id_valid(client_id) {
        //return error we already have this client
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Unknown client_id {client_id}\"}}"
        );
        HttpResponse::UnprocessableEntity()
            .content_type(ContentType::json())
            .body(response)
    } else {
        // if not add it
        service.add_client(client_id, wif);

        let response = "{\"status\": \"Success\"}".to_string();
        HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response)
    }
}

/// Delete client
/// Example:
///     curl -X POST http://127.0.0.1:8080/client/client_1/
#[delete("/client/{client_id}")]
pub async fn delete_client(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let mut service = data.service.lock().await;
    // These local vars are required as the format! strings don't accept '.` in `{}`
    let client_id: String = info.to_string();
    log::info!("delete_client {}", &client_id);

    // check to see if client_id already exists
    if service.is_client_id_valid(&client_id) {
        // if so delete it
        service.delete_client(&client_id);

        let response = "{\"status\": \"Success\"}".to_string();
        HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response)
    } else {
        // return error as we already have this client
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Unknown client_id {client_id} \"}}"
        );
        HttpResponse::UnprocessableEntity()
            .content_type(ContentType::json())
            .body(response)
    }
}

/// Get Address for a particular client_id
#[get("/client/{client_id}/address")]
pub async fn get_address(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let client_id: String = info.to_string();
    let service = data.service.lock().await;

    // Check client_id
    if !service.is_client_id_valid(&client_id) {
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Unknown client_id {client_id} \"}}"
        );
        HttpResponse::UnprocessableEntity()
            .content_type(ContentType::json())
            .body(response)
    } else {
        let address = service.get_address(&client_id).unwrap();
        let response = format!("{{\"status\": \"Success\", \"Balance\": {address} \"}}");
        HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response)
    }
}

/// Get Balance for a particular client_id endpoint
#[get("/client/{client_id}/balance")]
pub async fn balance(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let client_id: String = info.to_string();
    let service = data.service.lock().await;

    // Check client_id
    if !service.is_client_id_valid(&client_id) {
        let response = format!(
            "{{\"status\": \"Failure\", \"description\": \"Unknown client_id {client_id} \"}}"
        );
        HttpResponse::UnprocessableEntity()
            .content_type(ContentType::json())
            .body(response)
    } else {
        let balance = service.get_balance(&client_id).unwrap();
        let response = format!("{{\"status\": \"Success\", \"Balance\": {balance} }}");
        HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(response)
    }
}
