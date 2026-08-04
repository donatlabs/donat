#![forbid(unsafe_code)]

mod canonical;
mod model;
mod source;

pub use canonical::*;
pub use donat_connector_abi::ConnectorErrorClass;
pub use model::*;
pub use source::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogError {
    code: &'static str,
    detail: String,
}

impl CatalogError {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl core::fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for CatalogError {}
