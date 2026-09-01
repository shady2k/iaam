use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{AccountView, Clock, Principal, Scope};
use iaam_app::scenarios::categories::{
    CategoryRuleInput, create_category, create_category_rule, create_group, preview_category_rule,
    retire_category, retire_category_rule, retire_group,
};
use iaam_app::scenarios::reports::{MoneyFlowQuery, money_flow};
use iaam_core::category::{CategoryInterval, CategoryMatcher, CategoryRuleProposal};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CategoryGroupId, CategoryId, CategoryRuleId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, normalize};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> Date {
        date!(2026 - 08 - 31)
    }
}

struct Ctx {
    services: AppServices,
    principal: Principal,
}

fn harness() -> Ctx {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    let mut services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    );
    services.categories = adapter.clone();
    let owner = OwnerId::new_random();
    Ctx {
        services,
        principal: Principal {
            token_id: Uuid::new_v4(),
            owner,
            scope: Scope::Owner,
        },
    }
}

impl Ctx {
    async fn account(&self, title: &str) -> AccountId {
        let id = AccountId::new_random();
        self.services
            .store
            .upsert_account(
                self.principal.owner,
                AccountView {
                    id,
                    title: title.to_owned(),
                    institution: None,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("insert account: {error}"));
        id
    }

    async fn contour(&self, accounts: &[AccountId]) -> ContourId {
        let id = ContourId::new_random();
        self.services
            .store
            .insert_contour_version(
                self.principal.owner,
                ContourDefinition::new(id, ContourVersion(1), accounts.iter().copied()),
                "test contour".to_owned(),
                accounts.to_vec(),
            )
            .await
            .unwrap_or_else(|error| panic!("insert contour: {error}"));
        id
    }

    async fn submit_outflow(
        &self,
        account: AccountId,
        amount: i64,
        on: &str,
        source_category: Option<&str>,
    ) {
        let day = Date::parse(on, &time::format_description::well_known::Iso8601::DATE)
            .unwrap_or_else(|error| panic!("parse date: {error}"));
        let operation = SubmittedOperation {
            account,
            kind: OperationKind::Withdrawal {
                amount_minor: amount,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                trade: Some(day),
                settled: Some(day),
                cash_posted: Some(day),
                paid: None,
            },
            source_time: None,
            idempotency_key: None,
            source_operation_id: None,
            source_category: source_category.map(str::to_owned),
        };
        let event = normalize(
            &operation,
            iaam_ingest::operation::NormalizationContext {
                owner: self.principal.owner,
                source: SourceId::new_random(),
            },
        )
        .unwrap_or_else(|error| panic!("normalize operation: {error:?}"))
        .event;
        self.services
            .store
            .append_events(vec![event], IdentityScope::Source)
            .await
            .unwrap_or_else(|error| panic!("append operation: {error}"));
    }

    async fn create_group(&self, title: &str) -> CategoryGroupId {
        create_group(&self.services, &self.principal, title)
            .await
            .unwrap_or_else(|error| panic!("create group: {error}"))
    }

    async fn retire_group(&self, group: CategoryGroupId) {
        retire_group(&self.services, &self.principal, group)
            .await
            .unwrap_or_else(|error| panic!("retire group: {error}"));
    }

    async fn create_category(&self, group: CategoryGroupId, title: &str) -> CategoryId {
        create_category(&self.services, &self.principal, group, title)
            .await
            .unwrap_or_else(|error| panic!("create category: {error}"))
    }

    async fn create_rule_on_source_category(
        &self,
        source_category: &str,
        category: CategoryId,
        valid_from: Option<Date>,
        valid_to: Option<Date>,
    ) {
        create_category_rule(
            &self.services,
            &self.principal,
            CategoryRuleInput {
                matcher: CategoryMatcher::SourceCategory {
                    value: source_category.to_owned(),
                },
                category,
                interval: CategoryInterval {
                    from: valid_from,
                    to: valid_to,
                },
                replaces: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("create category rule: {error}"));
    }
}

async fn august_card(ctx: &Ctx) -> (AccountId, ContourId) {
    let card = ctx.account("Card").await;
    let contour = ctx.contour(&[card]).await;
    ctx.submit_outflow(card, 30_000, "2026-08-05", Some("Супермаркеты"))
        .await;
    ctx.submit_outflow(card, 12_000, "2026-08-12", None).await;
    (card, contour)
}

#[tokio::test]
async fn the_flow_report_decomposes_by_the_owners_rules() {
    let ctx = harness();
    let (_card, contour) = august_card(&ctx).await;
    let group = ctx.create_group("Usual Expenses").await;
    let food = ctx.create_category(group, "Продукты").await;
    ctx.create_rule_on_source_category("Супермаркеты", food, None, None)
        .await;

    let report = money_flow(
        &ctx.services,
        &ctx.principal,
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        },
    )
    .await
    .expect("report");

    let by_category = report
        .flow
        .went_out_by_category(CurrencyCode::Rub)
        .expect("fits");
    assert_eq!(
        by_category,
        vec![(
            food,
            Money::new(PostedMinor::new(30_000), CurrencyCode::Rub)
        )]
    );
    let (rows, amount) = report.flow.not_decomposed(CurrencyCode::Rub).expect("fits");
    assert_eq!(rows, 1);
    assert_eq!(amount.amount().raw(), 12_000);
    assert_eq!(report.category_rule_versions, vec![1]);
}

#[tokio::test]
async fn a_rule_outside_the_month_does_not_touch_it() {
    let ctx = harness();
    let (_card, contour) = august_card(&ctx).await;
    let group = ctx.create_group("Usual Expenses").await;
    let food = ctx.create_category(group, "Продукты").await;
    ctx.create_rule_on_source_category("Супермаркеты", food, None, Some(date!(2026 - 07 - 31)))
        .await;

    let report = money_flow(
        &ctx.services,
        &ctx.principal,
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        },
    )
    .await
    .expect("report");

