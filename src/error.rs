use actix_web::{http::header::ContentType, HttpResponse};
use serde::Serialize;

#[derive(Serialize)]
pub struct ErrorResponse {
    pub description: String,
}

pub fn error_response(description: impl Into<String>) -> HttpResponse {
    HttpResponse::UnprocessableEntity()
        .content_type(ContentType::json())
        .json(ErrorResponse {
            description: description.into(),
        })
}
