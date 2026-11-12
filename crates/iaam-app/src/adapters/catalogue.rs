//! One catalogue registration path shared by live broker channels.

use iaam_broker::BrokerInstrumentDescription;
use iaam_core::ids::{InstrumentId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentMetadata};
use time::Date;

use crate::error::AppError;
use crate::ports::{InstrumentDirectory, InstrumentUpsert};

pub(crate) async fn resolve_or_record(
    directory: &dyn InstrumentDirectory,
    description: &BrokerInstrumentDescription,
    source: SourceId,
    on: Date,
) -> Result<InstrumentId, AppError> {
    let isin = description
        .isin
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::Invalid {
            field: "isin".to_owned(),
            expected: "an ISIN in the broker instrument description".to_owned(),
            actual: "absent".to_owned(),
        })?;
    tracing::info!(
        phase = "register_instrument",
        status = "start",
        requested = 1,
        "broker catalogue registration"
    );
    let instrument = directory
        .register_instrument_with_isin(
            InstrumentUpsert {
                id: InstrumentId::new_random(),
                kind: description.kind,
                symbol: description.ticker.clone(),
                title: description.name.clone(),
                currencies: CurrencyRoles::uniform(description.currency),
                lineage: None,
                metadata: Some(InstrumentMetadata {
                    mic: description.mic.clone(),
                    board: description.board.clone(),
                    lot: description.lot,
                    price_decimals: description.price_decimals,
                }),
            },
            isin.to_owned(),
            source,
            on,
        )
        .await?;
    tracing::info!(
        phase = "register_instrument",
        status = "complete",
        instruments = 1,
        "broker catalogue registration"
    );
    Ok(instrument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::instrument::InstrumentKind;
    use iaam_core::money::CurrencyCode;
    use time::macros::date;

    #[tokio::test]
    async fn concurrent_brokers_reuse_one_isin_and_earlier_ranges_reuse_it() {
        let directory = crate::adapters::sqlite::SqliteAdapter::new(
            iaam_store::SqliteStore::open_in_memory().expect("invented store"),
        );
        let description = BrokerInstrumentDescription {
            isin: Some("XX0000FIXT01".to_owned()),
            ticker: "FIXT".to_owned(),
            name: "Invented share".to_owned(),
            kind: Some(InstrumentKind::Share),
            currency: CurrencyCode::Rub,
            mic: Some("MISX".to_owned()),
            board: Some("TQBR".to_owned()),
            lot: Some(10),
            price_decimals: Some(2),
        };
        let (first, second) = tokio::join!(
            resolve_or_record(
                &directory,
                &description,
                SourceId::new_random(),
                date!(2025 - 06 - 30)
            ),
            resolve_or_record(
                &directory,
                &description,
                SourceId::new_random(),
                date!(2025 - 01 - 01)
            ),
        );
        let instrument = first.expect("first broker registers");
        assert_eq!(instrument, second.expect("second broker reuses"));
        let records = directory.list_instruments().await.expect("catalogue");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, instrument);
        assert_eq!(records[0].kind.as_deref(), Some("share"));
        assert_eq!(
            records[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.lot),
            Some(10)
        );
        assert_eq!(directory.list_aliases().await.expect("aliases").len(), 1);
        assert_eq!(
            directory
                .resolve("isin", "XX0000FIXT01", date!(2000 - 01 - 01))
                .await
                .expect("earlier history resolves"),
            instrument
        );
    }
}