    assert!(
        report
            .flow
            .went_out_by_category(CurrencyCode::Rub)
            .expect("fits")
            .is_empty()
    );
    let (rows, amount) = report.flow.not_decomposed(CurrencyCode::Rub).expect("fits");
    assert_eq!(rows, 2);
    assert_eq!(amount.amount().raw(), 42_000);
}

#[tokio::test]
async fn a_category_cannot_be_created_under_a_retired_group() {
    let ctx = harness();
    let group = ctx.create_group("Usual Expenses").await;
    ctx.retire_group(group).await;

    let error = create_category(&ctx.services, &ctx.principal, group, "Продукты")
        .await
        .expect_err("refused");
    assert!(matches!(
        error,
        AppError::Invalid { ref field, .. } if field == "group"
    ));
}

#[tokio::test]
async fn a_preview_reports_what_would_move_and_writes_nothing() {
    let ctx = harness();
    let account = ctx.account("Card").await;
    ctx.submit_outflow(account, 1_000, "2026-07-05", Some("Пекарни"))
        .await;
    ctx.submit_outflow(account, 2_000, "2026-07-12", Some("Пекарни"))
        .await;
    ctx.submit_outflow(account, 3_000, "2026-08-05", Some("Пекарни"))
        .await;

    let group = ctx.create_group("Usual Expenses").await;
    let existing = ctx.create_category(group, "Продукты").await;
    ctx.create_rule_on_source_category("Супермаркеты", existing, None, None)
        .await;
    let proposed_category = ctx.create_category(group, "Кафе").await;
    let proposed = CategoryRuleProposal {
        id: CategoryRuleId::new_random(),
        interval: CategoryInterval {
            from: None,
            to: None,
        },
        matcher: CategoryMatcher::SourceCategory {
            value: "Пекарни".to_owned(),
        },
        category: proposed_category,
    };

    let before = ctx
        .services
        .categories
        .list_category_rules(ctx.principal.owner)
        .await
        .expect("rules")
        .len();
    let impact = preview_category_rule(&ctx.services, &ctx.principal, &proposed)
        .await
        .expect("preview");
    let after = ctx
        .services
        .categories
        .list_category_rules(ctx.principal.owner)
        .await
        .expect("rules")
        .len();

    assert_eq!(before, after, "a preview must not write a rule");
    assert_eq!(impact.rows, 3);
    assert_eq!(impact.months.len(), 2);
    assert_eq!(impact.months[0].month, date!(2026 - 07 - 01));
    assert_eq!(impact.months[1].month, date!(2026 - 08 - 01));
    assert_eq!(impact.months[0].moved.len(), 1);
    assert_eq!(impact.months[0].moved[0].from, None);
    assert_eq!(impact.months[0].moved[0].to, proposed_category);
    assert_eq!(
        impact.months[0].moved[0].amount,
        Money::new(PostedMinor::new(3_000), CurrencyCode::Rub)
    );
    assert_eq!(impact.months[0].moved[0].rows, 2);
    assert_eq!(impact.months[1].moved.len(), 1);
    assert_eq!(impact.months[1].moved[0].from, None);
    assert_eq!(impact.months[1].moved[0].to, proposed_category);
    assert_eq!(
        impact.months[1].moved[0].amount,
        Money::new(PostedMinor::new(3_000), CurrencyCode::Rub)
    );
    assert_eq!(impact.months[1].moved[0].rows, 1);
}

