//! Saltbox's configuration (config spec): the schema rules Saltbox ships
//! for its config files, loaded strictly, for `sb validate-config`.

pub mod cloudflare;
pub mod dockerhub;
pub mod facts;
pub mod http;
pub mod issue;
pub mod migrate;
pub mod parse;
pub mod rules;
pub mod run;
pub mod scalar;
pub mod schema;
pub mod secret;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tree;
pub mod user;
pub mod validators;
