//! Вид операции, каким его назвал канал брокера.
//!
//! Тип один на все каналы намеренно. Раньше `ChannelOperationKind` был
//! объявлен дважды — у T-Invest и у Finam, — и два перечисления одного
//! смысла расходятся молча: член, добавленный одному каналу, второму
//! не мешает собираться, и разница обнаруживается не сборкой, а тем,
//! что операция одного брокера превращается не в то, во что превратилась
//! бы операция другого.
//!
//! **Это словарь смыслов, а не кодов брокера.** Коды у каждого канала
//! свои, их множество открыто и меняется без нашего участия, поэтому
//! соответствие «код источника → член этого перечисления» живёт
//! в данных, а не в `match` (эпик iaam-d8b.2.2). Здесь перечислено
//! то, что система умеет сделать с операцией, и новый член обязан
//! сломать сборку везде, где разбор не полон (§15.1).

/// Что канал сообщил об операции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOperationKind {
    /// Покупка инструмента.
    Buy,
    /// Продажа инструмента.
    Sell,
    /// Дивидендная выплата.
    Dividend,
    /// Купонная выплата.
    Coupon,
    /// Комиссия брокера или сервиса.
    Commission,
    /// Пополнение счёта.
    Deposit,
    /// Вывод денег или бумаг.
    Withdrawal,
    /// Перевод между счетами или депозитариями.
    Transfer,
    /// Амортизация облигации: непогашенный номинал уменьшается, деньги
    /// приходят, количество бумаг не меняется (§6.5).
    ///
    /// Отдельный член, а не доход: доход не уменьшает номинал, и учтённая
    /// доходом амортизация завысила бы и доход, и стоимость позиции.
    BondAmortisation,
    /// Окончательное погашение облигации: номинал возвращён целиком
    /// и бумага выбывает.
    BondRedemption,
    /// Вид, которого нет в словаре канала.
    ///
    /// Строка, а не отказ разбора: имя вида нужно назвать владельцу,
    /// иначе отказ не говорит, чего именно система не знает.
    Other(String),
}

impl ChannelOperationKind {
    /// Имя вида в словаре и в схеме.
    ///
    /// У `Other` имени нет намеренно: «вида не знаем» выражается
    /// отсутствием строки в словаре, а не строкой с именем «прочее».
    /// Записанное «прочее» означало бы решение не разбирать, а такого
    /// решения не принимали.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::Buy => Some("buy"),
            Self::Sell => Some("sell"),
            Self::Dividend => Some("dividend"),
            Self::Coupon => Some("coupon"),
            Self::Commission => Some("commission"),
            Self::Deposit => Some("deposit"),
            Self::Withdrawal => Some("withdrawal"),
            Self::Transfer => Some("transfer"),
            Self::BondAmortisation => Some("bond_amortisation"),
            Self::BondRedemption => Some("bond_redemption"),
            Self::Other(_) => None,
        }
    }

    /// Разбор имени из словаря.
    ///
    /// Неизвестное имя даёт `None`, а не `Other`: `Other` означает
    /// «канал прислал код, которого нет в словаре», а здесь случилось
    /// другое — в словаре лежит вид, которого не знает эта сборка.
    /// Свести их в одно значило бы спрятать рассинхронизацию схемы
    /// и кода за обычным неизвестным кодом брокера.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "buy" => Some(Self::Buy),
            "sell" => Some(Self::Sell),
            "dividend" => Some(Self::Dividend),
            "coupon" => Some(Self::Coupon),
            "commission" => Some(Self::Commission),
            "deposit" => Some(Self::Deposit),
            "withdrawal" => Some(Self::Withdrawal),
            "transfer" => Some(Self::Transfer),
            "bond_amortisation" => Some(Self::BondAmortisation),
            "bond_redemption" => Some(Self::BondRedemption),
            _ => None,
        }
    }
}

/// Словарь одного канала: как его коды превращаются в виды операций.
///
/// Строится из данных хранилища и передаётся в разбор параметром.
/// Крейта брокера про хранилище не знает намеренно (см. `lib.rs`),
/// поэтому связывает их адаптер приложения — тем же приёмом, что уже
/// сделан для SQLite.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationKindDictionary {
    entries: std::collections::BTreeMap<String, ChannelOperationKind>,
}

/// Строка словаря, которую эта сборка прочитать не смогла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableEntry {
    pub source_kind: String,
    pub kind: String,
}

impl OperationKindDictionary {
    /// Собирает словарь из пар «код канала → имя вида».
    ///
    /// Непрочитанные строки возвращаются рядом со словарём, а не
    /// отбрасываются: строка, которую сборка не понимает, означает, что
    /// база новее кода, и молчание об этом превратило бы известный код
    /// брокера в неизвестный — то есть в отказ импорта без объяснения.
    #[must_use]
    pub fn build<I, K, V>(rows: I) -> (Self, Vec<UnreadableEntry>)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: AsRef<str>,
    {
        let mut entries = std::collections::BTreeMap::new();
        let mut unreadable = Vec::new();
        for (source_kind, kind) in rows {
            let source_kind = source_kind.into();
            match ChannelOperationKind::parse(kind.as_ref()) {
                Some(parsed) => {
                    entries.insert(source_kind, parsed);
                }
                None => unreadable.push(UnreadableEntry {
                    source_kind,
                    kind: kind.as_ref().to_owned(),
                }),
            }
        }
        (Self { entries }, unreadable)
    }

    /// Во что канал превратил свой код.
    ///
    /// Кода нет в словаре — это `Other` с самим кодом внутри: отказ
    /// обязан назвать, чего именно система не знает.
    #[must_use]
    pub fn kind_of(&self, source_kind: &str) -> ChannelOperationKind {
        self.entries
            .get(source_kind)
            .cloned()
            .unwrap_or_else(|| ChannelOperationKind::Other(source_kind.to_owned()))
    }

    /// Сколько кодов знает словарь.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Пуст ли словарь.
    ///
    /// Пустой словарь — не «брокер прислал непонятное», а «словарь
    /// не заведён»: различать их обязан вызывающий, иначе владелец
    /// получит отказ про брокера вместо отказа про настройку.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Начальный словарь канала по коду брокера.
///
/// Возвращает `None` для брокера, о котором знания нет: пустая таблица
/// означала бы «у этого канала видов операций не бывает», а это другое
/// утверждение, и заведение доступа приняло бы его молча.
#[must_use]
pub fn seed_for(broker: &str) -> Option<(&'static str, &'static [(&'static str, &'static str)])> {
    match broker {
        "tinkoff" => Some((
            crate::tinkoff::dictionary_seed::TINKOFF_SEED_NAME,
            crate::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS,
        )),
        "finam" => Some((
            crate::finam::dictionary_seed::FINAM_SEED_NAME,
            crate::finam::dictionary_seed::FINAM_OPERATION_KINDS,
        )),
        _ => None,
    }
}
