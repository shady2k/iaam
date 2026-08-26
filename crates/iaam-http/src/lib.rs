//! Исходящий HTTP: транспорт, доверие, устойчивость.
//!
//! Единственная крейта в дереве, объявляющая HTTP-клиент. Крейты
//! источников (`iaam-broker`, `iaam-market`) описывают запрос и разбирают
//! ответ; ни та, ни другая операция сети не касается, и потому обе
//! проверяются на замороженных образцах.
//!
//! Правило проверяется заслоном: `scripts/check-architecture.sh`
//! запрещает `reqwest` во всех крейтах, кроме этой.

pub mod client;
pub mod destination;
pub mod request;
pub mod resilience;
pub mod response;
pub mod trust;

pub use destination::Destination;
pub use request::{HttpMethod, HttpRequest, RequestBody, Secret};
pub use response::{HttpError, HttpResponse};