#[tokio::test]
async fn a_preview_with_no_changes_is_empty() {
    let ctx = harness();
    let account = ctx.account("Card").await;
    ctx.submit_outflow(account, 1_000, "2026-08-05", Some("Супермаркеты"))
        .await;
    let group = ctx.create_group("Usual Expenses").await;
    let category = ctx.create_category(group, "Продукты").await;
    ctx.create_rule_on_source_category("Супермаркеты", category, None, None)
        .await;

    let proposed = CategoryRuleProposal {
        id: CategoryRuleId::new_random(),
        interval: CategoryInterval {
            from: None,
            to: None,
        },
        matcher: CategoryMatcher::SourceCategory {
            value: "Супермаркеты".to_owned(),
        },
        category,
    };

    let before = ctx
        .services
        .categories
        .list_category_rules(ctx.principal.owner)
        .await
        .expect("rules")
        .len();
    let impact = preview_category_rule(&ctx.services, &ctx.principal, &proposed)
        .await
        .expect("preview");
    let after = ctx
        .services
        .categories
        .list_category_rules(ctx.principal.owner)
        .await
        .expect("rules")
        .len();

    assert_eq!(before, after);
    assert_eq!(impact.rows, 0);
    assert!(impact.months.is_empty());
}

#[tokio::test]
async fn category_rule_creation_rejects_reversed_intervals_and_accepts_each_matcher() {
    let ctx = harness();
    let group = ctx.create_group("Usual Expenses").await;
    let category = ctx.create_category(group, "Food").await;
    let reversed = create_category_rule(
        &ctx.services,
        &ctx.principal,
        CategoryRuleInput {
            matcher: CategoryMatcher::Row {
                key: "row-1".to_owned(),
            },
            category,
            interval: CategoryInterval {
                from: Some(date!(2026 - 08 - 31)),
                to: Some(date!(2026 - 08 - 01)),
            },
            replaces: None,
        },
    )
    .await
    .expect_err("reversed interval must be rejected");
    assert!(matches!(
        reversed,
        AppError::Invalid { ref field, .. } if field == "interval"
    ));

    for matcher in [
        CategoryMatcher::Row {
            key: "row-1".to_owned(),
        },
        CategoryMatcher::SourceCategory {
            value: "Supermarkets".to_owned(),
        },
        CategoryMatcher::DescriptionContains {
            text: "cafe".to_owned(),
        },
    ] {
        create_category_rule(
            &ctx.services,
            &ctx.principal,
            CategoryRuleInput {
                matcher,
                category,
                interval: CategoryInterval {
                    from: None,
                    to: None,
                },
                replaces: None,
            },
        )
        .await
        .expect("each matcher is serialisable and accepted");
    }

    let rules = ctx
        .services
        .categories
        .list_category_rules(ctx.principal.owner)
        .await
        .expect("rules");
    assert_eq!(rules.len(), 3);
}

#[tokio::test]
async fn category_creation_and_rule_retirement_preserve_actionable_errors() {
    let ctx = harness();
    let missing = create_category(
        &ctx.services,
        &ctx.principal,
        CategoryGroupId::new_random(),
        "Food",
    )
    .await
    .expect_err("missing group must be refused");
    assert!(matches!(
        missing,
        AppError::NotFound {
            what: "category group",
            ..
        }
    ));

    let group = ctx.create_group("Usual Expenses").await;
    let category = ctx.create_category(group, "Food").await;
    let rule = create_category_rule(
        &ctx.services,
        &ctx.principal,
        CategoryRuleInput {
            matcher: CategoryMatcher::Row {
                key: "row-1".to_owned(),
            },
            category,
            interval: CategoryInterval {
                from: None,
                to: None,
            },
            replaces: None,
        },
    )
    .await
    .expect("rule");

    retire_category_rule(&ctx.services, &ctx.principal, rule.id)
        .await
        .expect("rule retires");
    let second = retire_category_rule(&ctx.services, &ctx.principal, rule.id)
        .await
        .expect_err("retired rule cannot be retired twice");
    assert!(matches!(
        second,
        AppError::NotFound {
            what: "active category rule",
            ..
        }
    ));

    let category = create_category(&ctx.services, &ctx.principal, group, "Cafe")
        .await
        .expect("second category");
    retire_category(&ctx.services, &ctx.principal, category)
        .await
        .expect("category retires");
}
