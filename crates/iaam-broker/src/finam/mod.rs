//! Независимый разбор и HTTP-доступ к Finam Trade API.
//!
//! Клиент возвращает сырое тело ответа, а смысл полей и точные доменные
//! величины живут в `parse`; это не даёт транспортному слою стать вторым
//! парсером канала.

mod client;
pub mod parse;

pub use client::{FinamClient, FinamError};
pub use parse::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, FINAM_PARSER_VERSION, ParseError,
    parse_operations, parse_portfolio,
};
