//! Сценарии: собрать срез, позвать ядро, сохранить результат.
//!
//! Арифметики над деньгами здесь нет ни одной строки. Любое число,
//! попадающее в ответ API, приходит из `iaam-core` (§3.1, §13).

pub mod ingest;
pub mod market_reference;
pub mod reports;

pub mod classification;
pub mod documents;
pub mod reconciliation;
