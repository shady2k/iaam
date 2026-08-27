//! Правила классификации и пересчёт истории (§10.4).
//!
//! **Классификация не является полем события.** Событие несёт факт —
//! `CashTransfer` с обоими счетами, — а «внутри контура или снаружи»
//! выводит классификатор контура (§4.10). Правила нужны там, где из
//! данных не выводится сам **тип** операции: перевод себе против
//! перевода третьему лицу, комиссия против вывода, доход против
//! возврата средств.
//!
//! Классифицируется поэтому не построенная операция, а признаки строки,
//! видимые **до** выбора типа: счёт-контрагент, назначение платежа
//! и слово, которым источник назвал операцию.
//!
//! **Пересчёт истории — новые факты, а не правка старых.** Правка
//! правила даёт план из сторнирования и замены; журнал остаётся
//! append-only (§4.8). Тип [`Correction`] не умеет выражать изменение
//! события — это гарантия формы, а не дисциплины вызывающего.

use std::collections::BTreeMap;

use iaam_core::event::Event;
use iaam_core::event::correction::{CorrectionError, resolve};
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId};

/// Куда движутся деньги. Нужно, чтобы вопрос владельцу был по делу:
/// у списания и поступления разные развилки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Movement {
    In,
    Out,
}

/// Кто на другой стороне.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counterparty {
    /// Счёт владельца, узнанный справочником.
    OwnAccount(AccountId),
    /// Счёт назван, но не узнан: строка реквизитов из отчёта.
    Named(String),
    /// Сторона не названа вовсе.
    Unknown,
}

/// Признаки строки, по которым решается тип операции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationSubject {
    pub account: AccountId,
    pub counterparty: Counterparty,
    pub description: Option<String>,
    /// Как операцию назвал источник. Открытое множество: у каждого
    /// брокера свои слова, поэтому строка, а не перечисление.
    pub source_kind: Option<String>,
    pub movement: Movement,
}

/// Условие правила. Заданные поля соединяются «и».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatcher {
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    pub kind: Option<String>,
}

impl RuleMatcher {
    /// Условие, не спрашивающее ни о чём, не подходит ни к чему.
    ///
    /// Правило «на всё» заводится только по ошибке, а молча
    /// переклассифицировать им весь портфель нельзя.
    #[must_use]
    pub const fn asks_nothing(&self) -> bool {
        self.counterparty_account.is_none()
            && self.description_contains.is_none()
            && self.kind.is_none()
    }

    /// Подходит ли условие к строке.
    #[must_use]
    pub fn matches(&self, subject: &ClassificationSubject) -> bool {
        if self.asks_nothing() {
            return false;
        }
        let by_counterparty = self.counterparty_account.as_deref().is_none_or(
            |wanted| matches!(&subject.counterparty, Counterparty::Named(name) if name == wanted),
        );
        // Назначение платежа брокеры пишут как придётся: правило,
        // чувствительное к регистру, перестало бы работать на следующем
        // отчёте того же брокера.
        let by_description = self.description_contains.as_deref().is_none_or(|wanted| {
            subject
                .description
                .as_deref()
                .is_some_and(|text| text.to_lowercase().contains(&wanted.to_lowercase()))
        });
        let by_kind = self
            .kind
            .as_deref()
            .is_none_or(|wanted| subject.source_kind.as_deref() == Some(wanted));
        by_counterparty && by_description && by_kind
    }
}

/// Чем операция оказалась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    InternalTransfer { to: AccountId },
    ExternalFlow,
    Fee { origin: FeeOrigin },
    Income,
}

/// Решение владельца, записанное правилом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRule {
    pub id: ClassificationRuleId,
    pub version: u32,
    pub matcher: RuleMatcher,
    pub outcome: Classification,
}

impl ClassificationRule {
    /// Формулировка правила словами.
    ///
    /// Правило обязано быть видимым: без формулировки объяснить
    /// прошлую классификацию нечем (§10.4).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut conditions = Vec::new();
        if let Some(account) = &self.matcher.counterparty_account {
            conditions.push(format!("счёт контрагента — {account}"));
        }
        if let Some(text) = &self.matcher.description_contains {
            conditions.push(format!("в назначении есть «{text}»"));
        }
        if let Some(kind) = &self.matcher.kind {
            conditions.push(format!("источник назвал операцию «{kind}»"));
        }
        let conditions = if conditions.is_empty() {
            "условий нет, поэтому правило не применяется".to_owned()
        } else {
            conditions.join(" и ")
        };
        format!(
            "версия {}: если {conditions}, то {}",
            self.version,
            describe_outcome(self.outcome)
        )
    }
}

fn describe_outcome(outcome: Classification) -> &'static str {
    match outcome {
        Classification::InternalTransfer { .. } => "это перевод между своими счетами",
        Classification::ExternalFlow => "это движение за пределы портфеля",
        Classification::Fee { .. } => "это комиссия",
        Classification::Income => "это доход",
    }
}

/// Почему операция классифицирована именно так.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// Выведено из данных: правило не понадобилось.
    Derived,
    /// Решение владельца.
    Rule {
        rule: ClassificationRuleId,
        version: u32,
    },
}

