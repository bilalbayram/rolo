use axum::{
    Json,
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error("this API key has read-only scope")]
    Forbidden,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::Invalid(_) => "invalid",
            Error::Conflict(_) => "conflict",
            Error::Unauthorized => "unauthorized",
            Error::Forbidden => "forbidden",
            Error::Internal(_) => "internal",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Forbidden => StatusCode::FORBIDDEN,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &err {
            match db.kind() {
                sqlx::error::ErrorKind::UniqueViolation => {
                    return Error::Conflict(unique_violation_message(db.message()));
                }
                sqlx::error::ErrorKind::ForeignKeyViolation => {
                    return Error::Invalid("a referenced record does not exist".into());
                }
                sqlx::error::ErrorKind::CheckViolation => {
                    return Error::Invalid(format!("constraint failed: {}", db.message()));
                }
                _ => {}
            }
        }
        Error::Internal(err.into())
    }
}

impl From<JsonRejection> for Error {
    fn from(rejection: JsonRejection) -> Self {
        Error::Invalid(rejection.body_text())
    }
}

impl From<QueryRejection> for Error {
    fn from(rejection: QueryRejection) -> Self {
        Error::Invalid(rejection.body_text())
    }
}

impl From<PathRejection> for Error {
    fn from(rejection: PathRejection) -> Self {
        Error::Invalid(rejection.body_text())
    }
}

fn unique_violation_message(message: &str) -> String {
    // SQLite reports expression indexes as "index 'name'" rather than column names.
    if message.contains("contacts_email_key") {
        "a contact with this email already exists".into()
    } else if message.contains("companies_domain_key") {
        "a company with this domain already exists".into()
    } else {
        format!("duplicate value: {message}")
    }
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Machine-readable code: not_found, invalid, conflict, unauthorized, forbidden, internal.
    pub code: &'static str,
    pub message: String,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let message = match &self {
            Error::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                "internal server error".to_string()
            }
            other => other.to_string(),
        };
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code(),
                message,
            },
        };
        (self.status(), Json(body)).into_response()
    }
}
