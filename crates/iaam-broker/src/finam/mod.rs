//! Independent parsing and HTTP access for the Finam Trade API.
//!
//! The client returns raw response bodies, while field meanings and exact
//! domain values live in `parse`; this keeps the transport layer from becoming
//! a second channel parser.

mod client;
pub mod dictionary_seed;
pub mod parse;

pub use client::{FinamClient, FinamError};
pub use parse::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, FINAM_PARSER_VERSION, ParseError,
    parse_operations, parse_portfolio,
};