/// Вопрос владельцу.
///
/// Перечисление, а не строка: вопрос уходит в API и рендерится
/// с человеческими именами счетов, которых у чистой функции нет,
/// а строка с UUID внутри не является конкретным вопросом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Счёт получателя назван, но не узнан.
    IsTransferInternal {
        account: AccountId,
        counterparty: String,
    },
    /// Списание без названной стороны: комиссия или вывод?
    IsOutflowAFee { account: AccountId },
    /// Поступление без названной стороны: доход или возврат средств?
    IsInflowIncome { account: AccountId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassificationResult {
    Resolved {
        classification: Classification,
        basis: Basis,
    },
    /// Из данных не выводится и правилом не покрыто. Догадка запрещена.
    Ambiguous { question: Question },
}

/// Классификация строки.
///
/// Из нескольких подошедших правил выигрывает старшая версия: правка
/// заводит новую версию, и старшая — последнее решение владельца.
#[must_use]
pub fn classify(
    subject: &ClassificationSubject,
    rules: &[ClassificationRule],
) -> ClassificationResult {
    if let Counterparty::OwnAccount(to) = subject.counterparty {
        return ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to },
            basis: Basis::Derived,
        };
    }
    let chosen = rules
        .iter()
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    match chosen {
        Some(rule) => ClassificationResult::Resolved {
            classification: rule.outcome,
            basis: Basis::Rule {
                rule: rule.id,
                version: rule.version,
            },
        },
        None => ClassificationResult::Ambiguous {
            question: question_for(subject),
        },
    }
}

fn question_for(subject: &ClassificationSubject) -> Question {
    match (&subject.counterparty, subject.movement) {
        (Counterparty::Named(counterparty), _) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: counterparty.clone(),
        },
        (Counterparty::Unknown, Movement::Out) => Question::IsOutflowAFee {
            account: subject.account,
        },
        (Counterparty::Unknown, Movement::In) => Question::IsInflowIncome {
            account: subject.account,
        },
        // Собственный счёт до сюда не доходит: он разобран в `classify`
        // как выводимый из данных.
        (Counterparty::OwnAccount(to), _) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: to.inner().to_string(),
        },
    }
}

/// Какую классификацию событие уже выражает.
///
/// `None` — событие классификации не несёт: сделка, оценка,
/// восстановленное начало и контрольное утверждение являются фактами,
/// а не решениями владельца, и пересчёту не подлежат.
///
/// Исчерпывающий `match`: новый вид события обязан сломать сборку
/// здесь, а не молча выпасть из пересчёта.
#[must_use]
pub const fn classification_of(event: &Event) -> Option<Classification> {
    match event.kind {
        EventKind::CashTransfer { to, .. } => Some(Classification::InternalTransfer { to }),
        EventKind::CashIn { .. } | EventKind::CashOut { .. } => Some(Classification::ExternalFlow),
        EventKind::Fee { origin, .. } => Some(Classification::Fee { origin }),
        EventKind::Income { .. } => Some(Classification::Income),
        EventKind::Trade { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        // Корпоративное действие и оферта — факты, а не решения
        // владельца, и пересчёту правилами не подлежат. `Income` здесь
        // была бы правдоподобной и молчаливой ошибкой: амортизация —
        // возврат собственного капитала (§6.5), и отнести её к доходу
        // значит завысить доход на весь возвращённый номинал.
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => None,
    }
}

/// Одно исправление плана пересчёта.
///
/// Раскрывается ровно в два шага — сторно и замену. Варианта «изменить
/// событие» в типе нет: журнал append-only обеспечен формой, а не
/// обещанием вызывающего (§4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub target: EventId,
    pub was: Classification,
    pub becomes: Classification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionStep {
    Reverse {
        target: EventId,
    },
    Replace {
        target: EventId,
        classification: Classification,
    },
}

impl Correction {
    #[must_use]
    pub const fn steps(&self) -> [CorrectionStep; 2] {
        [
            CorrectionStep::Reverse {
                target: self.target,
            },
            CorrectionStep::Replace {
                target: self.target,
                classification: self.becomes,
            },
        ]
    }
}

/// План пересчёта истории после правки правил.
///
/// Строится по **действующему** набору событий: сторнированные
/// и заменённые в него не входят. Отсюда идемпотентность — после
/// применения плана действующим становится замещающее событие, чья
/// классификация уже совпадает с правилом, и повторный запуск даёт
/// пустой план сам собой, без отдельной проверки «уже делали».
///
/// Строка, не покрытая правилом, остаётся как есть: пересчёт не
/// догадывается там, где не догадывается приёмка.
pub fn recompute_plan(
    events: &[Event],
    subjects: &BTreeMap<EventId, ClassificationSubject>,
    rules: &[ClassificationRule],
) -> Result<Vec<Correction>, CorrectionError> {
    let mut plan = Vec::new();
    for event in resolve(events)? {
        let (Some(subject), Some(was)) = (subjects.get(&event.id), classification_of(event)) else {
            continue;
        };
        let ClassificationResult::Resolved {
            classification: becomes,
            ..
        } = classify(subject, rules)
        else {
            continue;
        };
        if becomes != was {
            plan.push(Correction {
                target: event.id,
                was,
                becomes,
            });
        }
    }
    Ok(plan)
}
